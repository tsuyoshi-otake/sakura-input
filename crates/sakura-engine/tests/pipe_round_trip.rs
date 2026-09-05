//! The engine answers a real client over a private test-owned pipe.
//!
//! Every other test in this workspace stops one layer short of this. The
//! dispatcher's unit tests call `dispatch` directly; `sakura-ipc`'s tests put
//! a server and a client on a scratch pipe name; the text service's tests
//! talk to a scripted fake engine. This test exercises the built
//! `sakura_engine.exe` in a separate process, but gives that process a unique
//! explicit test pipe and owned profile. It never resolves or touches the
//! installed engine's well-known pipe, so real-process test binaries may run
//! concurrently.
//!
//! That gap is where the startup crash of the boxed-session-table fix lived —
//! the engine failed before it accepted its first connection, and a fully
//! green `cargo test` said nothing about it, because nothing in the suite had
//! ever started the binary.
//!
//! The sequence below is deliberately the DLL's, in the DLL's order (see
//! `sakura-tsf`'s `engine::open`): connect, `Hello`, `CreateSession`, then
//! keystrokes. If this passes and the text service still cannot type, the
//! fault is in the text service, not in the protocol or the transport.

#[allow(dead_code)]
mod common;

use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_ipc::Client;
use sakura_proto::{
    InputScope, KeyCode, KeyInput, Mode, Modifiers, Request, Response, SessionId, PROTOCOL_VERSION,
};

use common::{char_key, named_key, session_for, shifted_char_key, visible, Engine, PATIENT};
use sakura_ime_eval::capture_engine::capture_candidates;
use sakura_ime_eval::types::{Constraints, Context, Input, SemanticCase};

#[test]
fn duplicate_engine_is_rejected_before_dictionary_initialization() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let mut engine = Engine::spawn_isolated();
    drop(engine.client());
    let missing = engine
        .local_app_data()
        .join("deliberately-absent-dictionary.bin");
    let mut duplicate = OwnedChild(
        std::process::Command::new(env!("CARGO_BIN_EXE_sakura_engine"))
            .arg("--test-pipe")
            .arg(engine.pipe_name())
            .env("LOCALAPPDATA", engine.local_app_data())
            .env("SAKURA_DICTIONARY", missing)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + PATIENT;
    let status = loop {
        if let Some(status) = duplicate.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "duplicate engine did not terminate"
        );
        sleep(Duration::from_millis(10));
    };
    drop(duplicate);
    engine.cleanup().unwrap();
    assert_eq!(
        status.code(),
        Some(2),
        "duplicate touched dictionary initialization before claiming ownership"
    );
}

fn publish_test_configuration(engine: &Engine, source: &str) {
    let path = engine
        .local_app_data()
        .join("SakuraInput")
        .join("config")
        .join("config.toml");
    fs::create_dir_all(path.parent().expect("configuration parent")).expect("config directory");
    let temporary = path.with_extension("e2e.tmp");
    fs::write(&temporary, source).expect("write complete temporary configuration");
    fs::rename(&temporary, &path).expect("publish complete configuration atomically");
}

fn assert_shifted_term(client: &mut Client, session: SessionId, typed: &str, expected: &[&str]) {
    let mut shifted_preedit = String::new();
    for character in typed.chars() {
        match client.call(
            &Request::SendKey {
                session,
                key: shifted_char_key(character),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                assert!(
                    output.consumed,
                    "Shift+{character} in {typed} must stay in English composition"
                );
                shifted_preedit = visible(output.preedit);
            }
            other => panic!("Shift+{character} in {typed}: expected Output, got {other:?}"),
        }
    }
    assert_eq!(shifted_preedit, typed);

    let converted = match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert!(output.consumed);
            let converted = visible(output.preedit);
            assert!(
                expected.contains(&converted.as_str()),
                "Henkan after Shift+{typed}: expected one of {expected:?}, got {converted:?}"
            );
            converted
        }
        other => panic!("Henkan after Shift+{typed}: expected Output, got {other:?}"),
    };
    match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some(converted.as_str()))
        }
        other => panic!("Enter after Shift+{typed}: expected Output, got {other:?}"),
    }
}

