//! The connecting end of the pipe.
//!
//! Used by the TSF DLL on every keystroke, and by `sakura_regtool --stop`
//! and the renderer's watchdog occasionally. All three need the same two
//! things the server end does not: a *deadline*, and a rule for what to do
//! with a reply that arrives after it.
//!
//! # Why every operation is overlapped
//!
//! A blocking read on a byte-mode pipe has no timeout. There is no flag
//! for one, `SetNamedPipeHandleState` does not add one, and the collection
//! timeout in `CreateNamedPipe` is about write batching, not reads. So a
//! DLL that reads the engine's reply with a plain `ReadFile` waits as long
//! as the engine takes — and that DLL is loaded into the host application,
//! on the thread delivering the keystroke. An engine that wedges would
//! wedge Word.
//!
//! The budget is 50 ms per keystroke (DESIGN 4.3), which is a real
//! deadline, so every call here is issued overlapped and waited on with a
//! timeout. Missing it is [`Fault::Timeout`] and the caller passes the key
//! through untouched.
//!
//! # Why a timeout does not close the connection
//!
//! The request is still in flight when the deadline passes. Reconnecting
//! would throw away the session — the engine's per-session state is keyed
//! to the connection — so the connection is kept and the *reply* is dealt
//! with instead: every frame carries a monotonic request id, and a reply
//! whose id is older than what this client is now waiting for is read and
//! dropped, silently, before it can be mistaken for an answer.
//!
//! That rule is here rather than in the DLL on purpose. A late "commit
//! これは" applied on top of text the application already received raw is
//! the single worst failure this protocol can produce, and it is exactly
//! the kind of thing that gets reimplemented slightly wrong in a second
//! place.

use std::time::{Duration, Instant};

use sakura_proto::{
    decode_response, encode_request, payload_len, peek_header, Request, RequestId, Response,
    FRAME_HEADER_LEN,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED, HANDLE, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};

use crate::security::{pipe_name, CLIENT_ACCESS};
use crate::transport::Fault;

/// What an installer or a command-line tool can afford to wait for a free
/// pipe instance. Reached only when more than `MAX_INSTANCES` applications
/// connect at once, and then only until a thread finishes accepting.
///
/// The DLL must not use this. Its connect happens on the host
/// application's keystroke thread, where two seconds is two seconds of a
/// frozen editor, so it passes a budget of its own.
pub const PATIENT_CONNECT: Duration = Duration::from_millis(2_000);

/// A connection to the engine.
#[derive(Debug)]
pub struct Client {
    handle: HANDLE,
    /// Signalled when an overlapped operation completes. Manual-reset,
    /// because an auto-reset event consumed by the wrong wait is how
    /// overlapped code deadlocks.
    event: HANDLE,
    /// The id of the next request. Monotonic for the life of the
    /// connection: it is what makes a late reply recognizable.
    next_id: RequestId,
    request: Vec<u8>,
    reply: Vec<u8>,
}

// SAFETY: both handles are kernel objects with no thread affinity, and
// `Client` hands out no references to them. `Sync` is deliberately not
// claimed — two threads sharing one connection would interleave frames.
unsafe impl Send for Client {}

impl Client {
    /// Connects to this logon session's engine, giving up after `budget`.
    ///
    /// The budget bounds the wait for a *free instance*, which is the only
    /// part of connecting that can take real time. Callers on a keystroke
    /// thread pass something they can afford to lose; tools that are
    /// allowed to wait pass [`PATIENT_CONNECT`].
    pub fn connect(budget: Duration) -> Result<Self, Fault> {
        Self::connect_to(&pipe_name()?, budget)
    }

