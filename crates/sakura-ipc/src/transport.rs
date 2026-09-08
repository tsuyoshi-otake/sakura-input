//! The named pipe itself: one instance, and the framing on top of it.
//!
//! The engine treats this boundary as hostile (DESIGN 7). Every length is
//! range-checked before a byte is read on its word, every decode failure is
//! answered and then the connection is dropped rather than resynchronized,
//! and nothing here can panic on input — a malformed frame from a
//! compromised host process must cost that host its IME and nothing else.
//!
//! # Byte mode, not message mode
//!
//! Windows offers a message-mode pipe that would do the framing for us. We
//! use byte mode anyway, because the frame layout is already ours (DESIGN 7:
//! a `u32` length followed by the payload) and it has to be, since the same
//! frames have to survive a transport that is not a Windows pipe — the
//! protocol crate's tests and fuzzers run on plain byte slices. One framing
//! implementation, exercised everywhere, beats two that can disagree.
//!
//! # One waiting worker per connection
//!
//! Each worker waits for its own overlapped I/O to complete; there is no
//! completion port or concurrent transfer on one instance. Overlapped handles
//! let a peer inspect disconnection without blocking behind that worker's
//! pending read. This matters when an abandoned composing worker is still
//! computing and its replacement must stop treating it as a live duplicate.

use sakura_proto::{payload_len, FRAME_HEADER_LEN, MAX_PAYLOAD};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ARITHMETIC_OVERFLOW, ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE,
    ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PeekNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::security::{Descriptor, ServerRejection};

/// How many instances of the pipe may exist at once, which is the ceiling
/// on simultaneously connected host processes.
///
/// One connection is one application with the IME active, so this is sized
/// for "every window a person has open", not for a server's client count.
/// A connection costs one thread and a few kilobytes of buffers; the cap
/// exists so a runaway client cannot make the engine spawn threads without
/// bound.
pub const MAX_INSTANCES: u32 = 64;

/// Kernel buffer hint for each direction, in bytes.
///
/// Frames are a few hundred bytes at most — a preedit is capped at
/// `MAX_PREEDIT_BYTES` — so this only has to be large enough that a
/// keystroke's request and its reply never round-trip through a partial
/// write. It is a hint: the kernel grows past it when it has to.
const PIPE_BUFFER_BYTES: u32 = 8 * 1024;

/// How an accept ended.
///
/// Separating "no client on the other end" from a genuine failure is what
/// keeps a routine client race from costing an acceptor: the server's whole
/// worker used to end on any error [`PipeInstance::wait_for_client`]
/// returned, so a host process that connected and exited immediately took a
/// pipe instance with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accept {
    /// A client is on the other end and the connection can be served.
    Connected,
    /// A client reached the instance and was gone before it could be
    /// served. The instance is still healthy; it has to be disconnected
    /// before it can accept again.
    ClientGone,
}

