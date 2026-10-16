//! Correction learning through the dispatcher (#305): an idle commit erased
//! whole and retyped with another spelling rejects the erased spelling in the
//! left context it was committed in. Tests T1-T9 of
//! `docs/decisions/issue-305-correction-learning.md`.
//!
//! Each erased commit follows 電気, so it is made in left context 3. The idle
//! Backspace that erases it ends the carried context (#273), so the retyped
//! commit is made in left context 0, and only the correction reaches 3.

use super::*;
use crate::correction::CorrectionTrail;

const READING: &str = "かな";
/// Right ID of 電気: the left context of a commit that follows it.
const LEFT: u16 = 3;
/// Dictionary order of `かな`: 仮名 ends in right ID 1, the rest in 2.
const FIRST: &str = "仮名";
const SECOND: &str = "加奈";
const THIRD: &str = "候補03";
const KATAKANA: &str = "カナ";

fn correction_conversion() -> Arc<ConversionService> {
    let mut source = String::from(
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t1\t100\t100\t\tfixture\nかな\t加奈\t0\t2\t200\t200\t\tfixture\nでんき\t電気\t0\t3\t100\t100\t\tfixture\n",
    );
    for index in 3..=6 {
        writeln!(
            source,
            "かな\t候補{index:02}\t0\t2\t{}\t{}\t\tfixture",
            index * 100,
            index * 100
        )
        .expect("write fixture entry");
    }
    let entries = dictc_core::parse_entries("conversion.tsv", &source).expect("entries");
    let matrix = dictc_core::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t4\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc_core::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"))
}

struct Harness {
    learning: Arc<LearningService>,
    dispatcher: Dispatcher,
    out: OutputBuf,
    session: SessionId,
    probes: u32,
}

impl Harness {
    /// A session whose caret follows a committed 電気.
    fn new() -> Self {
        let learning = Arc::new(LearningService::memory());
        let mut dispatcher =
            Dispatcher::new_with_services(correction_conversion(), Arc::clone(&learning))
                .expect("dispatcher");
        let mut out = OutputBuf::new();
        let session = normal_session(&mut dispatcher, &mut out, "editor.exe");
        let mut harness = Self {
            learning,
            dispatcher,
            out,
            session,
            probes: 0,
        };
        harness.commit("denki", "電気");
        assert_eq!(harness.left_context(), LEFT);
        harness
    }

    fn keys(&mut self, keys: &[KeyInput]) {
        send_keys(&mut self.dispatcher, self.session, keys, &mut self.out);
    }

    /// Converts `romaji` and commits `surface` from its candidates.
    fn commit(&mut self, romaji: &str, surface: &str) {
        type_word(&mut self.dispatcher, self.session, romaji, &mut self.out);
        self.convert_and_commit(surface);
    }

    fn convert_and_commit(&mut self, surface: &str) {
        self.keys(&[named_key(KeyCode::Space)]);
        for _ in 0..8 {
            if self.out.preedit_text() == surface {
                break;
            }
            self.keys(&[named_key(KeyCode::Down)]);
        }
        assert_eq!(self.out.preedit_text(), surface, "{surface} is offered");
        self.keys(&[named_key(KeyCode::Enter)]);
        assert_eq!(self.out.commit_text(), Some(surface));
    }

    /// Commits the composed `かな` through the katakana transform, which is
    /// not learned.
    fn commit_katakana(&mut self) {
        self.keys(&[named_key(KeyCode::F7), named_key(KeyCode::Enter)]);
        assert_eq!(self.out.commit_text(), Some(KATAKANA));
    }

    /// Idle Backspaces, each erasing one character in the host.
    fn erase(&mut self, characters: usize) {
        for _ in 0..characters {
            self.keys(&[named_key(KeyCode::Backspace)]);
            assert!(!self.out.consumed, "the idle Backspace belongs to the host");
        }
    }

    fn left_context(&self) -> u16 {
        self.dispatcher
            .sessions
            .get(self.session)
            .expect("session")
            .carry_right_id()
    }

    fn rejection(&self, surface: &str) -> Option<bool> {
        self.learning.test_rejection(READING, LEFT, surface)
    }

    fn learned(&self, surface: &str) -> bool {
        let mut found = false;
        self.learning
            .visit_prediction_history(READING, |_, learned, _, _| {
                found |= learned == surface;
                true
            });
        found
    }

    /// The first candidate a new session offers for `かな` after 電気, in the
    /// left context the erased commits were made in. The probe closes its
    /// composition, and each probe is its own process, so no probe holds the
    /// next one back as an idle peer.
    fn first_candidate_after_denki(&mut self) -> String {
        self.probes += 1;
        let name = format!("probe{}.exe", self.probes);
        let other = normal_session(&mut self.dispatcher, &mut self.out, &name);
        type_word(&mut self.dispatcher, other, "denki", &mut self.out);
        let commit = [named_key(KeyCode::Space), named_key(KeyCode::Enter)];
        send_keys(&mut self.dispatcher, other, &commit, &mut self.out);
        assert_eq!(self.out.commit_text(), Some("電気"));
        type_word(&mut self.dispatcher, other, "kana", &mut self.out);
        send_keys(
            &mut self.dispatcher,
            other,
            &[named_key(KeyCode::Space)],
            &mut self.out,
        );
        let first = self.out.preedit_text().to_owned();
        let close = [named_key(KeyCode::Escape), named_key(KeyCode::Escape)];
        send_keys(&mut self.dispatcher, other, &close, &mut self.out);
        assert_eq!(
            self.out.preedit_text(),
            "",
            "the probe composition is closed"
        );
        first
    }

