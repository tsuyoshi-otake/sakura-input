//! How long a keystroke actually takes to cross the pipe and come back.
//!
//! This is the Phase 1 exit criterion "SendKey round-trip p99 < 5 ms"
//! (PLAN.md), measured against the arrangement that ships: the built
//! `sakura_engine.exe`, the well-known per-logon-session pipe, and a client
//! in a separate process. Everything cheaper than that — timing `dispatch`
//! directly, or a scratch pipe inside one process — measures something the
//! user never experiences.
//!
//! It is what the user experiences that sets the budget. DESIGN 10 gives the
//! whole keystroke 5 ms, and the round trip measured here is only part of it:
//! the text service still has to hand the result back to the host
//! application, which then has to draw. So p99 < 5 ms is a ceiling for a
//! *component* of a 5 ms budget, and the interesting number is not whether it
//! passes but by how much — which is why this prints the whole distribution
//! instead of just a verdict.
//!
//! # Why it is ignored by default
//!
//! Two reasons, and only the first is about flakiness. A timing threshold on
//! a shared CI runner fails for reasons that have nothing to do with the
//! code, and a test that fails for unrelated reasons gets muted, at which
//! point it protects nothing. The second is that this test starts and talks
//! to a process singleton: run concurrently with `pipe_round_trip.rs` — which
//! `cargo test` would happily do, since separate test binaries run in
//! parallel — the two would contend for one engine and both would measure
//! something meaningless.
//!
//! So it runs on purpose, on a machine somebody chose:
//!
//! ```text
//! cargo test -p sakura-engine --release --test ipc_latency -- --ignored --nocapture
//! ```
//!
//! Release matters. A debug engine is several times slower and says nothing
//! about the binary that ships.

mod common;

use std::time::{Duration, Instant};

use sakura_proto::{KeyCode, Request, Response};

use common::{char_key, named_key, session_for, visible, Engine, PATIENT};

/// Enough samples that the 99th percentile is an observation rather than an
/// extrapolation: 5,000 keystrokes puts 50 samples above p99, so one unlucky
/// scheduling hiccup moves it instead of defining it.
const SAMPLES: usize = 5_000;

/// Keystrokes sent before the clock starts, to pay for the one-time costs
/// that a user pays once and a benchmark must not charge to every keystroke:
/// the first page faults, the connection's first buffer growth, and whatever
/// the branch predictors and caches learn on the way through.
const WARMUP: usize = 500;

/// DESIGN 10's per-keystroke budget. The round trip has to fit inside this
/// with room left for the text service and the host application's redraw, so
/// treating the whole budget as the pass mark for one component is generous
/// on purpose — it is a smoke alarm, not a target.
const BUDGET: Duration = Duration::from_millis(5);

/// Romaji that composes into kana and commits, over and over. A benchmark
/// that sent the same key 5,000 times would measure a path the engine never
/// takes: real typing grows a preedit, resolves it, and starts again, and the
/// commit is the expensive keystroke.
fn keystroke(n: usize) -> sakura_proto::KeyInput {
    const ROMAJI: &[u8] = b"sakuranyuuryoku";
    let step = n % (ROMAJI.len() + 1);
    if step == ROMAJI.len() {
        named_key(KeyCode::Enter)
    } else {
        char_key(ROMAJI[step] as char)
    }
}

