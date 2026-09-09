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

#[allow(dead_code)]
mod common;

use std::time::{Duration, Instant};

use sakura_ipc::{Client, Fault};
use sakura_proto::{
    FaultInjectionEntry, FaultPoint, KeyCode, Request, Response, SessionId, PROTOCOL_VERSION,
};

use common::{char_key, named_key, session_for, visible, Engine, PATIENT};

/// What the TSF key path allows one key before it gives up
/// (`sakura_tsf::engine::KEY_BUDGET`). Reproduced rather than imported: this
/// crate must not depend on the DLL, and the number's role here is to be a
/// client that abandons a key, not to track that constant.
const TSF_KEY_BUDGET: Duration = Duration::from_millis(50);

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

/// The abandoned-key ordering. The engine has applied the key and produced
/// the answer; only delivery is late, and the client gives up first.
///
/// What this pins down is that the *engine* loses nothing, and that the late
/// answer never lands on the next request: the client that resumed sees its
/// own reply, containing both keys. Whether the host ever displays the first
/// one is a TSF-layer question and is not settled here.
#[test]
fn an_abandoned_reply_is_never_delivered_as_the_answer_to_the_next_key() {
    let mut engine = Engine::spawn_isolated_with_faults("during-reply=400");
    let mut client = engine.client();
    handshake(&mut client);
    let session = session_for(&mut client, "high-load.exe");

    let abandoned = client.call(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        TSF_KEY_BUDGET,
    );
    assert!(
        matches!(abandoned, Err(Fault::Timeout | Fault::DeadlineExpired)),
        "a 50 ms client should not have outlasted a 400 ms stall: {abandoned:?}"
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