    fn learn_after_denki(&self, surface: &str, right_context: u16) {
        for _ in 0..8 {
            self.learning.learn(READING, surface, LEFT, right_context);
        }
    }
}

fn normal_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
    let session = create_session(dispatcher, out, name);
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            out,
        ),
        Reply::Message(Response::Ok)
    );
    session
}

// T1: a commit learned eight times, erased and retyped through the katakana
// transform, stops leading its context; the transform itself is not learned
// (fail-before).
#[test]
fn issue_305_kana_transform_retype_rejects_the_erased_spelling() {
    let mut harness = Harness::new();
    harness.learn_after_denki(SECOND, 2);
    assert_eq!(harness.first_candidate_after_denki(), SECOND);
    harness.commit("kana", SECOND);
    harness.erase(2);
    assert_eq!(
        harness.left_context(),
        0,
        "the erase ended the carried context"
    );
    type_word(
        &mut harness.dispatcher,
        harness.session,
        "kana",
        &mut harness.out,
    );
    harness.commit_katakana();
    harness.keys(&[named_key(KeyCode::Right)]);

    assert_eq!(harness.rejection(SECOND), Some(true));
    assert!(
        !harness.learned(KATAKANA),
        "a kana transform is never learned"
    );
    assert_eq!(
        harness.first_candidate_after_denki(),
        FIRST,
        "dictionary order again"
    );
}

// T2: a spelling retyped from dictionary index 2 leads the context the erased
// commit was made in (fail-before).
#[test]
fn issue_305_retyped_spelling_leads_the_erased_commits_context() {
    let mut harness = Harness::new();
    harness.learn_after_denki(FIRST, 1);
    harness.commit("kana", FIRST);
    harness.erase(2);
    harness.commit("kana", THIRD);
    harness.keys(&[named_key(KeyCode::Right)]);

    assert_eq!(harness.rejection(FIRST), Some(true));
    assert_eq!(harness.first_candidate_after_denki(), THIRD);
}

// T3: erasing a later commit together with the corrected one still matches
// the corrected one (fail-before).
#[test]
fn issue_305_retype_after_erasing_two_commits_corrects_the_first() {
    let mut harness = Harness::new();
    harness.learn_after_denki(FIRST, 1);
    harness.commit("kana", FIRST);
    harness.commit("denki", "電気");
    harness.erase(4);
    harness.commit("kana", THIRD);
    harness.keys(&[named_key(KeyCode::Right)]);

    assert_eq!(harness.rejection(FIRST), Some(true));
    assert_eq!(harness.first_candidate_after_denki(), THIRD);
}

// T4: retyping the erased spelling records no tombstone and learns the retyped
// commit only where it was typed.
#[test]
fn issue_305_retyping_the_same_spelling_is_not_a_correction() {
    let mut harness = Harness::new();
    harness.commit("kana", FIRST);
    harness.erase(2);
    harness.commit("kana", FIRST);
    harness.keys(&[named_key(KeyCode::Right)]);

    assert_eq!(harness.rejection(FIRST), None);
    let candidates = [(FIRST, 1), (SECOND, 2)];
    assert_eq!(
        harness.learning.preference(READING, LEFT, candidates).exact,
        None,
        "neither the erased commit nor the retype is learned after 電気"
    );
    assert_eq!(
        harness.learning.preference(READING, 0, candidates).exact,
        Some(0)
    );
}