/// The whole M0 story across a real pipe: the engine starts, accepts the
/// handshake, opens a session, verifies stateful composition and UI behavior,
/// then verifies isolated durable English learning.
///
/// The harness owns a unique pipe and process, so concurrent integration test
/// binaries cannot interfere with this test or a user's installed engine.
#[test]
fn a_real_engine_serves_a_real_client_over_an_owned_private_pipe() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();

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

    let session = session_for(&mut client, "pipe_round_trip.exe");

    let mut preedit = String::new();
    for c in "sakura".chars() {
        match client.call(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                assert!(output.consumed, "the engine must claim a composing key");
                preedit = visible(output.preedit);
            }
            other => panic!("SendKey {c:?}: expected Output, got {other:?}"),
        }
    }
    assert_eq!(preedit, "さくら", "romaji did not survive the round trip");

    match client.call(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some("さくら"));
            assert!(
                output.preedit.is_none_or(|p| p.segments.is_empty()),
                "committing must leave nothing composing"
            );
        }
        other => panic!("Enter: expected Output, got {other:?}"),
    }

    // A second connection, which is what focusing a second application
    // does. It has to be served at all — the pool must grow past the
    // instance the engine started with — and the first client hanging up
    // must not have taken the engine with it.
    drop(client);
    let mut next = engine.client();
    match next.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        PATIENT,
    ) {
        Ok(Response::Hello { .. }) => {}
        other => panic!("second connection: expected Hello, got {other:?}"),
    }
    let next_session = session_for(&mut next, "second.exe");

    // Deliberately not asserting this id differs from the first. Sessions
    // are numbered by the `Dispatcher` that owns them, and per the
    // share-nothing design in `server`'s module docs each connection gets
    // its own — so id 1 on two connections is two unrelated sessions, and
    // that is fine precisely because a `SessionId` is only ever interpreted
    // against the connection it arrived on.
    match next.call(
        &Request::SendKey {
            session: next_session,
            key: char_key('a'),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(visible(output.preedit), "あ");
        }
        other => panic!("expected Output on the second connection, got {other:?}"),
    }

    // 無変換 while composing is deliberately a temporary kana transform and
    // must not publish a persistent mode change. Commit the preedit first so
    // the mode-key assertion below exercises the idle `mode_kana_cycle` path
    // that the renderer's shared UI state is meant to observe.
    match next.call(
        &Request::SendKey {
            session: next_session,
            key: named_key(KeyCode::Enter),
        },
        PATIENT,
    ) {
        Ok(Response::Output(output)) => {
            assert_eq!(output.commit.as_deref(), Some("あ"));
        }
        other => panic!("Enter on the second connection: expected Output, got {other:?}"),
    }

    // Real-pipe tests intentionally run against the durable learning store.
    // A user who previously selected Claude Code should keep that Microsoft
    // IME-style preference, while a fresh profile still ranks Claude first.
    assert_shifted_term(
        &mut next,
        next_session,
        "CLAUDE",
        &["Claude", "Claude Code"],
    );
    assert_shifted_term(&mut next, next_session, "OPENAI", &["OpenAI"]);
    assert_shifted_term(&mut next, next_session, "GITLAB", &["GitLab"]);
    assert_shifted_term(&mut next, next_session, "PYTORCH", &["PyTorch"]);

    // These and the stateful composition/UI keys above deliberately remain
    // non-test-only: this test verifies retained engine state. Every write is
    // confined to this test's owned profile, never the user's LOCALAPPDATA.
    let learning_path = engine
        .local_app_data()
        .join("SakuraInput")
        .join("learning")
        .join("log.bin");
    let learning = sakura_engine::learning::read_snapshot(&learning_path).unwrap_or_else(|error| {
        panic!(
            "read isolated learning log {}: {error}",
            learning_path.display()
        )
    });
    assert!(
        learning
            .records
            .iter()
            .any(|record| record.surface == "Claude" || record.surface == "Claude Code"),
        "non-test-only English commit did not persist in isolated learning log {}: {learning:?}",
        learning_path.display()
    );

    // What the renderer does. `since: 0` is nobody's revision, so this is
    // answered from the engine's current state without blocking.
    let mut renderer = engine.client();
    let seen = match renderer.call(&Request::WatchUi { since: 0 }, PATIENT) {
        Ok(Response::Ui(state)) => state,
        other => panic!("WatchUi: expected Ui, got {other:?}"),
    };

    // A mode key on the typing connection has to reach the watcher on the
    // renderer's connection. Cycle until the session reports a mode different
    // from the observed board; within three states that must publish a change.
    let mut changed_mode = None;
    for _ in 0..3 {
        match next.call(
            &Request::SendKey {
                session: next_session,
                key: named_key(KeyCode::Muhenkan),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                let mode = output.mode.expect("idle Muhenkan must report the new mode");
                if Some(mode) != seen.mode {
                    changed_mode = Some(mode);
                    break;
                }
            }
            other => panic!("Muhenkan: expected Output, got {other:?}"),
        }
    }
    assert!(
        changed_mode.is_some(),
        "the three-state mode cycle must differ from the observed UI mode"
    );

    match renderer.call(
        &Request::WatchUi {
            since: seen.revision,
        },
        PATIENT,
    ) {
        Ok(Response::Ui(state)) => {
            assert_ne!(
                state.revision, seen.revision,
                "the mode change did not reach the UI"
            );
            assert!(state.mode.is_some(), "a mode change must name a mode");
        }
        other => panic!("WatchUi after a mode change: expected Ui, got {other:?}"),
    }

    drop(renderer);
    drop(next);
    let cleanup = engine.cleanup().expect("owned engine cleanup must succeed");
    assert!(
        cleanup.status.success(),
        "owned engine pid {} exited with {}",
        cleanup.pid,
        cleanup.status
    );
}