/// Why an operation on a connection stopped.
#[derive(Debug)]
pub enum Fault {
    /// The client closed the pipe or its process exited. Routine: a host
    /// application closing is the ordinary end of a connection, not an
    /// error to report anywhere.
    Disconnected,
    /// The client sent something the protocol forbids. The connection is
    /// not recoverable — a byte stream that has lost frame alignment
    /// cannot be resynchronized — so the server answers if it still can
    /// and then drops it.
    Protocol(sakura_proto::Error),
    /// This side failed to encode an outgoing frame — a request or a
    /// reply — before a single byte of it reached the peer.
    ///
    /// Distinct from [`Protocol`](Fault::Protocol): the peer never sent
    /// anything malformed, and never misbehaved. What the connection does
    /// next still depends on which side failed, so this variant alone does
    /// not decide it. A client whose own request could not be encoded has
    /// told the peer nothing yet, so only that request is rejected and the
    /// link stays usable (e.g. a reconversion selection too large to fit
    /// the wire format). A server that cannot encode its reply has already
    /// dispatched the client's request and left it waiting for an answer
    /// it can never receive on this connection — the caller ends the
    /// connection rather than leave that wait unresolved.
    Encode(sakura_proto::Error),
    /// The reply did not arrive inside the caller's deadline. Only the
    /// client end can produce this: the DLL must never block a keystroke
    /// for longer than 50 ms (DESIGN 4.3), and a slow engine is answered
    /// by passing the key through, not by waiting.
    ///
    /// This variant preserves the frame boundary: the connection survives
    /// because the request may still be in
    /// flight — which is why the protocol puts a monotonic id on every
    /// frame. A late reply is matched by id and discarded, never applied
    /// to whatever the user typed next. Partial-frame timeout is instead
    /// Desynchronized and requires dropping the connection.
    Timeout,
    /// An absolute request deadline expired before writing any request byte.
    /// The peer was not contacted and its session did not change.
    DeadlineExpired,
    /// A reply arrived carrying a request id that was never sent, or an
    /// interrupted partial transfer lost the stream's frame boundary.
    ///
    /// Distinct from [`Protocol`](Fault::Protocol): the bytes decoded
    /// fine, they just do not answer anything this client asked. Nothing
    /// on the connection can be trusted after that, and unlike a late
    /// reply — which is expected and dropped — this one cannot be
    /// explained away.
    Desynchronized,
    /// The kernel-reported peer on a verified connection did not satisfy the
    /// caller's image/integrity policy. The handle is dropped before Hello,
    /// so no untrusted process receives protocol traffic.
    ///
    /// `rejection` names which step refused. It is carried rather than
    /// discarded because the refusal is often the only thing observable — in
    /// CI there is no debugger, and "rejected" alone has now cost four
    /// investigations (Issue #104).
    UntrustedServer {
        process_id: u32,
        rejection: ServerRejection,
    },
    /// The operating system refused the operation.
    Os(windows::core::Error),
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Fault::Disconnected => write!(f, "client disconnected"),
            Fault::Protocol(error) => write!(f, "protocol violation: {error:?}"),
            Fault::Encode(error) => write!(f, "failed to encode outgoing request: {error:?}"),
            Fault::Timeout => write!(f, "the engine did not answer in time"),
            Fault::DeadlineExpired => write!(f, "request deadline expired before send"),
            Fault::Desynchronized => write!(f, "reply identity or stream framing desynchronized"),
            Fault::UntrustedServer {
                process_id,
                rejection,
            } => {
                write!(f, "untrusted server process {process_id}: {rejection}")
            }
            Fault::Os(error) => write!(f, "{error}"),
        }
    }
}

impl From<windows::core::Error> for Fault {
    fn from(error: windows::core::Error) -> Self {
        Fault::Os(error)
    }
}

/// One instance of the server pipe.
///
/// The handle lives as long as the instance: after a client disconnects the
/// same handle is reused for the next one, which is what
/// `DisconnectNamedPipe` followed by another `ConnectNamedPipe` is for.
/// Recreating the pipe between clients would open a window in which the
/// name does not exist and a squatter could claim it.
#[derive(Debug)]
pub struct PipeInstance {
    handle: HANDLE,
    event: HANDLE,
    connection: Arc<Mutex<ConnectionState>>,
}

struct ConnectionState {
    handle: Option<HANDLE>,
    generation: u64,
    connected: bool,
    probe_event: HANDLE,
    probe_io: Box<OVERLAPPED>,
    probe_pending: bool,
}

impl core::fmt::Debug for ConnectionState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConnectionState")
            .field("generation", &self.generation)
            .field("connected", &self.connected)
            .field("probe_pending", &self.probe_pending)
            .finish_non_exhaustive()
    }
}

// SAFETY: the handle is a cross-thread kernel object. Its only observer uses
// metadata/zero-byte I/O on an OVERLAPPED handle while holding this state's
// mutex. The instance joins any probe I/O and invalidates the state under that
// same mutex before closing it. The boxed OVERLAPPED never moves while pending.
unsafe impl Send for ConnectionState {}

/// Read-only liveness of one accepted connection, never a later client that
/// reuses the same pipe instance. Unknown/busy observations retain the claim.
#[derive(Debug, Clone)]
pub struct ConnectionProbe {
    state: Arc<Mutex<ConnectionState>>,
    generation: u64,
}