    /// Connects to a pipe by name. Exposed for tests that stand up their
    /// own server; production code wants [`connect`](Self::connect), which
    /// cannot name the wrong pipe.
    pub fn connect_to(name: &str, budget: Duration) -> Result<Self, Fault> {
        let deadline = Instant::now() + budget;
        let wide = to_wide_nul(name);
        let handle = loop {
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            // `CLIENT_ACCESS` is the exact mask the server grants; asking
            // for `GENERIC_WRITE` here would be denied outright, because
            // the server withholds the create-instance bit that shares its
            // encoding (see `security::CLIENT_ACCESS`).
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    CLIENT_ACCESS,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match opened {
                Ok(handle) => break handle,
                Err(error) if is(&error, ERROR_PIPE_BUSY) => {
                    // Every instance is mid-accept. Wait for one, but only
                    // for what is left of the budget: winning the race for
                    // an instance and then having no time to use it is
                    // still a timeout, and a caller that said 20 ms must
                    // not be held for two seconds by a busy pipe.
                    let left = remaining_ms(deadline);
                    if left == 0 {
                        return Err(Fault::Timeout);
                    }
                    // SAFETY: `wide` is NUL-terminated and outlives the
                    // call.
                    let waited = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), left) };
                    if !waited.as_bool() {
                        return Err(Fault::Timeout);
                    }
                }
                Err(error) => return Err(Fault::Os(error)),
            }
        };

        // SAFETY: no security attributes and no name; the returned handle
        // is owned by this struct and closed in `Drop`.
        let event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(event) => event,
            Err(error) => {
                // SAFETY: `handle` was just opened by this function and is
                // not stored anywhere yet.
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(Fault::Os(error));
            }
        };

        Ok(Client {
            handle,
            event,
            next_id: 1,
            request: Vec::new(),
            reply: Vec::new(),
        })
    }

    /// Sends `request` and waits up to `budget` for its reply.
    ///
    /// Replies to earlier requests — ones this client already gave up on —
    /// are discarded on the way, without disturbing the connection.
    pub fn call(&mut self, request: &Request, budget: Duration) -> Result<Response, Fault> {
        let deadline = Instant::now() + budget;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        encode_request(request, id, &mut self.request).map_err(Fault::Encode)?;
        // Borrowed out of `self` so the buffer is not aliased by the
        // `&mut self` the I/O helpers take.
        let frame = core::mem::take(&mut self.request);
        let sent = self.write_all(&frame, deadline);
        self.request = frame;
        sent?;

        loop {
            let payload = self.read_frame(deadline)?;
            let header = peek_header(payload).map_err(Fault::Protocol)?;
            if header.request_id == id {
                let (_, response) = decode_response(payload).map_err(Fault::Protocol)?;
                return Ok(response);
            }
            // An answer to something we stopped waiting for. Dropping it
            // is the whole point of the id; applying it would land old
            // text on top of new.
            if header.request_id > id {
                return Err(Fault::Desynchronized);
            }
        }
    }

    /// The id the next [`call`](Self::call) will use. Exposed so a caller
    /// that logs timeouts can correlate them with the engine's own log.
    pub fn next_request_id(&self) -> RequestId {
        self.next_id
    }

    /// Returns the PID of the server attached to this exact pipe connection.
    ///
    /// Unlike looking up a pipe by name, this queries the kernel handle that
    /// [`connect_to`](Self::connect_to) opened. Callers that own a short-lived
    /// test server can use it to fail closed before sending a request if a
    /// different process won a pipe-name race.
    pub fn server_process_id(&self) -> Result<u32, Fault> {
        let mut process_id = 0;
        // SAFETY: `self.handle` is this live client's pipe handle, and the
        // mutable output points to initialized storage for the duration of the
        // call.
        unsafe { GetNamedPipeServerProcessId(self.handle, &mut process_id) }.map_err(Fault::Os)?;
        Ok(process_id)
    }

    fn read_frame(&mut self, deadline: Instant) -> Result<&[u8], Fault> {
        let mut header = [0u8; FRAME_HEADER_LEN];
        self.read_exact(&mut header, deadline)?;
        // Validated before the body is read: the length prefix is the one
        // field the other end fully controls.
        let len = payload_len(&header).map_err(Fault::Protocol)?;

        let mut body = core::mem::take(&mut self.reply);
        body.clear();
        body.resize(len, 0);
        let read = self.read_exact(&mut body, deadline);
        self.reply = body;
        read?;
        Ok(&self.reply)
    }

    fn read_exact(&mut self, buf: &mut [u8], deadline: Instant) -> Result<(), Fault> {
        let mut filled = 0;
        while filled < buf.len() {
            let read = self.transfer(Op::Read(&mut buf[filled..]), deadline)?;
            if read == 0 {
                return Err(Fault::Disconnected);
            }
            filled += read;
        }
        Ok(())
    }

    fn write_all(&mut self, buf: &[u8], deadline: Instant) -> Result<(), Fault> {
        let mut written = 0;
        while written < buf.len() {
            let count = self.transfer(Op::Write(&buf[written..]), deadline)?;
            if count == 0 {
                return Err(Fault::Disconnected);
            }
            written += count;
        }
        Ok(())
    }

    /// Issues one overlapped operation and waits for it, or cancels it.
    fn transfer(&mut self, op: Op<'_>, deadline: Instant) -> Result<usize, Fault> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.event,
            ..Default::default()
        };
        let mut transferred = 0u32;

        // SAFETY: the buffer outlives this function, and this function
        // does not return while the kernel may still be writing to it:
        // every path either waits for completion or cancels and then waits
        // for the cancellation to finish. `overlapped` likewise lives
        // until after the final `GetOverlappedResult`.
        let started = unsafe {
            match op {
                Op::Read(buf) => ReadFile(
                    self.handle,
                    Some(buf),
                    Some(&mut transferred),
                    Some(&mut overlapped),
                ),
                Op::Write(buf) => WriteFile(
                    self.handle,
                    Some(buf),
                    Some(&mut transferred),
                    Some(&mut overlapped),
                ),
            }
        };

        match started {
            Ok(()) => return Ok(transferred as usize),
            Err(error) if is(&error, ERROR_IO_PENDING) => {}
            Err(error) if is_disconnect(&error) => return Err(Fault::Disconnected),
            Err(error) => return Err(Fault::Os(error)),
        }

        // SAFETY: `overlapped` describes the operation just started on
        // `self.handle` and is still live.
        let finished = unsafe {
            GetOverlappedResultEx(
                self.handle,
                &overlapped,
                &mut transferred,
                remaining_ms(deadline),
                false,
            )
        };

        match finished {
            Ok(()) => Ok(transferred as usize),
            // The timeout arrives as a WAIT_* status rather than an
            // ERROR_*, which is why it is not checked with `is`.
            Err(error) if error.code() == windows::core::HRESULT::from_win32(WAIT_TIMEOUT.0) => {
                self.abandon(&overlapped);
                Err(Fault::Timeout)
            }
            Err(error) if is_disconnect(&error) => Err(Fault::Disconnected),
            Err(error) => Err(Fault::Os(error)),
        }
    }

    /// Cancels an operation that ran out of time and waits for the kernel
    /// to let go of its buffer.
    ///
    /// The wait is not optional. `CancelIoEx` only *requests* cancellation;
    /// until the operation actually completes, the kernel may still write
    /// into the buffer and the `OVERLAPPED`. Returning before then would
    /// hand the caller a buffer that is still being written to — a
    /// use-after-free that reproduces only under load, which is the worst
    /// kind.
    fn abandon(&self, overlapped: &OVERLAPPED) {
        // SAFETY: `overlapped` describes an operation still outstanding on
        // `self.handle`.
        unsafe {
            let _ = CancelIoEx(self.handle, Some(overlapped));
            let mut transferred = 0u32;
            // Blocks until the cancellation lands; the expected result is
            // ERROR_OPERATION_ABORTED, and any other outcome equally means
            // the kernel is done with the buffer.
            let _ = GetOverlappedResult(self.handle, overlapped, &mut transferred, true);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: both handles were created by `connect_to` and are closed
        // exactly once, here. Closing the pipe cancels anything still
        // outstanding on it, and nothing is outstanding anyway: every
        // method waits for its own I/O to finish before returning.
        unsafe {
            let _ = CloseHandle(self.handle);
            let _ = CloseHandle(self.event);
        }
    }
}

