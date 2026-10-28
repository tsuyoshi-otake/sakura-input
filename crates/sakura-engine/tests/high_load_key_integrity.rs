//! Keeps input intact while the real engine is deliberately held still.
//!
//! Every test here launches the shipped `sakura_engine.exe` on its own private
//! pipe and its own `LOCALAPPDATA`, arms one of the four delay points inside
//! that child, and drives synthetic keys through it. Nothing reads or writes
//! the user's engine, history, learning or dictionary.
//!
//! The property under test is not speed. A stalled engine is allowed to be
//! slow; it is not allowed to drop a key, apply one twice, reorder two, or
//! answer a new request with an old reply. Each test therefore checks the
//! engine's own fault counters as well, because a run in which nothing was
//! ever injected would otherwise pass every one of these assertions while
//! testing nothing at all.
//!
//! The last two tests hold the engine still the way a busy machine does,
//! without a delay point: the engine is started below normal, as the logon
//! task starts it, and either checks the class it moves itself to or competes
//! with a saturated CPU (#322).

#[allow(dead_code)]
mod common;

use std::time::{Duration, Instant};

use sakura_ipc::{Client, Fault};
use sakura_proto::{
    FaultInjectionEntry, FaultPoint, KeyCode, Request, Response, SessionId, PROTOCOL_VERSION,
};

use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadPriority, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, THREAD_PRIORITY, THREAD_PRIORITY_HIGHEST,
};

use common::{char_key, named_key, session_for, visible, Engine, PATIENT};

/// What the TSF key path allows one key once the engine has missed two waits
/// in a row (`sakura_tsf::key_wait::DESIGN_BUDGET`). Reproduced rather than
/// imported: this crate must not depend on the DLL, and the number's role here
/// is to be a client that abandons a key, not to track that constant.
const TSF_KEY_BUDGET: Duration = Duration::from_millis(50);

/// What the TSF key path allows one key while the engine keeps answering
/// (`sakura_tsf::key_wait::PATIENT_BUDGET`, #415), reproduced for the same
/// reason as [`TSF_KEY_BUDGET`].
const TSF_PATIENT_KEY_BUDGET: Duration = Duration::from_millis(200);

fn handshake(client: &mut Client) {
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { server_version, .. }) => {
            assert_eq!(server_version, PROTOCOL_VERSION);
        }
        other => panic!("handshake: expected Hello, got {other:?}"),
    }
}

fn send_char(client: &mut Client, session: SessionId, character: char) -> Response {
    client
        .call(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            PATIENT,
        )
        .expect("a delayed engine still answers a patient client")
}

fn send_named(client: &mut Client, session: SessionId, code: KeyCode) -> Response {
    client
        .call(
            &Request::SendKey {
                session,
                key: named_key(code),
            },
            PATIENT,
        )
        .expect("a delayed engine still answers a patient client")
}

fn preedit_of(response: &Response) -> String {
    match response {
        Response::Output(output) => visible(output.preedit.clone()),
        other => panic!("expected Output, got {other:?}"),
    }
}

fn commit_of(response: &Response) -> Option<String> {
    match response {
        Response::Output(output) => output.commit.clone(),
        other => panic!("expected Output, got {other:?}"),
    }
}

/// Sends a key the way a client that gives up at `TSF_KEY_BUDGET` does, and
/// asserts that it really did give up. The engine still applies the key; what
/// the client loses is the reply, not the keystroke.
fn abandon(client: &mut Client, session: SessionId, character: char) {
    let answer = client.call(
        &Request::SendKey {
            session,
            key: char_key(character),
        },
        TSF_KEY_BUDGET,
    );
    assert!(
        matches!(answer, Err(Fault::Timeout | Fault::DeadlineExpired)),
        "the client outlasted a stall it was supposed to abandon: {answer:?}"
    );
}

/// The one request `sakura_tsf`'s `Link::resync` sends before its next key.
fn revert(client: &mut Client, session: SessionId) {
    let answer = client.call(&Request::Revert { session }, PATIENT);
    assert!(
        matches!(answer, Ok(Response::Ok)),
        "the engine must accept the resync the TSF sends: {answer:?}"
    );
}

fn fault_status(client: &mut Client) -> Vec<FaultInjectionEntry> {
    match client.call(&Request::FaultStatus, PATIENT) {
        Ok(Response::FaultStatus { entries }) => {
            // A reading that skipped or reordered a point would put one
            // point's counters under another point's name.
            assert_eq!(entries.len(), FaultPoint::ALL.len());
            for (entry, point) in entries.iter().zip(FaultPoint::ALL) {
                assert_eq!(entry.point, point);
            }
            entries
        }
        other => panic!("expected FaultStatus, got {other:?}"),
    }
}

