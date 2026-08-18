use std::fs;
use std::sync::Arc;

use sakura_proto::{
    InputScope, KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response, SessionId,
};

use crate::composition_fence::CompositionFence;
use crate::dictionary::ConversionService;
use crate::dispatch::{take_conversion_lookup_count_for_test, Dispatcher, Reply};
use crate::space_key_dispatch_oracle::{
    apply, apply_all, no_dual_effect, ConnState, DomainEvent, OracleState,
};

fn char_key(character: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(character),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn space_key() -> KeyInput {
    KeyInput {
        code: KeyCode::Space,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
    match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: name.to_string(),
        },
        out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected session, got {other:?}"),
    }
}

fn send(dispatcher: &mut Dispatcher, session: SessionId, key: KeyInput, out: &mut OutputBuf) {
    assert_eq!(
        dispatcher.dispatch(&Request::SendKey { session, key }, out),
        Reply::Output
    );
}

fn probe_space(dispatcher: &mut Dispatcher, session: SessionId, out: &mut OutputBuf) {
    assert_eq!(
        dispatcher.dispatch(
            &Request::ProbeKey {
                session,
                scope: InputScope::Normal,
                fresh_context: false,
                key: KeyInput {
                    test_only: true,
                    ..space_key()
                },
            },
            out,
        ),
        Reply::Output
    );
}

fn dispatcher_with_probe_conversion_fixture() -> Dispatcher {
    let source = concat!(
        "# license: MIT\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "\u{304b}\u{306a}\t\u{4eee}\u{540d}\t0\t0\t100\t100\t\tprobe fixture\n",
    );
    let entries = dictc::parse_entries("probe-conversion.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "probe-conversion-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("dictionary image")
            .into_boxed_slice(),
    );
    let conversion =
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"));
    Dispatcher::new_with_conversion(conversion).expect("dispatcher")
}

struct ProductionWorld {
    fence: Arc<CompositionFence>,
    workers: Vec<Dispatcher>,
    sessions: Vec<SessionId>,
    live: Vec<bool>,
    document_spaces: u32,
    conversions: u32,
    last_inserted: bool,
    last_converted: bool,
}

impl ProductionWorld {
    fn new(actors: usize) -> Self {
        let fence = Arc::new(CompositionFence::new());
        let mut workers = Vec::new();
        let mut sessions = Vec::new();
        let mut out = OutputBuf::new();
        for _ in 0..actors {
            let mut dispatcher = Dispatcher::new().expect("dispatcher");
            dispatcher.set_composition_fence(Arc::clone(&fence));
            // Same host process name: Electron dual-delivery class.
            let session = create_session(&mut dispatcher, &mut out, "space-key-host.exe");
            workers.push(dispatcher);
            sessions.push(session);
        }
        Self {
            fence,
            live: vec![true; actors],
            workers,
            sessions,
            document_spaces: 0,
            conversions: 0,
            last_inserted: false,
            last_converted: false,
        }
    }