impl ConnectionProbe {
    /// True only after positive evidence that this exact connection is gone.
    /// No bytes are read and no retry, background thread, or blocking lock is
    /// introduced. A PID query cannot substitute for this: Windows continues
    /// returning the old client PID after its handle has closed.
    pub fn is_disconnected(&self) -> bool {
        let Ok(mut state) = self.state.try_lock() else {
            return false;
        };
        if !state.connected || state.generation != self.generation {
            return true;
        }
        let Some(handle) = state.handle else {
            return true;
        };
        let mut transferred = 0;
        if state.probe_pending {
            // SAFETY: the boxed operation is still alive; this is a poll,
            // never a wait on another worker's response or computation.
            let result =
                unsafe { GetOverlappedResult(handle, &*state.probe_io, &mut transferred, false) };
            if result
                .as_ref()
                .is_err_and(|error| is(error, ERROR_IO_INCOMPLETE))
            {
                return false;
            }
            state.probe_pending = false;
            if result.is_err_and(|error| is_disconnect(&error)) {
                state.connected = false;
                return true;
            }
        }
        let mut available = 0;
        // SAFETY: the state lock prevents close while this call uses the
        // OVERLAPPED handle. Peek does not consume pending request bytes.
        match unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut available), None) } {
            Err(error) if is_disconnect(&error) => {
                state.connected = false;
                true
            }
            Err(_) => false,
            Ok(()) if available == 0 => false,
            Ok(()) => {
                // Peek still succeeds on a closed client's unread queue. A
                // byte-mode null write checks the outbound connection without
                // sending a frame or consuming its inbound bytes. If Windows
                // queues it, retain exactly one stable operation and report
                // unknown until a later poll; never wait on the key path.
                *state.probe_io = OVERLAPPED {
                    hEvent: state.probe_event,
                    ..Default::default()
                };
                // SAFETY: no data buffer is supplied; probe_io is boxed and
                // retained until completion or joined cancellation at teardown.
                match unsafe { WriteFile(handle, Some(&[]), None, Some(&mut *state.probe_io)) } {
                    Err(error) if is(&error, ERROR_IO_PENDING) => {
                        state.probe_pending = true;
                        false
                    }
                    Err(error) if is_disconnect(&error) => {
                        state.connected = false;
                        true
                    }
                    _ => false,
                }
            }
        }
    }
}

impl ConnectionState {
    fn retire(&mut self) {
        self.connected = false;
        if self.probe_pending {
            if let Some(handle) = self.handle {
                let mut transferred = 0;
                // SAFETY: cancellation alone is not completion. Joining here
                // prevents freeing an OVERLAPPED still owned by the kernel.
                unsafe {
                    let _ = CancelIoEx(handle, Some(&*self.probe_io));
                    let _ = GetOverlappedResult(handle, &*self.probe_io, &mut transferred, true);
                }
            }
            self.probe_pending = false;
        }
    }
}

// SAFETY: a pipe handle is a kernel object usable from any thread, and
// `PipeInstance` hands out no interior references to it. Each instance is
// owned by exactly one server thread, which is what makes `Send` enough
// (`Sync` is deliberately not claimed).
unsafe impl Send for PipeInstance {}

impl PipeInstance {
    /// Creates one instance of the pipe.
    ///
    /// `first` must be true for exactly one instance, and that one must be
    /// created before any other. `FILE_FLAG_FIRST_PIPE_INSTANCE` makes the
    /// call fail if the name is already taken, which is how the engine
    /// finds out that something else — a stale copy of itself, or a
    /// squatter trying to collect our clients' keystrokes — already owns
    /// the name. Without it, `CreateNamedPipeW` would happily add an
    /// instance to somebody else's pipe.
    pub fn create(name: &str, security: &Descriptor, first: bool) -> windows::core::Result<Self> {
        Self::create_with_capacity(name, security, first, MAX_INSTANCES)
    }