/// A settings save must cross the same boundary as a real TSF request: the
/// running engine accepts the new complete snapshot, applies its Normalizer at
/// a request boundary, and the next output reflects the changed width,
/// punctuation, and bracket style. This deliberately uses a separate engine
/// process and its
/// private pipe; a direct `Dispatcher` call would not prove the watcher path.
#[test]
fn a_running_engine_applies_saved_width_punctuation_and_brackets_to_real_output() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let session = session_for(&mut client, "settings-runtime-e2e.exe");

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
keymap-preset = "ms-ime"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "full"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
brackets = "corner"
"#,
    );

    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));

    let width_deadline = Instant::now() + PATIENT;
    let mut width_output = None;
    let mut width_last = String::new();
    while Instant::now() < width_deadline {
        let _ = client.call(&Request::Revert { session }, PATIENT);
        let mode_reply = client.call(
            &Request::SetMode {
                session,
                mode: Mode::FullAlnum,
            },
            PATIENT,
        );
        if let Ok(Response::Output(output)) = client.call(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            PATIENT,
        ) {
            width_last = format!("mode={mode_reply:?}, output={output:?}");
            if output.commit.as_deref() == Some("ａ") {
                width_output = output.commit;
                break;
            }
        }
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        width_output.as_deref(),
        Some("ａ"),
        "runtime width did not reach the output: {width_last}"
    );

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
keymap-preset = "ms-ime"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "comma-kuten"
brackets = "square"
"#,
    );

    let punctuation_session = session_for(&mut client, "settings-punctuation-e2e.exe");
    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session: punctuation_session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));
    let punctuation_deadline = Instant::now() + PATIENT;
    let mut punctuation_output = None;
    while Instant::now() < punctuation_deadline {
        let _ = client.call(
            &Request::SetMode {
                session: punctuation_session,
                mode: Mode::Hiragana,
            },
            PATIENT,
        );
        let _ = client.call(
            &Request::SendKey {
                session: punctuation_session,
                key: char_key('a'),
            },
            PATIENT,
        );
        if let Ok(Response::Output(output)) = client.call(
            &Request::SendKey {
                session: punctuation_session,
                key: char_key('.'),
            },
            PATIENT,
        ) {
            let text = visible(output.preedit);
            if text == "あ。" {
                punctuation_output = Some(text);
                break;
            }
        }
        let _ = client.call(
            &Request::Revert {
                session: punctuation_session,
            },
            PATIENT,
        );
        sleep(Duration::from_millis(25));
    }
    assert_eq!(punctuation_output.as_deref(), Some("あ。"));

    let _ = client.call(
        &Request::Revert {
            session: punctuation_session,
        },
        PATIENT,
    );
    let mut comma_output = None;
    let comma_deadline = Instant::now() + PATIENT;
    while Instant::now() < comma_deadline {
        let _ = client.call(
            &Request::SetMode {
                session: punctuation_session,
                mode: Mode::Hiragana,
            },
            PATIENT,
        );
        let _ = client.call(
            &Request::SendKey {
                session: punctuation_session,
                key: char_key('a'),
            },
            PATIENT,
        );
        if let Ok(Response::Output(output)) = client.call(
            &Request::SendKey {
                session: punctuation_session,
                key: char_key(','),
            },
            PATIENT,
        ) {
            let text = visible(output.preedit);
            if text == "あ，" {
                comma_output = Some(text);
                break;
            }
        }
        let _ = client.call(
            &Request::Revert {
                session: punctuation_session,
            },
            PATIENT,
        );
        sleep(Duration::from_millis(25));
    }
    assert_eq!(comma_output.as_deref(), Some("あ，"));

    let bracket_session = session_for(&mut client, "settings-bracket-e2e.exe");
    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session: bracket_session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));
    let bracket_deadline = Instant::now() + PATIENT;
    let mut bracket_output = None;
    let mut bracket_last = String::new();
    while Instant::now() < bracket_deadline {
        let _ = client.call(
            &Request::Revert {
                session: bracket_session,
            },
            PATIENT,
        );
        let mode_reply = client.call(
            &Request::SetMode {
                session: bracket_session,
                mode: Mode::FullAlnum,
            },
            PATIENT,
        );
        if let Ok(Response::Output(output)) = client.call(
            &Request::SendKey {
                session: bracket_session,
                key: char_key('['),
            },
            PATIENT,
        ) {
            bracket_last = format!("mode={mode_reply:?}, output={output:?}");
            if output.commit.as_deref() == Some("\u{ff3b}") {
                bracket_output = output.commit;
                break;
            }
        }
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        bracket_output.as_deref(),
        Some("\u{ff3b}"),
        "runtime bracket style did not reach the FullAlnum output: {bracket_last}"
    );

    drop(client);
    let cleanup = engine.cleanup().expect("owned engine cleanup");
    assert!(
        cleanup.status.success(),
        "engine exited with {}",
        cleanup.status
    );
}