    fn apply(&mut self, event: DomainEvent) {
        self.last_inserted = false;
        self.last_converted = false;
        match event {
            DomainEvent::Type { connection } => {
                if let Some((worker, session)) = self.pair(connection) {
                    let mut out = OutputBuf::new();
                    send(worker, session, char_key('a'), &mut out);
                }
            }
            DomainEvent::Space { targets } => self.apply_space(targets),
            DomainEvent::Commit { connection } | DomainEvent::Cancel { connection } => {
                if let Some((worker, session)) = self.pair(connection) {
                    let mut out = OutputBuf::new();
                    let code = if matches!(event, DomainEvent::Commit { .. }) {
                        KeyCode::Enter
                    } else {
                        KeyCode::Escape
                    };
                    send(
                        worker,
                        session,
                        KeyInput {
                            code,
                            ch: None,
                            modifiers: Modifiers::NONE,
                            repeat: false,
                            test_only: false,
                        },
                        &mut out,
                    );
                }
            }
            DomainEvent::ReplaceContext { connection }
            | DomainEvent::CrashRestart { connection } => {
                let index = usize::from(connection);
                if index < self.workers.len() {
                    self.workers[index].reset();
                    let mut out = OutputBuf::new();
                    let mut dispatcher = Dispatcher::new().expect("replacement dispatcher");
                    dispatcher.set_composition_fence(Arc::clone(&self.fence));
                    let session = create_session(&mut dispatcher, &mut out, "space-key-host.exe");
                    self.workers[index] = dispatcher;
                    self.sessions[index] = session;
                    self.live[index] = true;
                }
            }
            DomainEvent::Disconnect { connection } => {
                let index = usize::from(connection);
                if index < self.live.len() {
                    self.live[index] = false;
                }
            }
            DomainEvent::TimeoutSpace { .. } | DomainEvent::DropSpace => {}
        }
    }

    fn apply_space(&mut self, targets: u8) {
        for index in 0..self.workers.len() {
            if (targets & (1 << index)) == 0 || !self.live[index] {
                continue;
            }
            let session = self.sessions[index];
            let mut out = OutputBuf::new();
            send(&mut self.workers[index], session, space_key(), &mut out);
            let commit = out.commit_text().unwrap_or("");
            let committed_space = commit == "\u{3000}" || commit == " ";
            if committed_space {
                self.last_inserted = true;
                self.document_spaces = self.document_spaces.saturating_add(1);
            } else if out.has_candidates()
                || !out.preedit_text().is_empty()
                || looks_converted(out.preedit_text())
            {
                self.last_converted = true;
                self.conversions = self.conversions.saturating_add(1);
            }
        }
    }

    fn pair(&mut self, connection: u8) -> Option<(&mut Dispatcher, SessionId)> {
        let index = usize::from(connection);
        if !*self.live.get(index)? {
            return None;
        }
        let session = *self.sessions.get(index)?;
        Some((self.workers.get_mut(index)?, session))
    }
}

fn looks_converted(preedit: &str) -> bool {
    !preedit.is_empty()
        && preedit
            .chars()
            .any(|character| !is_hiragana_or_romaji(character))
}

fn is_hiragana_or_romaji(character: char) -> bool {
    character.is_ascii_alphabetic() || ('ぁ'..='ゖ').contains(&character) || character == 'ー'
}

#[test]
fn production_single_connection_idle_space_inserts_fullwidth() {
    let mut world = ProductionWorld::new(1);
    world.apply(DomainEvent::Space { targets: 1 });
    assert!(world.last_inserted);
    assert!(!world.last_converted);
    assert_eq!(world.document_spaces, 1);
    let oracle = apply_all(1, [DomainEvent::Space { targets: 1 }]);
    assert_eq!(world.document_spaces, oracle.document_spaces);
}

#[test]
fn production_single_connection_composing_space_converts() {
    let mut dispatcher = Dispatcher::new().expect("dispatcher");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "space-convert.exe");
    send(&mut dispatcher, session, char_key('a'), &mut out);
    assert!(!out.preedit_text().is_empty());
    send(&mut dispatcher, session, space_key(), &mut out);
    assert!(
        out.commit_text().unwrap_or("").is_empty(),
        "composing Space must not commit a document space, got {:?}",
        out.commit_text()
    );
    assert!(
        out.has_candidates() || !out.preedit_text().is_empty(),
        "composing Space must convert or keep preedit"
    );
}

