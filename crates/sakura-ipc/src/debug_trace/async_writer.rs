//! Optional process-owned writer. The engine opts in before serving clients;
//! TSF DLLs keep the synchronous, thread-free policy. A queue failure never
//! sends the caller back to disk. Producer timestamp/thread identity survives
//! the handoff, and shutdown abandons queued work after the current write.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};

use super::{CapturedEvent, BACKOFF, MAX_GENERATION_BYTES};
use std::io;

const CAPACITY: usize = 1024;
static REQUESTED: AtomicBool = AtomicBool::new(false);
static EMITTER: OnceLock<Emitter> = OnceLock::new();

#[derive(Debug, Default)]
struct Counters {
    accepting: AtomicBool,
    attempted: AtomicU64,
    written: AtomicU64,
    failures: AtomicU64,
}

struct Emitter {
    sender: SyncSender<Option<CapturedEvent>>,
    counters: Arc<Counters>,
}

impl Emitter {
    fn emit(&self, event: CapturedEvent) {
        self.counters.attempted.fetch_add(1, Ordering::Relaxed);
        if self.counters.accepting.load(Ordering::Acquire) {
            // Full/disconnected queues drop diagnostics, never block input.
            let _ = self.sender.try_send(Some(event));
        }
    }
}

/// Content-free counter snapshot after the writer has joined. `dropped`
/// includes a full/closed queue and work abandoned during shutdown. Concurrent
/// producers may still be finishing an enqueue when this snapshot is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncTraceStats {
    pub attempted: u64,
    pub written: u64,
    pub failures: u64,
    pub dropped: u64,
    pub worker_panicked: bool,
}

/// Owns the sole optional trace thread. Stop it after request producers have
/// joined when exact final accounting is required. Only the current operation can delay
/// shutdown; the backlog is discarded rather than flushed on the caller.
#[derive(Debug)]
pub struct AsyncTraceWriter {
    sender: SyncSender<Option<CapturedEvent>>,
    counters: Arc<Counters>,
    worker: Option<JoinHandle<()>>,
}

impl AsyncTraceWriter {
    pub fn stop(mut self) -> AsyncTraceStats {
        self.finish()
    }

    fn finish(&mut self) -> AsyncTraceStats {
        self.counters.accepting.store(false, Ordering::Release);
        // Wake an idle receiver. A full queue already has work to wake it.
        let _ = self.sender.try_send(None);
        let worker_panicked = self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err());
        let attempted = self.counters.attempted.load(Ordering::Relaxed);
        let written = self.counters.written.load(Ordering::Relaxed);
        let failures = self.counters.failures.load(Ordering::Relaxed);
        AsyncTraceStats {
            attempted,
            written,
            failures,
            dropped: attempted.saturating_sub(written.saturating_add(failures)),
            worker_panicked,
        }
    }
}

impl Drop for AsyncTraceWriter {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.finish();
        }
    }
}

/// Opt this process into asynchronous tracing exactly once. Startup failure
/// leaves tracing unavailable, never synchronous on a later key path.
pub fn start_async_writer() -> io::Result<AsyncTraceWriter> {
    start_with_before_write(|| {})
}

/// Compile-time fixture availability, owned here even when a tool reuses the
/// engine's main.rs under a different package's feature namespace.
pub const WRITE_DELAY_FIXTURE_ENABLED: bool = cfg!(feature = "dev-fixtures");

/// Real-process fixture: delay each write on the writer thread, before disk
/// access. The engine accepts this only with an explicit private test pipe.
/// Default/shipping builds reject it; the delay implementation is compiled out.
pub fn start_async_writer_with_delay(delay: std::time::Duration) -> io::Result<AsyncTraceWriter> {
    #[cfg(feature = "dev-fixtures")]
    {
        if delay.is_zero() || delay > std::time::Duration::from_secs(10) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trace delay must be 1..10000 ms",
            ));
        }
        start_with_before_write(move || thread::sleep(delay))
    }
    #[cfg(not(feature = "dev-fixtures"))]
    {
        let _ = delay;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "trace delay requires dev-fixtures",
        ))
    }
}