// T5: an erase run ends at any key other than a plain or Shift Backspace, and
// at a document or field change; a later retype then matches nothing.
#[test]
fn issue_305_an_interrupted_erase_run_matches_nothing() {
    type Interruption = fn(&mut Harness);
    let interruptions: [(&str, Interruption, Option<bool>); 9] = [
        ("Left", |h| h.keys(&[named_key(KeyCode::Left)]), None),
        ("Right", |h| h.keys(&[named_key(KeyCode::Right)]), None),
        ("Enter", |h| h.keys(&[named_key(KeyCode::Enter)]), None),
        ("Delete", |h| h.keys(&[named_key(KeyCode::Delete)]), None),
        (
            "Ctrl+Backspace outside the undo window",
            |h| h.keys(&[modified_named_key(KeyCode::Backspace, Modifiers::CTRL)]),
            None,
        ),
        (
            "Alt+Backspace",
            |h| h.keys(&[modified_named_key(KeyCode::Backspace, Modifiers::ALT)]),
            None,
        ),
        (
            "ResetDocumentContext",
            |h| {
                assert_eq!(
                    h.dispatcher.dispatch(
                        &Request::ResetDocumentContext { session: h.session },
                        &mut h.out,
                    ),
                    Reply::Message(Response::Ok)
                );
            },
            None,
        ),
        (
            "another field",
            |h| {
                for scope in [InputScope::Url, InputScope::Normal] {
                    assert_eq!(
                        h.dispatcher.dispatch(
                            &Request::SetInputScope {
                                session: h.session,
                                scope,
                            },
                            &mut h.out,
                        ),
                        Reply::Message(Response::Ok)
                    );
                }
            },
            None,
        ),
        (
            "Shift+Backspace, which erases like Backspace",
            |h| h.keys(&[modified_named_key(KeyCode::Backspace, Modifiers::SHIFT)]),
            Some(true),
        ),
    ];
    for (name, interrupt, expected) in interruptions {
        let mut harness = Harness::new();
        harness.commit("kana", FIRST);
        harness.erase(1);
        interrupt(&mut harness);
        if expected.is_none() {
            harness.erase(1);
        }
        harness.commit("kana", THIRD);
        harness.keys(&[named_key(KeyCode::Right)]);
        assert_eq!(harness.rejection(FIRST), expected, "{name}");
    }
}

// T6: an erase one character short of the commit, or one past it, matches
// nothing.
#[test]
fn issue_305_an_erase_of_another_length_matches_nothing() {
    for (erased, expected) in [(1, None), (2, Some(true)), (3, None)] {
        let mut harness = Harness::new();
        harness.commit("kana", FIRST);
        harness.erase(erased);
        harness.commit("kana", THIRD);
        harness.keys(&[named_key(KeyCode::Right)]);
        assert_eq!(harness.rejection(FIRST), expected, "{erased} erased");
    }
}

// T7: an erased correction records nothing; the next retype rejects both
// erased spellings, or only the first when it repeats the erased correction.
#[test]
fn issue_305_an_erased_correction_carries_its_rejection_to_the_next_retype() {
    for (retype, expected_second) in [(THIRD, Some(true)), (SECOND, Some(false))] {
        let mut harness = Harness::new();
        harness.commit("kana", FIRST);
        harness.erase(2);
        harness.commit("kana", SECOND);
        harness.erase(2);
        assert_eq!(harness.rejection(FIRST), None, "{retype}: nothing applied");
        assert!(!harness.learned(SECOND), "{retype}: nothing learned");

        harness.commit("kana", retype);
        harness.keys(&[named_key(KeyCode::Right)]);
        assert_eq!(harness.rejection(FIRST), Some(true), "{retype}");
        assert_eq!(harness.rejection(SECOND), expected_second, "{retype}");
        assert!(harness.learned(retype), "{retype}");
    }
}

// T8: a commit the host removed through an applied undo counts as erased; the
// undo restores its reading, and the transform commits it (fail-before).
#[test]
fn issue_305_retype_after_an_applied_undo_rejects_the_undone_spelling() {
    let mut harness = Harness::new();
    harness.learn_after_denki(SECOND, 2);
    harness.commit("kana", SECOND);
    harness.keys(&[modified_named_key(KeyCode::Backspace, Modifiers::CTRL)]);
    assert_eq!(harness.out.delete_before(), SECOND);
    assert_eq!(
        harness.dispatcher.dispatch(
            &Request::UndoCommit {
                session: harness.session,
                outcome: UndoCommitOutcome::Applied,
            },
            &mut harness.out,
        ),
        Reply::Message(Response::Ok)
    );
    harness.commit_katakana();
    harness.keys(&[named_key(KeyCode::Right)]);

    assert_eq!(harness.rejection(SECOND), Some(true));
    assert_eq!(harness.first_candidate_after_denki(), FIRST);
}

// T9: a commit in a sensitive scope never enters the trail, and it ends what
// the trail remembered before it.
#[test]
fn issue_305_a_sensitive_commit_never_enters_the_trail() {
    let learning = LearningService::memory();
    let mut session = Session::new("editor.exe");
    session.preedit.push_str(READING).expect("bounded reading");
    let commit = |session: &mut Session, surface: &str, right_context: u16| {
        record_learning(
            session,
            Some(&learning),
            true,
            None,
            ExecutionPolicy::Apply,
            surface,
            right_context,
        );
    };
    commit(&mut session, FIRST, 1);
    session.correction_trail_mut().erase();
    session.correction_trail_mut().erase();
    assert_ne!(*session.correction_trail_mut(), CorrectionTrail::default());

    session.scope = InputScope::Password;
    commit(&mut session, SECOND, 2);
    assert_eq!(*session.correction_trail_mut(), CorrectionTrail::default());

    session.scope = InputScope::Normal;
    commit(&mut session, THIRD, 2);
    learning.confirm_all_staged();
    assert_eq!(learning.test_rejection(READING, 0, FIRST), None);
}