#[test]
fn japanese_test_phrase_space_does_not_insert_a_document_space() {
    let mut dispatcher = Dispatcher::new().expect("dispatcher");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "nihongo-test.exe");
    for character in "nihongonyuuryokunotesuto".chars() {
        send(&mut dispatcher, session, char_key(character), &mut out);
    }
    assert_eq!(out.preedit_text(), "にほんごにゅうりょくのてすと");
    send(&mut dispatcher, session, space_key(), &mut out);
    let commit = out.commit_text().unwrap_or("");
    assert!(
        commit != "\u{3000}" && commit != " ",
        "Space during にほんごにゅうりょくのてすと must convert, not insert a document space, got {commit:?}"
    );
    assert!(
        out.has_candidates() || !out.preedit_text().is_empty() || out.beep,
        "composing Space must convert, keep preedit, or beep"
    );
}

#[test]
fn composing_space_probe_consumes_without_converting_or_inserting() {
    // With no service, an Apply Convert would enter `begin_conversion` and
    // beep. Keeping the probe beep-free remains a behavior-level entrypoint
    // guard; the next test also uses a direct conversion-call spy.
    let mut dispatcher = Dispatcher::new().expect("dispatcher");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "probe-convert.exe");
    for character in "nihongonyuuryoku".chars() {
        send(&mut dispatcher, session, char_key(character), &mut out);
    }
    let preedit_before = out.preedit_text().to_owned();
    assert!(!preedit_before.is_empty());
    probe_space(&mut dispatcher, session, &mut out);
    assert!(
        !out.has_candidates(),
        "Probe Convert must not build a candidate list"
    );
    assert!(out.consumed, "composing Space probe must be eaten");
    assert!(
        !out.beep,
        "Probe must not run dictionary conversion; beep means begin_conversion ran without a table"
    );
    assert_eq!(
        out.commit_text(),
        None,
        "Probe Convert must not commit any document text"
    );
    assert_eq!(out.preedit_text(), preedit_before);
}

#[test]
fn composing_space_probe_with_conversion_service_makes_zero_lookups_and_no_document_commit() {
    let mut dispatcher = dispatcher_with_probe_conversion_fixture();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "probe-convert-service.exe");
    for character in "kana".chars() {
        send(&mut dispatcher, session, char_key(character), &mut out);
    }
    let preedit_before = out.preedit_text().to_owned();
    assert_eq!(preedit_before, "\u{304b}\u{306a}");
    // Scope the per-thread spy to this Probe request only.
    take_conversion_lookup_count_for_test();

    probe_space(&mut dispatcher, session, &mut out);

    assert_eq!(
        take_conversion_lookup_count_for_test(),
        0,
        "Probe Convert must not call ConversionService"
    );
    assert!(out.consumed, "composing Space probe must be eaten");
    assert!(
        !out.beep,
        "Probe Convert must not take the conversion failure path"
    );
    assert!(
        !out.has_candidates(),
        "Probe Convert must not publish candidates"
    );
    assert_eq!(
        out.commit_text(),
        None,
        "Probe Convert must not commit text"
    );
    assert_eq!(out.preedit_text(), preedit_before);

    // The same fixture proves a real Apply still reaches dictionary conversion;
    // the Probe above must therefore have left the live composition untouched.
    send(&mut dispatcher, session, space_key(), &mut out);
    assert!(
        take_conversion_lookup_count_for_test() > 0,
        "the real Apply conversion must hit the service"
    );
    assert!(out.consumed);
    assert_eq!(out.commit_text(), None);
    assert_eq!(
        out.candidate(0),
        Some(("\u{4eee}\u{540d}", "probe fixture"))
    );
}