#[test]
#[ignore = "timing against a process singleton: run with --release --ignored --nocapture"]
fn a_keystroke_crosses_the_pipe_and_returns_inside_the_budget() {
    let mut engine = Engine::running();
    if !engine.compatible() {
        eprintln!("skipping IPC benchmark: an older engine owns the well-known pipe");
        return;
    }
    let mut client = engine.client();
    let session = session_for(&mut client, "ipc_latency.exe");

    // Warm up, and prove on the way through that these keystrokes actually
    // compose. A run that measured 5,000 rejected keys would be fast and
    // completely worthless, so the check is here rather than in a comment.
    let mut composed = String::new();
    for n in 0..WARMUP {
        if let Response::Output(output) = send(&mut client, session, keystroke(n)) {
            let preedit = visible(output.preedit);
            if !preedit.is_empty() {
                composed = preedit;
            }
        }
    }
    assert!(
        composed.starts_with('さ'),
        "the benchmark is not composing anything ({composed:?}); it would be timing rejected keys"
    );

    let mut samples = Vec::with_capacity(SAMPLES);
    for n in 0..SAMPLES {
        let key = keystroke(WARMUP + n);
        let start = Instant::now();
        let response = send(&mut client, session, key);
        samples.push(start.elapsed());
        // Reading the response inside the timed region is the point: the
        // round trip is not over until the client has the answer it would
        // draw from.
        debug_assert!(matches!(response, Response::Output(_)));
    }

    samples.sort_unstable();
    let report = Percentiles::of(&samples);
    println!("\nSendKey round trip over the well-known pipe, {SAMPLES} samples\n{report}");
    write_report_if_requested(&report);

    assert!(
        report.p99 < BUDGET,
        "p99 {:?} exceeds the {BUDGET:?} keystroke budget (DESIGN 10)\n{report}",
        report.p99,
    );
}

fn write_report_if_requested(report: &Percentiles) {
    let Some(path) = std::env::var_os("SAKURA_IPC_LATENCY_REPORT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create IPC latency report directory");
    }
    let micros = |duration: Duration| duration.as_secs_f64() * 1e6;
    let body = format!(
        concat!(
            "{{\n",
            "  \"schema_version\": 1,\n",
            "  \"samples\": {},\n",
            "  \"budget_us\": {:.3},\n",
            "  \"min_us\": {:.3},\n",
            "  \"p50_us\": {:.3},\n",
            "  \"p90_us\": {:.3},\n",
            "  \"p99_us\": {:.3},\n",
            "  \"max_us\": {:.3},\n",
            "  \"mean_us\": {:.3},\n",
            "  \"passed\": {}\n",
            "}}\n"
        ),
        SAMPLES,
        micros(BUDGET),
        micros(report.min),
        micros(report.p50),
        micros(report.p90),
        micros(report.p99),
        micros(report.max),
        micros(report.mean),
        report.p99 < BUDGET,
    );
    std::fs::write(&path, body).expect("write IPC latency report");
}

fn send(
    client: &mut sakura_ipc::Client,
    session: sakura_proto::SessionId,
    key: sakura_proto::KeyInput,
) -> Response {
    match client.call(&Request::SendKey { session, key }, PATIENT) {
        Ok(response) => response,
        Err(fault) => panic!("SendKey failed mid-benchmark: {fault:?}"),
    }
}

/// The shape of the distribution, not just its verdict.
///
/// The maximum is reported alongside the percentiles because it is the one a
/// user would actually notice — a single 40 ms keystroke is a visible stutter
/// even when p99 is comfortable — and because a max far above p99 says the
/// cost is something occasional (a scheduler quantum, a page fault) rather
/// than something the code does every time.
#[derive(Debug)]
struct Percentiles {
    min: Duration,
    p50: Duration,
    p90: Duration,
    p99: Duration,
    max: Duration,
    mean: Duration,
}

impl Percentiles {
    /// `sorted` must be ascending; the caller sorts because it also owns the
    /// samples.
    fn of(sorted: &[Duration]) -> Percentiles {
        assert!(!sorted.is_empty(), "nothing was measured");
        let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
        Percentiles {
            min: sorted[0],
            p50: at(0.50),
            p90: at(0.90),
            p99: at(0.99),
            max: sorted[sorted.len() - 1],
            mean: sorted.iter().sum::<Duration>() / sorted.len() as u32,
        }
    }
}

impl std::fmt::Display for Percentiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        write!(
            f,
            "  min {:8.1} us\n  p50 {:8.1} us\n  p90 {:8.1} us\n  p99 {:8.1} us\n  max {:8.1} us\n mean {:8.1} us",
            us(self.min),
            us(self.p50),
            us(self.p90),
            us(self.p99),
            us(self.max),
            us(self.mean),
        )
    }
}