/// The quality runner must exercise the same real engine binary as the
/// ordinary pipe tests, not a dispatcher double or the user's ambient pipe.
#[test]
fn real_engine_candidate_capture_round_trip() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sakura-ime-eval-capture");
    fs::create_dir_all(&root).expect("create capture fixture root");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let profile = root.join(format!("fixture-{}-{nonce:x}", std::process::id()));
    fs::create_dir(&profile).expect("create capture fixture profile");
    let dictionary = common::test_dictionary(&profile);
    let temp_root = root.join(format!("runner-{}-{nonce:x}", std::process::id()));
    let case = SemanticCase {
        schema_version: 1,
        case_id: "real-capture-kyou".to_owned(),
        task: "conversion".to_owned(),
        family: Some("normal-conversion".to_owned()),
        role: Some("positive".to_owned()),
        context: Context {
            left: "今日は".to_owned(),
            right: "晴れ".to_owned(),
        },
        input: Input {
            input_mode: Some("romaji".to_owned()),
            reading: "きょう".to_owned(),
            typing: Some("kyou".to_owned()),
        },
        constraints: Constraints::default(),
        privacy_provenance: None,
    };
    let engine = PathBuf::from(env!("CARGO_BIN_EXE_sakura_engine"));
    let result = capture_candidates(&engine, &dictionary, &[case], &temp_root, PATIENT);
    let _ = fs::remove_dir_all(&profile);
    let _ = fs::remove_dir_all(&temp_root);
    let outputs = result.expect("real capture must complete");
    assert_eq!(outputs.len(), 1);
    assert!(
        outputs[0]
            .candidates
            .iter()
            .any(|candidate| candidate == "今日"),
        "fixture candidate missing from real capture: {:?}",
        outputs[0].candidates
    );
}