fn start_with_before_write(
    mut before_write: impl FnMut() + Send + 'static,
) -> io::Result<AsyncTraceWriter> {
    if REQUESTED.swap(true, Ordering::AcqRel) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "trace writer already requested",
        ));
    }
    let path = super::default_path()?;
    let (emitter, writer) = spawn(CAPACITY, move |event| {
        before_write();
        super::emit_through(
            &BACKOFF,
            super::monotonic_ms(),
            super::jitter_seed(),
            &path,
            event,
            MAX_GENERATION_BYTES,
        )
    })?;
    // REQUESTED gives this initializer unique publication ownership.
    if EMITTER.set(emitter).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "trace writer already installed",
        ));
    }
    Ok(writer)
}

pub(super) fn route(event: CapturedEvent) -> bool {
    if !REQUESTED.load(Ordering::Acquire) {
        return false;
    }
    if let Some(emitter) = EMITTER.get() {
        emitter.emit(event);
    }
    true
}

fn spawn(
    capacity: usize,
    mut sink: impl FnMut(CapturedEvent) -> io::Result<()> + Send + 'static,
) -> io::Result<(Emitter, AsyncTraceWriter)> {
    let (sender, receiver) = mpsc::sync_channel::<Option<CapturedEvent>>(capacity);
    let counters = Arc::new(Counters::default());
    counters.accepting.store(true, Ordering::Release);
    let worker_counters = Arc::clone(&counters);
    let worker = thread::Builder::new()
        .name("sakura-debug-trace".into())
        .spawn(move || {
            while let Ok(Some(event)) = receiver.recv() {
                if !worker_counters.accepting.load(Ordering::Acquire) {
                    break;
                }
                let counter = if sink(event).is_ok() {
                    &worker_counters.written
                } else {
                    &worker_counters.failures
                };
                counter.fetch_add(1, Ordering::Relaxed);
            }
        })?;
    Ok((
        Emitter {
            sender: sender.clone(),
            counters: Arc::clone(&counters),
        },
        AsyncTraceWriter {
            sender,
            counters,
            worker: Some(worker),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn event() -> CapturedEvent {
        super::super::TraceEvent {
            component: "engine",
            instance: 1,
            event: "key_result",
            decision: "apply",
            k0: 0,
            k1: 0,
            k2: 0,
            k3: 0,
        }
        .capture()
    }

    #[test]
    fn stalled_trace_sink_does_not_block_producers_and_shutdown_drops_backlog() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (emitter, writer) = spawn(2, move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
        .unwrap();
        emitter.emit(event());
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // The sink remains blocked throughout these sends, including overflow.
        let (done_tx, done_rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            for _ in 0..3 {
                emitter.emit(event());
            }
            done_tx.send(()).unwrap();
        });
        let completed = done_rx.recv_timeout(Duration::from_secs(1));
        writer.counters.accepting.store(false, Ordering::Release);
        release_tx.send(()).unwrap();
        producer.join().unwrap();
        let stats = writer.stop();
        assert!(
            completed.is_ok(),
            "a blocked diagnostic write blocked key-path producers"
        );
        assert_eq!(
            stats,
            AsyncTraceStats {
                attempted: 4,
                written: 1,
                failures: 0,
                dropped: 3,
                worker_panicked: false
            }
        );
    }

    #[test]
    fn preserves_producer_identity_and_counts_failed_writes() {
        let original = event();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (emitter, writer) = spawn(2, move |captured| {
            seen_tx.send(super::super::format_row(captured)).unwrap();
            Err(io::Error::other("synthetic I/O failure"))
        })
        .unwrap();
        emitter.emit(original);
        assert_eq!(
            seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            super::super::format_row(original)
        );
        let stats = writer.stop();
        assert_eq!(stats.attempted, 1);
        assert_eq!(stats.failures, 1);
        assert_eq!(stats.dropped, 0);
    }

    #[test]
    fn idle_writer_stops_and_a_closed_emitter_never_reaches_the_sink() {
        let (emitter, writer) = spawn(1, |_| panic!("closed sink must not run")).unwrap();
        assert_eq!(writer.stop().attempted, 0);
        emitter.emit(event());
        assert_eq!(emitter.counters.attempted.load(Ordering::Relaxed), 1);
        assert_eq!(emitter.counters.written.load(Ordering::Relaxed), 0);
    }
}