#[test]
fn idle_space_probe_keeps_insert_and_fence_absorption_actions() {
    let fence = Arc::new(CompositionFence::new());
    let mut active = Dispatcher::new().expect("active dispatcher");
    let mut idle = Dispatcher::new().expect("idle dispatcher");
    active.set_composition_fence(Arc::clone(&fence));
    idle.set_composition_fence(Arc::clone(&fence));

    let mut active_out = OutputBuf::new();
    let mut idle_out = OutputBuf::new();
    let active_session = create_session(&mut active, &mut active_out, "probe-idle.exe");
    let idle_session = create_session(&mut idle, &mut idle_out, "probe-idle.exe");

    probe_space(&mut idle, idle_session, &mut idle_out);
    assert!(
        idle_out.consumed,
        "idle Probe Space must report the insert as consumed"
    );
    assert_eq!(idle_out.commit_text(), Some("\u{3000}"));

    send(&mut active, active_session, char_key('a'), &mut active_out);
    assert!(fence.any_active("probe-idle.exe"));

    idle_out.clear();
    probe_space(&mut idle, idle_session, &mut idle_out);
    assert!(
        idle_out.consumed,
        "idle Probe Space must report the fence absorption as consumed"
    );
    assert_eq!(
        idle_out.commit_text(),
        None,
        "the absorbed probe must not insert text"
    );
    assert!(!idle_out.has_candidates());
    assert!(!idle_out.beep);
}

#[test]
fn production_dual_delivery_is_fenced_against_insert_and_convert() {
    const SEED: u64 = 0x5350_4143_4520_0816;
    let events = [
        DomainEvent::Type { connection: 0 },
        DomainEvent::Space { targets: 0b11 },
    ];
    let oracle = apply_all(2, events);
    assert!(no_dual_effect(&oracle));
    assert!(!oracle.last_inserted);
    assert!(oracle.last_converted);

    let mut world = ProductionWorld::new(2);
    for event in events {
        world.apply(event);
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("verification/space-key-dispatch");
    fs::create_dir_all(&dir).expect("dir");
    fs::write(dir.join("pbt-production-seed.txt"), format!("{SEED}\n")).expect("seed");
    let body = format!(
        "# Production dual-delivery after composition fence\n\n\
seed: {SEED}\n\n\
events: Type(C0), Space(C0|C1)\n\n\
oracle: inserted={} converted={} spaces={}\n\n\
production: inserted={} converted={} spaces={}\n\n\
REQ-SPACE-03: idle peer Space is absorbed while a composing peer converts.\n",
        oracle.last_inserted,
        oracle.last_converted,
        oracle.document_spaces,
        world.last_inserted,
        world.last_converted,
        world.document_spaces
    );
    fs::write(dir.join("pbt-production-shrunk-counterexample.md"), body).expect("evidence");
    assert!(
        !world.last_inserted && world.last_converted,
        "fence must absorb idle Space under dual delivery"
    );
    assert_eq!(
        (oracle.last_inserted, oracle.last_converted),
        (world.last_inserted, world.last_converted)
    );
}

#[test]
fn production_crash_restart_does_not_keep_the_old_composition() {
    let mut world = ProductionWorld::new(1);
    world.apply(DomainEvent::Type { connection: 0 });
    world.apply(DomainEvent::CrashRestart { connection: 0 });
    world.apply(DomainEvent::Space { targets: 1 });
    assert!(world.last_inserted);
    assert!(!world.last_converted);
}

#[test]
fn oracle_and_production_agree_on_single_connection_sequences() {
    let sequences: [&[DomainEvent]; 4] = [
        &[DomainEvent::Space { targets: 1 }],
        &[
            DomainEvent::Type { connection: 0 },
            DomainEvent::Space { targets: 1 },
        ],
        &[
            DomainEvent::Type { connection: 0 },
            DomainEvent::Commit { connection: 0 },
            DomainEvent::Space { targets: 1 },
        ],
        &[
            DomainEvent::Type { connection: 0 },
            DomainEvent::Cancel { connection: 0 },
        ],
    ];
    for events in sequences {
        let mut oracle = OracleState::new(1);
        let mut world = ProductionWorld::new(1);
        for event in events.iter().copied() {
            apply(&mut oracle, event);
            world.apply(event);
        }
        if oracle.connections[0].state == ConnState::Idle && oracle.document_spaces > 0 {
            assert_eq!(world.document_spaces, oracle.document_spaces);
        }
    }
}
