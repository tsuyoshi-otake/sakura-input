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
//! # Blocking, one thread per connection
//!
//! No overlapped I/O and no completion port. A connection spends its life
//! blocked in `ReadFile` waiting for the next keystroke, which is exactly
//! what a blocking read is for, and the alternative would trade a large
//! amount of `unsafe` for a scalability limit we will never reach: the
//! number of connections is the number of host applications a person has
//! open, not a server's client count.

use sakura_proto::{payload_len, FRAME_HEADER_LEN, MAX_PAYLOAD};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    HANDLE, INVALID_HANDLE_VALUE, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

use crate::security::Descriptor;

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
    /// This side failed to encode its own outgoing request before sending
    /// a single byte to the peer.
    ///
    /// Distinct from [`Protocol`](Fault::Protocol): the peer never saw
    /// this request and never misbehaved, so the connection is not at
    /// fault and does not need to be dropped. Only the request itself —
    /// e.g. a reconversion selection too large to fit the wire format —
    /// is rejected.
    Encode(sakura_proto::Error),
    /// The reply did not arrive inside the caller's deadline. Only the
    /// client end can produce this: the DLL must never block a keystroke
    /// for longer than 50 ms (DESIGN 4.3), and a slow engine is answered
    /// by passing the key through, not by waiting.
    ///
    /// The connection survives a timeout — the request may still be in
    /// flight — which is why the protocol puts a monotonic id on every
    /// frame. A late reply is matched by id and discarded, never applied
    /// to whatever the user typed next.
    Timeout,
    /// A reply arrived carrying a request id that was never sent.
    ///
    /// Distinct from [`Protocol`](Fault::Protocol): the bytes decoded
    /// fine, they just do not answer anything this client asked. Nothing
    /// on the connection can be trusted after that, and unlike a late
    /// reply — which is expected and dropped — this one cannot be
    /// explained away.
    Desynchronized,
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
            Fault::Desynchronized => write!(f, "reply to a request that was never sent"),
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
        let wide = to_wide_nul(name);
        let mut flags = PIPE_ACCESS_DUPLEX;
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
                MAX_INSTANCES,
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
        Ok(PipeInstance { handle })
    }

    /// Blocks until a client connects.
    ///
    /// `ERROR_PIPE_CONNECTED` is success, not failure: it means a client
    /// won the race between `CreateNamedPipeW` and this call and is
    /// already on the other end. Treating it as an error is the classic way
    /// to drop every connection that arrives too quickly.
    pub fn wait_for_client(&self) -> windows::core::Result<()> {
        // SAFETY: `handle` is a live pipe instance owned by this struct.
        // The overlapped pointer is null, which is what makes the call
        // block, as the pipe was created without `FILE_FLAG_OVERLAPPED`.
        match unsafe { ConnectNamedPipe(self.handle, None) } {
            Ok(()) => Ok(()),
            Err(error) if is(&error, ERROR_PIPE_CONNECTED) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Releases the current client so the instance can accept another.
    ///
    /// Flushes first: `DisconnectNamedPipe` discards whatever the client
    /// has not yet read, and the last thing written is usually the reply
    /// the client is waiting for.
    pub fn disconnect(&self) {
        // SAFETY: `handle` is a live pipe instance owned by this struct.
        // Both calls fail harmlessly when no client is connected, which is
        // why their results are discarded.
        unsafe {
            let _ = FlushFileBuffers(self.handle);
            let _ = DisconnectNamedPipe(self.handle);
        }
    }

    /// Fills `buf` completely, looping over partial reads.
    ///
    /// A byte-mode pipe may return fewer bytes than asked for, so a single
    /// `ReadFile` is not a read of a known length.
    fn read_exact(&self, buf: &mut [u8]) -> Result<(), Fault> {
        let mut filled = 0;
        while filled < buf.len() {
            let mut read = 0u32;
            let rest = &mut buf[filled..];
            // SAFETY: `rest` is a live, uniquely borrowed slice and `read`
            // is a valid out-parameter; the overlapped pointer is null to
            // match the pipe's blocking mode.
            let result = unsafe { ReadFile(self.handle, Some(rest), Some(&mut read), None) };
            match result {
                Ok(()) if read == 0 => return Err(Fault::Disconnected),
                Ok(()) => filled += read as usize,
                Err(error) if is_disconnect(&error) => return Err(Fault::Disconnected),
                Err(error) => return Err(Fault::Os(error)),
            }
        }
        Ok(())
    }

    /// Writes every byte of `buf`, looping over partial writes.
    pub fn write_all(&self, buf: &[u8]) -> Result<(), Fault> {
        let mut written = 0;
        while written < buf.len() {
            let mut count = 0u32;
            let rest = &buf[written..];
            // SAFETY: `rest` is a live slice and `count` is a valid
            // out-parameter; the overlapped pointer is null to match the
            // pipe's blocking mode.
            let result = unsafe { WriteFile(self.handle, Some(rest), Some(&mut count), None) };
            match result {
                Ok(()) if count == 0 => return Err(Fault::Disconnected),
                Ok(()) => written += count as usize,
                Err(error) if is_disconnect(&error) => return Err(Fault::Disconnected),
                Err(error) => return Err(Fault::Os(error)),
            }
        }
        Ok(())
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
}

impl Drop for PipeInstance {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: the handle came from `CreateNamedPipeW` and is closed
            // exactly once, here.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
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