    /// Creates one instance with an endpoint-specific admission cap.
    ///
    /// The data endpoint keeps [`MAX_INSTANCES`] for host applications, while
    /// renderer/control endpoints use smaller independent caps. Keeping the
    /// cap in the kernel object means a stalled data-plane client cannot
    /// consume the renderer or control admission budget.
    pub fn create_with_capacity(
        name: &str,
        security: &Descriptor,
        first: bool,
        max_instances: u32,
    ) -> windows::core::Result<Self> {
        assert!(max_instances > 0, "a pipe must admit at least one instance");
        let wide = to_wide_nul(name);
        let mut flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let attributes = security.attributes();
        // SAFETY: `wide` is NUL-terminated and outlives the call, and
        // `attributes` borrows `security`, which the caller keeps alive
        // across it. The returned handle is owned by this struct and closed
        // in `Drop`.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                flags,
                // `PIPE_REJECT_REMOTE_CLIENTS` is the local-only check
                // DESIGN 7 requires. The DACL cannot express it: an SMB
                // client authenticates as the same user and would pass
                // every ACE.
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                max_instances,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                Some(&attributes),
            )
        };
        // `CreateNamedPipeW` signals failure with `INVALID_HANDLE_VALUE`
        // rather than a `Result`, so the last-error value has to be picked
        // up explicitly — and it must be read before anything else runs.
        if handle == INVALID_HANDLE_VALUE {
            return Err(windows::core::Error::from_thread());
        }
        // SAFETY: no name/security pointer is retained. The manual-reset
        // event is reused only after the preceding operation has completed.
        let event = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: the newly created pipe has not escaped.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(error);
            }
        };
        // SAFETY: separate manual-reset event for the sole optional probe.
        let probe_event = match unsafe { CreateEventW(None, true, false, None) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: neither newly created handle has escaped.
                unsafe {
                    let _ = CloseHandle(event);
                    let _ = CloseHandle(handle);
                }
                return Err(error);
            }
        };
        Ok(PipeInstance {
            handle,
            event,
            connection: Arc::new(Mutex::new(ConnectionState {
                handle: Some(handle),
                generation: 0,
                connected: false,
                probe_event,
                probe_io: Box::new(OVERLAPPED {
                    hEvent: probe_event,
                    ..Default::default()
                }),
                probe_pending: false,
            })),
        })
    }

    /// Blocks until a client connects.
    ///
    /// `ERROR_PIPE_CONNECTED` is success, not failure: it means a client
    /// won the race between `CreateNamedPipeW` and this call and is
    /// already on the other end. Treating it as an error is the classic way
    /// to drop every connection that arrives too quickly.
    ///
    /// `ERROR_NO_DATA` is the other end of that same race and is not a
    /// failure either: a client connected and closed its handle before this
    /// call ran, so the instance is holding a connection that no longer has
    /// anyone on it. Windows' remedy is to disconnect and accept again,
    /// which is why it is reported as [`Accept::ClientGone`] rather than as
    /// an error — a host process that exits the instant it connects must
    /// cost nothing more than one wasted accept.
    pub fn wait_for_client(&self) -> windows::core::Result<Accept> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        // SAFETY: `handle` is a live pipe instance owned by this struct.
        // The local OVERLAPPED stays alive through the completion wait.
        let started = unsafe { ConnectNamedPipe(self.handle, Some(&mut overlapped)) };
        let connected = match started {
            Ok(()) => Ok(Accept::Connected),
            Err(error) if is(&error, ERROR_PIPE_CONNECTED) => Ok(Accept::Connected),
            Err(error) if is(&error, ERROR_IO_PENDING) => {
                let mut transferred = 0;
                // SAFETY: this is the operation just issued; waiting joins
                // it before the stack OVERLAPPED can be released.
                match unsafe {
                    GetOverlappedResult(self.handle, &overlapped, &mut transferred, true)
                } {
                    Ok(()) => Ok(Accept::Connected),
                    Err(error) if is(&error, ERROR_NO_DATA) => Ok(Accept::ClientGone),
                    Err(error) => Err(error),
                }
            }
            Err(error) if is(&error, ERROR_NO_DATA) => Ok(Accept::ClientGone),
            Err(error) => Err(error),
        }?;
        if matches!(connected, Accept::Connected) {
            let mut state = self
                .connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.generation = state.generation.checked_add(1).ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(
                    ERROR_ARITHMETIC_OVERFLOW.0,
                ))
            })?;
            state.connected = true;
        }
        Ok(connected)
    }

    pub fn connection_probe(&self) -> Option<ConnectionProbe> {
        let state = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.connected.then(|| ConnectionProbe {
            state: Arc::clone(&self.connection),
            generation: state.generation,
        })
    }

    /// Releases the current client so the instance can accept another.
    ///
    /// Flushes first: `DisconnectNamedPipe` discards whatever the client
    /// has not yet read, and the last thing written is usually the reply
    /// the client is waiting for.
    pub fn disconnect(&self) {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retire();
        // SAFETY: `handle` is a live pipe instance owned by this struct.
        // Both calls fail harmlessly when no client is connected, which is
        // why their results are discarded.
        unsafe {
            let _ = FlushFileBuffers(self.handle);
            let _ = DisconnectNamedPipe(self.handle);
        }
    }

    /// Returns the PID attached to this exact server-side connection.
    ///
    /// The engine uses this before the first handshake for a bounded
    /// per-process admission quota. It is a kernel query on the accepted
    /// handle, not a client-supplied protocol field.
    pub fn client_process_id(&self) -> windows::core::Result<u32> {
        let mut process_id = 0;
        // SAFETY: this is a live server-side pipe handle and the output is
        // valid for the duration of the call.
        unsafe { GetNamedPipeClientProcessId(self.handle, &mut process_id)? };
        Ok(process_id)
    }

    /// Fills `buf` completely, looping over partial reads.
    ///
    /// A byte-mode pipe may return fewer bytes than asked for, so a single
    /// `ReadFile` is not a read of a known length.
    fn read_exact(&self, buf: &mut [u8]) -> Result<(), Fault> {
        let mut filled = 0;
        while filled < buf.len() {
            let rest = &mut buf[filled..];
            match self.transfer(Transfer::Read(rest))? {
                0 => return Err(Fault::Disconnected),
                read => filled += read as usize,
            }
        }
        Ok(())
    }

    /// Writes every byte of `buf`, looping over partial writes.
    pub fn write_all(&self, buf: &[u8]) -> Result<(), Fault> {
        let mut written = 0;
        while written < buf.len() {
            let rest = &buf[written..];
            match self.transfer(Transfer::Write(rest))? {
                0 => return Err(Fault::Disconnected),
                count => written += count as usize,
            }
        }
        Ok(())
    }

    fn transfer(&self, operation: Transfer<'_>) -> Result<u32, Fault> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        let mut transferred = 0;
        // SAFETY: this instance has one owning worker and is not Sync. Both
        // the buffer and OVERLAPPED outlive the joined pending operation.
        let started = unsafe {
            match operation {
                Transfer::Read(buffer) => ReadFile(
                    self.handle,
                    Some(buffer),
                    Some(&mut transferred),
                    Some(&mut overlapped),
                ),
                Transfer::Write(buffer) => WriteFile(
                    self.handle,
                    Some(buffer),
                    Some(&mut transferred),
                    Some(&mut overlapped),
                ),
            }
        };
        let completed = match started {
            Err(error) if is(&error, ERROR_IO_PENDING) => {
                #[cfg(test)]
                tests::AFTER_TRANSFER_PENDING.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().take() {
                        hook();
                    }
                });
                // SAFETY: no path returns while the kernel owns the buffer.
                unsafe { GetOverlappedResult(self.handle, &overlapped, &mut transferred, true) }
            }
            other => other,
        };
        completed.map(|()| transferred).map_err(|error| {
            if is_disconnect(&error) {
                Fault::Disconnected
            } else {
                Fault::Os(error)
            }
        })
    }

    /// Reads one frame's payload into `buf`, replacing its contents.
    ///
    /// `buf` keeps its capacity between calls, so a connection at steady
    /// state does not allocate: the first few frames grow it to whatever
    /// that client actually sends and it is reused from then on.
    pub fn read_frame<'a>(&self, buf: &'a mut Vec<u8>) -> Result<&'a [u8], Fault> {
        let mut header = [0u8; FRAME_HEADER_LEN];
        self.read_exact(&mut header)?;
        // Checked before a single byte of the body is read: the length is
        // the one field a hostile client fully controls, and `payload_len`
        // rejects anything above `MAX_PAYLOAD` rather than letting it size
        // an allocation.
        let len = payload_len(&header).map_err(Fault::Protocol)?;
        debug_assert!(len <= MAX_PAYLOAD);
        buf.clear();
        buf.resize(len, 0);
        self.read_exact(buf)?;
        Ok(buf.as_slice())
    }

    /// Reads one frame while bounding the time spent waiting for its first
    /// bytes and body. The regular connection path remains blocking; only the
    /// initial handshake uses this polling form, because a client can connect
    /// successfully and then never send a frame.
    pub fn read_frame_with_deadline<'a>(
        &self,
        buf: &'a mut Vec<u8>,
        budget: Duration,
    ) -> Result<&'a [u8], Fault> {
        let deadline = Instant::now() + budget;
        let mut header = [0u8; FRAME_HEADER_LEN];
        self.wait_for_bytes(FRAME_HEADER_LEN as u32, deadline)?;
        self.read_exact(&mut header)?;
        let len = payload_len(&header).map_err(Fault::Protocol)?;
        debug_assert!(len <= MAX_PAYLOAD);
        buf.clear();
        buf.resize(len, 0);
        self.wait_for_bytes(len as u32, deadline)?;
        self.read_exact(buf)?;
        Ok(buf.as_slice())
    }

    fn wait_for_bytes(&self, needed: u32, deadline: Instant) -> Result<(), Fault> {
        while self.available_bytes()? < needed {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Fault::Timeout);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
        Ok(())
    }

    fn available_bytes(&self) -> Result<u32, Fault> {
        let mut total = 0u32;
        // SAFETY: this is a live byte-mode named-pipe handle; the remaining
        // output pointer is valid for the duration of the call and no data is
        // copied because the buffer arguments are null/zero.
        unsafe {
            PeekNamedPipe(self.handle, None, 0, None, Some(&mut total), None).map_err(|error| {
                if is_disconnect(&error) {
                    Fault::Disconnected
                } else {
                    Fault::Os(error)
                }
            })?;
        }
        Ok(total)
    }
}