/// Space-width settings must cross the settings file watcher and the real
/// engine process, while Shift+Space remains a separate policy.
#[test]
fn a_running_engine_applies_saved_space_width_and_shift_space_policy() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let session = session_for(&mut client, "settings-space-width-e2e.exe");
    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
space-width = "half"
shift-space = "full"
association-enabled = "true"
neural-reranker-scope = "off"

[appearance]
theme = "auto"
"#,
    );

    let deadline = Instant::now() + PATIENT;
    let mut ordinary = None;
    let mut last = String::new();
    while Instant::now() < deadline {
        match client.call(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                last = format!("{output:?}");
                if output.commit.as_deref() == Some(" ") {
                    ordinary = output.commit;
                    break;
                }
            }
            other => last = format!("{other:?}"),
        }
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        ordinary.as_deref(),
        Some(" "),
        "space setting did not apply: {last}"
    );

    let shifted = KeyInput {
        modifiers: Modifiers::SHIFT,
        ..named_key(KeyCode::Space)
    };
    let shifted_output = client
        .call(
            &Request::SendKey {
                session,
                key: shifted,
            },
            PATIENT,
        )
        .expect("shifted Space response");
    match shifted_output {
        Response::Output(output) => assert_eq!(output.commit.as_deref(), Some("　")),
        other => panic!("shifted Space returned {other:?}"),
    }
}

/// A settings save must also change the input method used by a live session.
/// The test sends the same ASCII layout character before and after the
/// configuration watcher observes `input-method = "kana"`: Romaji emits the
/// table's hiragana syllable, while Kana accepts the layout character itself.
#[test]
fn a_running_engine_applies_saved_input_method_to_real_output() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let session = session_for(&mut client, "settings-input-method-e2e.exe");
    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
input-method = "kana"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
"#,
    );

    let deadline = Instant::now() + PATIENT;
    let mut kana_output = None;
    while Instant::now() < deadline {
        let _ = client.call(&Request::Revert { session }, PATIENT);
        let _ = client.call(
            &Request::SetMode {
                session,
                mode: Mode::Hiragana,
            },
            PATIENT,
        );
        if let Ok(Response::Output(output)) = client.call(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            PATIENT,
        ) {
            let text = visible(output.preedit);
            if text == "a" {
                kana_output = Some(text);
                break;
            }
        }
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        kana_output.as_deref(),
        Some("a"),
        "the live engine did not accept the layout character directly after saving Kana input"
    );

    drop(client);
    let cleanup = engine.cleanup().expect("owned engine cleanup");
    assert!(
        cleanup.status.success(),
        "engine exited with {}",
        cleanup.status
    );
}

/// A saved global character type must be used when a new input context is
/// created. Existing contexts intentionally retain their current mode, while
/// the next context receives the configured ATOK-like default and renders real
/// kana through the ordinary romaji path.
#[test]
fn a_running_engine_applies_saved_default_character_type_to_new_sessions() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let original = session_for(&mut client, "settings-default-mode-before.exe");
    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session: original,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