fn entry(entries: &[FaultInjectionEntry], point: FaultPoint) -> FaultInjectionEntry {
    entries[point as usize]
}

/// The shape every user's engine must have. If this ever fails, the delays
/// escaped the argument gate and a real installation could stall.
#[test]
fn an_engine_started_without_the_argument_has_every_point_disarmed() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);

    let entries = fault_status(&mut client);
    for entry in &entries {
        assert!(
            !entry.is_armed(),
            "{} is armed in an engine nobody armed",
            entry.point.name()
        );
        assert_eq!(entry.delay_us, 0, "{}", entry.point.name());
        assert_eq!(entry.fired, 0, "{}", entry.point.name());
        assert_eq!(entry.slept_us, 0, "{}", entry.point.name());
    }

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// The mildest of the four orderings: the key has not been looked at yet.
/// Delay is the only thing that may change.
#[test]
fn a_stall_before_dispatch_delays_every_key_without_losing_one() {
    let mut engine = Engine::spawn_isolated_with_faults("before-dispatch=50");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    let started = Instant::now();
    let mut seen = Vec::new();
    for character in "sakura".chars() {
        seen.push(preedit_of(&send_char(&mut client, session, character)));
    }
    let elapsed = started.elapsed();

    // Six keys, each held 50 ms before the engine looked at it. Measured on
    // the client so this is the delay a host would actually have suffered,
    // not the one the engine intended.
    assert!(
        elapsed >= Duration::from_millis(6 * 50),
        "six 50 ms stalls took only {elapsed:?}"
    );

    // Nothing lost, nothing doubled, nothing out of order: the reading grows
    // by exactly the roman letters typed, in the order they were typed.
    assert_eq!(
        seen,
        vec!["s", "さ", "さk", "さく", "さくr", "さくら"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        "a stalled engine changed what the reading became"
    );

    let commit = commit_of(&send_named(&mut client, session, KeyCode::Enter));
    assert_eq!(commit.as_deref(), Some("さくら"));
    // Exactly once: the terminal event happens on the key that caused it and
    // on no other.
    assert_eq!(commit_of(&send_char(&mut client, session, 'a')), None);

    let status = entry(&fault_status(&mut client), FaultPoint::BeforeDispatch);
    assert!(
        status.fired >= 8,
        "eight requests passed the point but only {} fired",
        status.fired
    );
    assert!(
        status.slept_us >= status.fired * 45_000,
        "{} fires slept only {} us in total",
        status.fired,
        status.slept_us
    );
    // "Every time" never exhausts, so a long run cannot quietly stop testing.
    assert_eq!(status.remaining, u64::MAX);

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// The ordering in which a lost key and a duplicated key are both possible:
/// the session has already advanced when the delay begins.
#[test]
fn a_stall_after_the_session_advanced_still_commits_exactly_once() {
    let mut engine = Engine::spawn_isolated_with_faults("after-mutation=400/1");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    // Only the first key is held, and only after the engine already applied
    // it. The reply must still describe that key and no other.
    let started = Instant::now();
    assert_eq!(preedit_of(&send_char(&mut client, session, 'k')), "k");
    assert!(started.elapsed() >= Duration::from_millis(400));

    let after = entry(&fault_status(&mut client), FaultPoint::AfterMutation);
    assert_eq!(after.fired, 1);
    // A bounded point that has spent its occurrences is disarmed, not
    // wrapped: the keys below are ordinary ones.
    assert_eq!(after.remaining, 0);
    assert!(!after.is_armed());

    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "か");
    assert_eq!(preedit_of(&send_char(&mut client, session, 'n')), "かn");
    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "かな");

    let commit = commit_of(&send_named(&mut client, session, KeyCode::Enter));
    assert_eq!(
        commit.as_deref(),
        Some("かな"),
        "the key that was stalled after being applied was applied once, not zero or twice"
    );

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// Conversion is where the dictionary, learning and prediction services meet.
/// Holding a key here must not split the session or lose its candidates.
#[test]
fn a_stall_during_conversion_keeps_the_session_and_its_candidates() {
    let mut engine = Engine::spawn_isolated_with_faults("during-conversion=100/3");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    for character in "kanji".chars() {
        send_char(&mut client, session, character);
    }
    let converted = send_named(&mut client, session, KeyCode::Space);
    match &converted {
        Response::Output(output) => {
            assert!(
                output
                    .candidates
                    .as_ref()
                    .is_some_and(|list| !list.items.is_empty()),
                "conversion after a stall produced no candidates"
            );
            assert!(!visible(output.preedit.clone()).is_empty());
        }
        other => panic!("expected Output, got {other:?}"),
    }

    let during = entry(&fault_status(&mut client), FaultPoint::DuringConversion);
    assert_eq!(during.fired, 3, "three keys should have been held");
    assert!(during.slept_us >= 3 * 95_000);

    // The same session keeps working afterwards: the stall delayed it, it did
    // not end it and did not start a second one behind the client's back.
    let commit = commit_of(&send_named(&mut client, session, KeyCode::Enter));
    assert!(commit.is_some_and(|text| !text.is_empty()));
    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "あ");

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// #415: a reply held 100 ms is past the old 50 ms budget and inside the new
/// 200 ms one. A client that waits the patient budget gets every reply, in
/// order, and the reading grows by exactly the letters typed: a held key is
/// slow, not lost, and nothing finalizes the composition.
#[test]
fn a_reply_held_inside_the_patient_budget_reaches_the_client_that_waits_for_it() {
    let mut engine = Engine::spawn_isolated_with_faults("during-reply=100");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    let mut seen = Vec::new();
    let mut slowest = Duration::ZERO;
    for character in "kana".chars() {
        let started = Instant::now();
        let reply = client
            .call(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                TSF_PATIENT_KEY_BUDGET,
            )
            .expect("a reply held 100 ms is inside the patient budget");
        slowest = slowest.max(started.elapsed());
        seen.push(preedit_of(&reply));
    }

    assert_eq!(seen, vec!["k", "か", "かn", "かな"]);
    assert!(
        slowest > TSF_KEY_BUDGET,
        "the replies were not held past the old budget: slowest {slowest:?}"
    );

    // Four key replies were held. Without this the run could pass with the
    // delay never armed.
    let status = entry(&fault_status(&mut client), FaultPoint::DuringReply);
    assert_eq!(status.fired, 4, "{status:?}");
    assert!(status.slept_us >= 4 * 95_000, "{status:?}");

    let commit = commit_of(&send_named(&mut client, session, KeyCode::Enter));
    assert_eq!(commit.as_deref(), Some("かな"));

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// The abandoned-key ordering. The engine has applied the key and produced
/// the answer; only delivery is late, and the client gives up first.
///
/// What this pins down is that the *engine* loses nothing, and that the late
/// answer never lands on the next request: the client that resumed sees its
/// own reply, containing both keys. Whether the host ever displays the first
/// one is a TSF-layer question and is not settled here.
#[test]
fn an_abandoned_reply_is_never_delivered_as_the_answer_to_the_next_key() {
    an_abandoned_reply_is_not_replayed(TSF_KEY_BUDGET);
}

/// The same property for the client that waits the patient budget (#415): its
/// first two misses are at 200 ms, and the engine must treat them no
/// differently from a miss at 50 ms.
#[test]
fn an_abandoned_reply_is_never_delivered_to_a_patient_client_either() {
    an_abandoned_reply_is_not_replayed(TSF_PATIENT_KEY_BUDGET);
}

fn an_abandoned_reply_is_not_replayed(client_budget: Duration) {
    let mut engine = Engine::spawn_isolated_with_faults("during-reply=400");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    let abandoned = client.call(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        client_budget,
    );
    assert!(
        matches!(abandoned, Err(Fault::Timeout | Fault::DeadlineExpired)),
        "a {client_budget:?} client should not have outlasted a 400 ms stall: {abandoned:?}"
    );

    // The stale reply for the abandoned key is still in the pipe. The next
    // call must not return it: applying an old answer under a new request's
    // id is how conversion state goes backwards.
    let next = send_char(&mut client, session, 'a');
    assert_eq!(
        preedit_of(&next),
        "か",
        "the reply to the second key must describe both keys, not replay the first"
    );

    // Two: this point sits on the key/output reply path only, so the two
    // SendKey replies are held and the administrative ones are not. Exactly
    // two is the assertion worth making — it says the abandoned key really
    // was stalled on its way out, and that the engine did not answer it twice.
    let status = entry(&fault_status(&mut client), FaultPoint::DuringReply);
    assert_eq!(status.fired, 2, "{status:?}");
    assert!(status.slept_us >= 2 * 395_000, "{status:?}");

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// What `Request::Revert` actually removes after a key was abandoned.
///
/// `sakura_tsf`'s `Link::resync` sends exactly this request when the previous
/// key timed out, and its doc comment justifies doing so by saying that what
/// it discards is "the engine's now-duplicate copy" of text the host already
/// committed. That is a claim about the engine's state, so it is checkable
/// here, against the real engine, with a real stall.
///
/// The same four keys are typed twice into one session, abandoning the third
/// the way a 50 ms client does. The two rounds differ in one request: the
/// second sends the `Revert`. What they disagree about is what the `Revert`
/// removed.
#[test]
fn the_revert_after_an_abandoned_key_discards_a_reading_the_client_never_saw() {
    let mut engine = Engine::spawn_isolated_with_faults("during-reply=200");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    // Round one, without the resync: the engine keeps composing across the
    // key the client stopped waiting for, so that key is in the next reply.
    assert_eq!(preedit_of(&send_char(&mut client, session, 'k')), "k");
    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "か");
    abandon(&mut client, session, 'i');
    assert_eq!(
        preedit_of(&send_char(&mut client, session, 'u')),
        "かいう",
        "the engine applied the abandoned key; only its reply was lost"
    );
    revert(&mut client, session);

    // Round two, with it.
    assert_eq!(preedit_of(&send_char(&mut client, session, 'k')), "k");
    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "か");
    abandon(&mut client, session, 'i');
    revert(&mut client, session);
    assert_eq!(
        preedit_of(&send_char(&mut client, session, 'u')),
        "う",
        "the Revert emptied the reading rather than trimming it to what the host saw"
    );

    // Both rounds were stalled on every key reply, so neither result came
    // from an engine that was never actually held still.
    let status = entry(&fault_status(&mut client), FaultPoint::DuringReply);
    assert_eq!(status.fired, 8, "{status:?}");
    assert!(status.slept_us >= 8 * 195_000, "{status:?}");

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// The logon task starts the engine below normal (#322). The engine must have
/// left that class before it can be sent a key, or every normal-priority
/// thread on the machine runs ahead of the user's keystroke.
///
/// The engine changes its class before it creates its pipe, so a client that
/// has connected is already talking to an engine at its final class. No sleep
/// or poll is needed to observe it.
#[test]
fn an_engine_started_below_normal_serves_keys_above_normal() {
    let mut engine = Engine::spawn_isolated_below_normal();
    let mut client = engine.client();
    handshake(&mut client);

    assert_eq!(
        engine.priority_class(),
        ABOVE_NORMAL_PRIORITY_CLASS.0,
        "the engine still runs at the class its launcher gave it (0x{:X}); \
         below normal is 0x{:X}",
        engine.priority_class(),
        BELOW_NORMAL_PRIORITY_CLASS.0
    );
    let session = session_for(&mut client, "priority.exe");
    assert_eq!(preedit_of(&send_char(&mut client, session, 'a')), "あ");

    // The change is diagnosable after the fact, from the owned profile's
    // engine log, including what the launcher had handed over.
    let log = std::fs::read_to_string(
        engine
            .local_app_data()
            .join("SakuraInput")
            .join("logs")
            .join("engine.log"),
    )
    .expect("the engine writes its log under the owned profile");
    assert!(
        log.contains("\tevent=priority\tinherited=below_normal\tapplied=above_normal\n"),
        "engine.log does not record the priority change:\n{log}"
    );

    drop(client);
    engine.cleanup().expect("clean shutdown");
}

/// What a saturated CPU does to a key round trip at each engine class.
///
/// This is the measurement behind `process_priority::ENGINE_CLASS`
/// (`verification/high-load-input-integrity.md` §5.6), not a pass/fail gate:
/// the numbers depend on the machine, and a timing threshold on a shared
/// runner fails for reasons unrelated to the code. It saturates every logical
/// processor of the machine it runs on for about a minute, so it runs only on
/// purpose:
///
/// ```text
/// cargo test -p sakura-engine --release --test high_load_key_integrity -- --ignored --nocapture
/// ```
///
/// The load is `2 × logical processors` busy threads at normal priority,
/// which is what a parallel build looks like to the scheduler. The client
/// thread runs at `THREAD_PRIORITY_HIGHEST` (base 10 in a normal process):
/// that is where a foreground UI thread stands after the +2 boost Windows
/// gives it for keyboard input, and it keeps the client's own starvation out
/// of a number meant to describe the engine's.
#[test]
#[ignore = "saturates every logical processor; run on purpose with --release --nocapture"]
fn key_round_trips_under_a_saturated_cpu_by_engine_priority_class() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const KEYS_PER_RUN: usize = 200;
    const RUN_WALL_LIMIT: Duration = Duration::from_secs(8);
    const ROUNDS: usize = 3;
    // Longer than `PATIENT`: below normal under saturation can wait for the
    // balance-set manager's anti-starvation boost, which takes seconds.
    const STARVED: Duration = Duration::from_secs(30);
    let classes = [
        ("below_normal", BELOW_NORMAL_PRIORITY_CLASS),
        ("normal", NORMAL_PRIORITY_CLASS),
        ("above_normal", ABOVE_NORMAL_PRIORITY_CLASS),
    ];

    // Started at normal: every run below sets the class it measures, and a
    // start below normal only adds a slower startup that nothing here reads.
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "saturated.exe");

    // SAFETY: the pseudo-handle names the calling thread and needs no close.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: as above.
    let previous = unsafe { GetThreadPriority(thread) };
    // SAFETY: as above.
    unsafe { SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST) }.expect("raise the client thread");

    // One run: type romaji, cancel every eight keys so the reading stays
    // short, and time each round trip from the client.
    let run = |client: &mut Client| -> Vec<Duration> {
        let started = Instant::now();
        let mut samples = Vec::with_capacity(KEYS_PER_RUN);
        for (index, character) in "kannjihennkannsakura"
            .chars()
            .cycle()
            .take(KEYS_PER_RUN)
            .enumerate()
        {
            if started.elapsed() >= RUN_WALL_LIMIT {
                break;
            }
            let sent = Instant::now();
            let answer = client.call(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                STARVED,
            );
            samples.push(sent.elapsed());
            assert!(
                matches!(answer, Ok(Response::Output(_))),
                "a starved engine still answers within {STARVED:?}: {answer:?}"
            );
            if index % 8 == 7 {
                let answer = client.call(
                    &Request::SendKey {
                        session,
                        key: named_key(KeyCode::Escape),
                    },
                    STARVED,
                );
                assert!(matches!(answer, Ok(Response::Output(_))), "{answer:?}");
            }
        }
        samples
    };
    let report = |label: &str, samples: &mut Vec<Duration>| {
        samples.sort();
        let at = |q: f64| {
            samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)].as_micros()
        };
        let over = samples.iter().filter(|s| **s > TSF_KEY_BUDGET).count();
        println!(
            "{label:<24} keys {:>4}  p50 {:>7} us  p95 {:>7} us  p99 {:>7} us  max {:>8} us  over-50ms {:>4}",
            samples.len(),
            at(0.50),
            at(0.95),
            at(0.99),
            samples.last().map_or(0, Duration::as_micros),
            over
        );
    };

    // Unloaded baseline at each class, after one warm-up run.
    run(&mut client);
    for (name, class) in classes {
        engine.set_priority_class(class);
        report(&format!("idle {name}"), &mut run(&mut client));
    }

    /// Stops and joins the load on every exit, including a panic, so a
    /// failed run never leaves the machine saturated.
    struct Load(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
    impl Drop for Load {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            for worker in self.1.drain(..) {
                let _ = worker.join();
            }
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let workers = 2 * std::thread::available_parallelism().map_or(8, usize::from);
    let load: Vec<_> = (0..workers)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut spin = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    spin = std::hint::black_box(spin.wrapping_add(1));
                }
            })
        })
        .collect();
    let load = Load(stop, load);
    println!("load: {workers} busy threads at normal priority");

    // Interleaved rounds, so drift in the machine's state lands on every
    // class rather than on whichever ran last.
    let mut loaded: Vec<Vec<Duration>> = vec![Vec::new(); classes.len()];
    for _ in 0..ROUNDS {
        for (slot, (_, class)) in classes.iter().enumerate() {
            engine.set_priority_class(*class);
            loaded[slot].extend(run(&mut client));
        }
    }

    drop(load);
    // SAFETY: as above.
    unsafe { SetThreadPriority(thread, THREAD_PRIORITY(previous)) }
        .expect("restore the client thread");

    for ((name, _), samples) in classes.iter().zip(loaded.iter_mut()) {
        report(&format!("saturated {name}"), samples);
    }

    drop(client);
    engine.cleanup().expect("clean shutdown");
}