impl Drop for PipeInstance {
    fn drop(&mut self) {
        {
            let mut state = self
                .connection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.retire();
            state.handle = None;
            // SAFETY: retire joined the sole probe operation. Later observers
            // see connected=false and cannot access this event or the pipe.
            unsafe {
                let _ = CloseHandle(state.probe_event);
            }
        }
        if !self.handle.is_invalid() {
            // SAFETY: the handle came from `CreateNamedPipeW` and is closed
            // exactly once, here.
            unsafe {
                let _ = CloseHandle(self.handle);
                let _ = CloseHandle(self.event);
            }
        }
    }
}

enum Transfer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

/// True when the error means the other end is gone rather than that
/// something went wrong.
fn is_disconnect(error: &windows::core::Error) -> bool {
    is(error, ERROR_BROKEN_PIPE) || is(error, ERROR_PIPE_NOT_CONNECTED) || is(error, ERROR_NO_DATA)
}

fn is(error: &windows::core::Error, code: WIN32_ERROR) -> bool {
    error.code() == windows::core::HRESULT::from_win32(code.0)
}

/// UTF-16 with a trailing NUL, for the pointer-only Win32 APIs above.
pub(crate) fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

#[cfg(test)]
mod tests {
    thread_local! {
        pub(super) static AFTER_TRANSFER_PENDING: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }
    use super::*;
    use sakura_proto::{encode_request, Request};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };

    /// Opens the pipe the way a client must: with the exact mask the server
    /// grants, never `GENERIC_READ | GENERIC_WRITE`.
    fn connect(name: &str) -> windows::core::Result<HANDLE> {
        let wide = to_wide_nul(name);
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                crate::security::CLIENT_ACCESS,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
    }

    /// A pipe name nobody else is using, derived from this process so two
    /// test binaries running at once cannot collide.
    fn scratch_name(tag: &str) -> String {
        let pid = std::process::id();
        format!(r"\\.\pipe\sakura_input_test_{tag}_{pid}")
    }

    #[test]
    fn connection_probe_does_not_wait_behind_a_pending_read() {
        let name = scratch_name("probe-pending-read");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("pipe");
        let client = connect(&name).expect("client");
        server.wait_for_client().expect("accept");
        let probe = server.connection_probe().expect("probe");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            AFTER_TRANSFER_PENDING.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    ready_tx.send(()).expect("ready receiver");
                }))
            });
            let mut buffer = Vec::new();
            matches!(server.read_frame(&mut buffer), Err(Fault::Disconnected))
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(2));
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            observed_tx
                .send(probe.is_disconnected())
                .expect("observer receiver");
        });
        let observed = observed_rx.recv_timeout(Duration::from_millis(300));
        // SAFETY: closing the owned client ends the pending server read even
        // when the nonblocking observation assertion is going to fail.
        unsafe {
            CloseHandle(client).expect("close client");
        }
        let read_ended = reader.join().expect("reader joined");
        observer.join().expect("observer joined");
        assert!(ready.is_ok() && read_ended);
        assert_eq!(
            observed,
            Ok(false),
            "liveness observation waited for the other worker's read"
        );
    }

    #[test]
    fn connection_probe_preserves_queued_bytes_and_a_waiting_client_response() {
        let name = scratch_name("probe-null-write");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("pipe");
        let mut expected = Vec::new();
        sakura_proto::encode_response(&sakura_proto::Response::Pong, 17, &mut expected)
            .expect("encode reply");
        let reply_len = expected.len();
        let client_thread = std::thread::spawn(move || {
            let client = connect(&name).expect("client");
            let mut request = Vec::new();
            encode_request(&Request::Ping, 17, &mut request).expect("encode request");
            request.push(0xAA); // Another queued request has begun, but is not consumed here.
            let mut written = 0;
            let mut received = vec![0; reply_len];
            let mut filled = 0;
            // SAFETY: this synchronous test handle is owned on this thread;
            // buffers live through each call, and close runs before assertions.
            unsafe {
                let sent = WriteFile(client, Some(&request), Some(&mut written), None);
                if sent.is_ok() {
                    while filled < received.len() {
                        let mut count = 0;
                        if ReadFile(
                            client,
                            Some(&mut received[filled..]),
                            Some(&mut count),
                            None,
                        )
                        .is_err()
                            || count == 0
                        {
                            break;
                        }
                        filled += count as usize;
                    }
                }
                let _ = CloseHandle(client);
            }
            received.truncate(filled);
            received
        });
        server.wait_for_client().expect("accept");
        let probe = server.connection_probe().expect("probe");
        let mut buffer = Vec::new();
        server.read_frame(&mut buffer).expect("first request");
        let queued_before = server.available_bytes();
        let mut stayed_live = true;
        for _ in 0..8 {
            stayed_live &= !probe.is_disconnected();
        }
        let queued_after = server.available_bytes();
        let sent = server.write_all(&expected);
        drop(server);
        let received = client_thread.join().expect("client joined");
        assert!(stayed_live && sent.is_ok());
        assert!(matches!(queued_before, Ok(1)) && matches!(queued_after, Ok(1)));
        assert_eq!(
            received, expected,
            "a zero-byte probe disturbed the framed response"
        );
    }

    #[test]
    fn connection_probe_detects_closed_client_with_unread_queued_bytes() {
        let name = scratch_name("probe-buffered-close");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("pipe");
        let client = connect(&name).expect("client");
        server.wait_for_client().expect("accept");
        let probe = server.connection_probe().expect("accepted probe");
        let live = !probe.is_disconnected();
        let mut written = 0;
        // SAFETY: this synchronous test client owns its handle, and the byte
        // and output count stay live through the completed WriteFile.
        unsafe {
            WriteFile(client, Some(&[1]), Some(&mut written), None).expect("buffered byte");
            CloseHandle(client).expect("close client");
        }
        let queued = server
            .available_bytes()
            .expect("closing pipe still has buffered bytes");
        let gone = probe.is_disconnected();
        drop(server);
        assert!(live);
        assert_eq!(queued, 1);
        assert!(
            gone,
            "unread abandoned requests are not a live composing peer"
        );
        assert!(
            probe.is_disconnected(),
            "observer must be safe after pipe drop"
        );
    }

    #[test]
    fn connection_probe_never_follows_a_reused_pipe_instance() {
        let name = scratch_name("probe-generation");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("pipe");
        let first = connect(&name).expect("first client");
        server.wait_for_client().expect("first accept");
        let old = server.connection_probe().expect("old probe");
        server.disconnect();
        // SAFETY: the test closes each successful client handle exactly once.
        unsafe {
            CloseHandle(first).expect("close first");
        }
        let accepting = std::thread::spawn(move || {
            server.wait_for_client().expect("second accept");
            server
        });
        let second =
            crate::Client::connect_to(&name, Duration::from_secs(2)).expect("second client");
        let server = accepting.join().expect("accept ended");
        let current = server.connection_probe().expect("current probe");
        let stale = old.is_disconnected();
        let live = !current.is_disconnected();
        drop(second);
        drop(server);
        assert!(stale && live);
        assert!(old.is_disconnected() && current.is_disconnected());
    }

    #[test]
    fn a_frame_survives_the_round_trip() {
        let name = scratch_name("roundtrip");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");

        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let handle = connect(&client_name).expect("connect");
            let mut frame = Vec::new();
            encode_request(&Request::Ping, 7, &mut frame).expect("encode");
            let mut written = 0u32;
            // SAFETY: `frame` outlives the call and `written` is a valid
            // out-parameter.
            unsafe {
                WriteFile(handle, Some(&frame), Some(&mut written), None).expect("write");
                let _ = CloseHandle(handle);
            }
        });

        server.wait_for_client().expect("accept");
        let mut buf = Vec::new();
        let payload = server.read_frame(&mut buf).expect("read");
        let (id, request) = sakura_proto::decode_request(payload).expect("decode");
        assert_eq!(id, 7);
        assert_eq!(request, Request::Ping);
        client.join().expect("client thread");
    }

    #[test]
    fn a_client_that_vanishes_reads_as_a_disconnect_not_an_error() {
        let name = scratch_name("vanish");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");

        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let handle = connect(&client_name).expect("connect");
            // SAFETY: `handle` came from `CreateFileW` and is closed once.
            unsafe {
                let _ = CloseHandle(handle);
            }
        });

        server.wait_for_client().expect("accept");
        let mut buf = Vec::new();
        match server.read_frame(&mut buf) {
            Err(Fault::Disconnected) => {}
            other => panic!("expected a disconnect, got {other:?}"),
        }
        client.join().expect("client thread");
    }

    /// A host process that connects and exits before the server reaches
    /// the accept leaves the instance holding a connection to nobody.
    /// Windows answers the accept with `ERROR_NO_DATA`, and reporting that
    /// as an error used to end the engine's acceptor — and leak its
    /// instance slot — over an ordinary client race.
    #[test]
    fn a_client_gone_before_the_accept_is_reported_rather_than_failing() {
        let name = scratch_name("gone_before_accept");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");

        // SAFETY: the handle came from `CreateFileW` and is closed once,
        // before the server has accepted anything on the instance.
        unsafe {
            let handle = connect(&name).expect("connect");
            let _ = CloseHandle(handle);
        }

        match server.wait_for_client() {
            Ok(Accept::ClientGone) => {}
            other => panic!("expected a departed client, got {other:?}"),
        }

        // And the instance is still usable, which is the whole point: the
        // race costs one accept, not the acceptor.
        server.disconnect();
        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let handle = connect(&client_name).expect("connect");
            let mut frame = Vec::new();
            encode_request(&Request::Ping, 11, &mut frame).expect("encode");
            let mut written = 0u32;
            // SAFETY: `frame` outlives the call and `written` is a valid
            // out-parameter.
            unsafe {
                WriteFile(handle, Some(&frame), Some(&mut written), None).expect("write");
                let _ = CloseHandle(handle);
            }
        });

        assert_eq!(
            server.wait_for_client().expect("accept"),
            Accept::Connected,
            "the instance stopped accepting after a departed client"
        );
        let mut buf = Vec::new();
        let payload = server.read_frame(&mut buf).expect("read");
        let (id, request) = sakura_proto::decode_request(payload).expect("decode");
        assert_eq!(id, 11);
        assert_eq!(request, Request::Ping);
        client.join().expect("client thread");
    }

    /// The length prefix is the one field a hostile client fully controls.
    #[test]
    fn an_oversized_length_prefix_is_rejected_before_the_body_is_read() {
        let name = scratch_name("oversized");
        let security = Descriptor::for_pipe().expect("descriptor");
        let server = PipeInstance::create(&name, &security, true).expect("create");

        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let handle = connect(&client_name).expect("connect");
            // Claims a payload far past the cap, and sends none of it. A
            // server that trusted the length would block forever, or size
            // an allocation from it.
            let header = u32::MAX.to_le_bytes();
            let mut written = 0u32;
            // SAFETY: `header` outlives the call; `written` is valid.
            unsafe {
                let _ = WriteFile(handle, Some(&header), Some(&mut written), None);
                std::thread::park_timeout(std::time::Duration::from_millis(200));
                let _ = CloseHandle(handle);
            }
        });

        server.wait_for_client().expect("accept");
        let mut buf = Vec::new();
        match server.read_frame(&mut buf) {
            Err(Fault::Protocol(sakura_proto::Error::TooLarge)) => {}
            other => panic!("expected TooLarge, got {other:?}"),
        }
        assert!(
            buf.capacity() < MAX_PAYLOAD,
            "the claim sized an allocation"
        );
        client.join().expect("client thread");
    }

    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` is what stops the engine from
    /// silently sharing its name with whatever already owns it.
    #[test]
    fn a_name_that_is_already_taken_is_refused_rather_than_shared() {
        let name = scratch_name("taken");
        let security = Descriptor::for_pipe().expect("descriptor");
        let _first = PipeInstance::create(&name, &security, true).expect("create");
        assert!(PipeInstance::create(&name, &security, true).is_err());
        // A further instance of our *own* pipe is still fine — that is how
        // the server accepts more than one client at a time.
        PipeInstance::create(&name, &security, false).expect("additional instance");
    }
}