input-method = "romaji"
default-mode = "katakana"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
"#,
    );

    let deadline = Instant::now() + PATIENT;
    let mut configured = None;
    while Instant::now() < deadline {
        let candidate = match client.call(
            &Request::CreateSession {
                process_name: "settings-default-mode-after.exe".to_owned(),
            },
            PATIENT,
        ) {
            Ok(Response::SessionCreated { session, mode }) => {
                if mode == Mode::Katakana {
                    Some(session)
                } else {
                    let _ = client.call(&Request::DeleteSession { session }, PATIENT);
                    None
                }
            }
            other => panic!(
                "CreateSession after default-mode save: expected SessionCreated, got {other:?}"
            ),
        };
        if let Some(session) = candidate {
            configured = Some(session);
            break;
        }
        sleep(Duration::from_millis(25));
    }
    let session =
        configured.expect("configuration watcher applied the saved default character type");

    let mut preedit = String::new();
    for character in "ka".chars() {
        match client.call(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => {
                assert!(
                    output.consumed,
                    "romaji key must be consumed in Katakana mode"
                );
                preedit = visible(output.preedit);
            }
            other => panic!("SendKey {character:?}: expected Output, got {other:?}"),
        }
    }
    assert_eq!(
        preedit, "カ",
        "saved character type must affect real conversion output"
    );

    drop(client);
    let cleanup = engine.cleanup().expect("owned engine cleanup");
    assert!(
        cleanup.status.success(),
        "engine exited with {}",
        cleanup.status
    );
}

/// The conversion-method preference is a construction policy, not a renderer
/// filter.  The same live engine session must produce a two-edge lattice under
/// multi-segment mode and a one-edge lattice after the saved single-segment
/// policy is observed.
#[test]
fn a_running_engine_applies_saved_conversion_method_to_real_candidates() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let session = session_for(&mut client, "settings-conversion-method-e2e.exe");
    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));

    fn converted_segment_count(
        client: &mut sakura_ipc::Client,
        session: SessionId,
    ) -> Option<usize> {
        let _ = client.call(&Request::Revert { session }, PATIENT);
        let _ = client.call(
            &Request::SetMode {
                session,
                mode: Mode::Hiragana,
            },
            PATIENT,
        );
        for character in "kyouha".chars() {
            let Ok(Response::Output(output)) = client.call(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                PATIENT,
            ) else {
                return None;
            };
            if !output.consumed {
                return None;
            }
        }
        match client.call(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            PATIENT,
        ) {
            Ok(Response::Output(output)) => output.preedit.map(|preedit| preedit.segments.len()),
            _ => None,
        }
    }

    fn wait_for_segments(
        engine: &Engine,
        client: &mut sakura_ipc::Client,
        session: SessionId,
        method: &str,
        expected: usize,
    ) {
        publish_test_configuration(
            engine,
            &format!(
                "[meta]\nformat-version = \"4\"\n\n[input]\ninput-method = \"romaji\"\nconversion-method = \"{method}\"\nprediction-enabled = \"false\"\nsuggest-accept = \"tab\"\nassociation-enabled = \"true\"\nneural-reranker-scope = \"off\"\ndeveloper-mode = \"false\"\n\n[appearance]\ntheme = \"auto\"\n\n[width]\nalnum = \"half\"\nnumber = \"half\"\nsymbol = \"half\"\npunctuation = \"kuten-touten\"\n"
            ),
        );
        let deadline = Instant::now() + PATIENT;
        let mut observed = None;
        while Instant::now() < deadline {
            if let Some(count) = converted_segment_count(client, session) {
                if count == expected {
                    observed = Some(count);
                    break;
                }
            }
            sleep(Duration::from_millis(25));
        }
        assert_eq!(
            observed,
            Some(expected),
            "saved conversion method {method:?} did not produce {expected} segment(s)"
        );
    }

    wait_for_segments(&engine, &mut client, session, "single-segment", 1);
    wait_for_segments(&engine, &mut client, session, "multi-segment", 2);

    drop(client);
    let cleanup = engine.cleanup().expect("owned engine cleanup");
    assert!(
        cleanup.status.success(),
        "engine exited with {}",
        cleanup.status
    );
}