/// Milliseconds left before `deadline`, saturating at zero.
///
/// Zero means "do not wait", which is what both callers want: a wait that
/// is already out of time must not be issued as an infinite one, and
/// `INFINITE` is `u32::MAX`, so a wrapping subtraction here would be the
/// difference between a dropped keystroke and a hung application.
fn remaining_ms(deadline: Instant) -> u32 {
    deadline
        .checked_duration_since(Instant::now())
        .map(|left| left.as_millis().min(u32::MAX as u128) as u32)
        .unwrap_or(0)
}

/// Which direction one overlapped operation goes.
enum Op<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

fn is_disconnect(error: &windows::core::Error) -> bool {
    is(error, ERROR_BROKEN_PIPE)
        || is(error, ERROR_PIPE_NOT_CONNECTED)
        || is(error, ERROR_NO_DATA)
        || is(error, ERROR_OPERATION_ABORTED)
}

fn is(error: &windows::core::Error, code: WIN32_ERROR) -> bool {
    error.code() == windows::core::HRESULT::from_win32(code.0)
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// `FILE_FLAG_OVERLAPPED` is a `FILE_FLAGS_AND_ATTRIBUTES`; naming the
/// import above keeps the `CreateFileW` call readable.
const _: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAG_OVERLAPPED;
