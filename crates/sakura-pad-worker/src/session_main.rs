//! Experimental persistent Pad crypto worker. No credential CLI arguments.
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sakura_pad_worker::session_protocol;

const ABSOLUTE_LIFETIME: Duration = Duration::from_secs(5 * 60);
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);

fn run() -> io::Result<()> {
    let started = Instant::now();
    let last_completed_ms = Arc::new(AtomicU64::new(0));
    let watchdog_activity = Arc::clone(&last_completed_ms);
    std::thread::Builder::new()
        .name("pad-session-deadline".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            let elapsed = started.elapsed();
            let last = Duration::from_millis(watchdog_activity.load(Ordering::Relaxed));
            if elapsed >= ABSOLUTE_LIFETIME || elapsed.saturating_sub(last) >= INACTIVITY_TIMEOUT {
                // A blocked/truncated pipe cannot keep keys in a living child.
                std::process::exit(124);
            }
        })?;
    session_protocol::run(io::stdin().lock(), io::stdout().lock(), || {
        let elapsed = started.elapsed().as_millis();
        last_completed_ms.store(elapsed.min(u64::MAX as u128) as u64, Ordering::Relaxed);
    })
}

fn main() {
    if std::env::args_os().len() != 1 || run().is_err() {
        eprintln!("Pad session could not complete its request");
        std::process::exit(1);
    }
}