/// Developer-mode must hot-attach history on a live engine: after the settings
/// watcher publishes ON, the next request boundary reports
/// `InputHistoryStats.active`, and a Normal key becomes durable without an
/// engine restart.
#[test]
fn live_engine_hot_enables_developer_history_and_records_a_normal_key() {
    let mut engine = Engine::spawn_isolated();
    let mut client = engine.client();
    let session = session_for(&mut client, "developer-history-hot.exe");

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
keymap-preset = "ms-ime"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
"#,
    );

    assert!(matches!(
        client.call(&Request::InputHistoryStats, PATIENT),
        Ok(Response::InputHistoryStats { active: false, .. })
    ));

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
keymap-preset = "ms-ime"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "true"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
"#,
    );

    let deadline = Instant::now() + PATIENT;
    let mut active = false;
    while Instant::now() < deadline {
        match client.call(&Request::InputHistoryStats, PATIENT) {
            Ok(Response::InputHistoryStats { active: true, .. }) => {
                active = true;
                break;
            }
            Ok(Response::InputHistoryStats { active: false, .. }) => {
                sleep(Duration::from_millis(50));
            }
            other => panic!("InputHistoryStats: {other:?}"),
        }
    }
    assert!(
        active,
        "developer-mode ON must attach history without restarting the engine"
    );

    assert!(matches!(
        client.call(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            PATIENT,
        ),
        Ok(Response::Ok)
    ));
    assert!(matches!(
        client.call(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            PATIENT,
        ),
        Ok(Response::Output(_))
    ));
    assert!(matches!(
        client.call(&Request::FlushInputHistory, PATIENT),
        Ok(Response::Ok)
    ));

    let history_path = engine
        .local_app_data()
        .join("SakuraInput")
        .join("history")
        .join("input.bin");
    let snapshot = sakura_engine::input_history::read_snapshot(&history_path)
        .expect("durable history snapshot after hot-enable");
    assert!(
        !snapshot.records.is_empty(),
        "Normal key after hot-enable must leave at least one durable record"
    );

    publish_test_configuration(
        &engine,
        r#"[meta]
format-version = "4"

[input]
keymap-preset = "ms-ime"
prediction-enabled = "false"
suggest-accept = "tab"
association-enabled = "true"
neural-reranker-scope = "off"
developer-mode = "false"

[appearance]
theme = "auto"

[width]
alnum = "half"
number = "half"
symbol = "half"
punctuation = "kuten-touten"
"#,
    );

    let deadline = Instant::now() + PATIENT;
    let mut inactive = false;
    while Instant::now() < deadline {
        match client.call(&Request::InputHistoryStats, PATIENT) {
            Ok(Response::InputHistoryStats { active: false, .. }) => {
                inactive = true;
                break;
            }
            Ok(Response::InputHistoryStats { active: true, .. }) => {
                sleep(Duration::from_millis(50));
            }
            other => panic!("InputHistoryStats after disable: {other:?}"),
        }
    }
    assert!(
        inactive,
        "developer-mode OFF must detach history at a request boundary"
    );

    let before = snapshot.records.len();
    assert!(matches!(
        client.call(
            &Request::SendKey {
                session,
                key: char_key('b'),
            },
            PATIENT,
        ),
        Ok(Response::Output(_))
    ));
    assert!(matches!(
        client.call(&Request::FlushInputHistory, PATIENT),
        Ok(Response::Ok)
    ));
    if history_path.exists() {
        let after = sakura_engine::input_history::read_snapshot(&history_path)
            .expect("snapshot after detach")
            .records
            .len();
        assert_eq!(after, before, "detached engine must not record new keys");
    }

    assert!(matches!(
        client.call(&Request::ClearInputHistory, PATIENT),
        Ok(Response::Ok)
    ));

    drop(client);
    let cleanup = engine.cleanup().expect("owned engine cleanup");
    assert!(
        cleanup.status.success(),
        "engine exited with {}",
        cleanup.status
    );
}
