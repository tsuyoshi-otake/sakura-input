use std::fs;
use std::sync::Arc;

use sakura_proto::{KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response, SessionId};

use crate::composition_fence::CompositionFence;
use crate::dispatch::{Dispatcher, Reply};
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
