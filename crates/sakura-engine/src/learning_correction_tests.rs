//! Correction learning (#305): what a staged correction records, how its
//! tombstones rank, and how they survive replay, compaction, and forget.
//! Tests L1-L9, S2, and S3 of `docs/decisions/issue-305-correction-learning.md`.

use super::*;
use sakura_store::learning::{REJECTION_PREFIX, REJECTION_SEPARATOR};

const READING: &str = "かな";
/// Left context the erased commit was made in.
const LEFT: u16 = 3;
/// Left context of the retyped commit: an idle Backspace ends the carried
/// context of the previous commit (#273).
const RETYPED_LEFT: u16 = 0;
/// The erased spelling and the right ID it was committed with.
const X: (&str, u16) = ("仮名", 4);
/// A learnable retyped spelling, at dictionary index 3 of `CANDIDATES`.
const Y: (&str, u16) = ("加奈", 5);
/// A mechanical kana transform, which is never learning evidence.
const KANA: (&str, u16) = ("カナ", 9);
const OWNER: u64 = 1;
const CANDIDATES: [(&str, u16); 4] = [X, ("別名", 6), ("仮字", 7), Y];
const NO_PREFERENCE: LearningPreference = LearningPreference {
    exact: None,
    general: None,
};

fn learn_x(service: &LearningService, times: usize) {
    for _ in 0..times {
        service.learn(READING, X.0, LEFT, X.1);
    }
}

/// Stages `retyped` as the learnable commit that replaced the erased X.
fn stage_correction(service: &LearningService, owner: u64, retyped: (&str, u16)) {
    service.stage(
        owner,
        READING,
        retyped.0,
        RETYPED_LEFT,
        retyped.1,
        Some(StagedCorrection::new(LEFT, [X])),
    );
}

/// Stages the kana transform that replaced the erased X.
fn stage_kana_correction(service: &LearningService, owner: u64) {
    service.stage_reject_only(
        owner,
        READING,
        KANA.0,
        RETYPED_LEFT,
        KANA.1,
        StagedCorrection::new(LEFT, [X]),
    );
}

fn has_marker(service: &LearningService, left_context: u16) -> bool {
    service.test_rejection(READING, left_context, X.0).is_some()
}

fn rejected(service: &LearningService, left_context: u16, surface: &str) -> bool {
    service.test_rejection(READING, left_context, surface) == Some(true)
}

fn records_of(records: &[LearningRecord], surface: &str) -> usize {
    records
        .iter()
        .filter(|record| record.reading == READING && record.surface == surface)
        .count()
}

// L1: the spelling retyped over an erased commit wins where that commit was
// made, from dictionary index 3 and past eight earlier choices (fail-before).
#[test]
fn issue_305_retyped_spelling_wins_in_the_erased_commits_context() {
    let corrected = LearningService::memory();
    learn_x(&corrected, 8);
    stage_correction(&corrected, OWNER, Y);
    corrected.confirm_staged(OWNER);
    assert_eq!(
        corrected.preference(READING, LEFT, CANDIDATES),
        LearningPreference {
            exact: Some(3),
            general: None
        }
    );
    assert!(rejected(&corrected, LEFT, X.0));
    assert!(!rejected(&corrected, LEFT, Y.0));

    // Without the correction the retyped commit is learned only at the left
    // context it was typed in, and the erased spelling keeps winning.
    let uncorrected = LearningService::memory();
    learn_x(&uncorrected, 8);
    uncorrected.stage(OWNER, READING, Y.0, RETYPED_LEFT, Y.1, None);
    uncorrected.confirm_staged(OWNER);
    assert_eq!(
        uncorrected.preference(READING, LEFT, CANDIDATES).exact,
        Some(0)
    );

    // Without a tombstone, one exact choice at index 3 stays behind the
    // position gate.
    let alone = LearningService::memory();
    alone.learn(READING, Y.0, LEFT, Y.1);
    assert_eq!(alone.preference(READING, LEFT, CANDIDATES).exact, None);
}

// L2: a kana-transform correction learns nothing, so the context falls back
// to dictionary order; an older one-off choice is not promoted.
#[test]
fn issue_305_kana_correction_falls_back_to_dictionary_order() {
    let older = ("可奈", 8);
    let candidates = [X, ("別名", 6), ("仮字", 7), Y, older];
    let service = LearningService::memory();
    service.learn(READING, older.0, LEFT, older.1);
    learn_x(&service, 8);
    assert_eq!(service.preference(READING, LEFT, candidates).exact, Some(0));

    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    assert_eq!(service.preference(READING, LEFT, candidates), NO_PREFERENCE);
    assert!(
        !history_contains(&service, READING, KANA.0),
        "a kana transform is never learned"
    );
}

// L3: a tombstone holds only in its own left context.
#[test]
fn issue_305_rejection_holds_only_in_its_left_context() {
    let service = LearningService::memory();
    learn_x(&service, 8);
    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    let other = LEFT + 6;
    assert!(!rejected(&service, other, X.0));
    assert_eq!(
        service.preference(READING, other, CANDIDATES),
        LearningPreference {
            exact: None,
            general: Some(0)
        }
    );
}

// L4: choosing the rejected spelling again in that context ends its tombstone,
// and the renewed choice wins past the position gate.
#[test]
fn issue_305_choosing_the_rejected_spelling_again_restores_it() {
    let candidates = [("別名", 6), ("仮字", 7), X, Y];
    let service = LearningService::memory();
    learn_x(&service, 8);
    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    assert_eq!(service.preference(READING, LEFT, candidates).exact, None);

    learn_x(&service, 1);
    assert!(!rejected(&service, LEFT, X.0));
    assert!(has_marker(&service, LEFT));
    assert_eq!(service.preference(READING, LEFT, candidates).exact, Some(2));
}

// L6: replay applies commits and tombstones in log order, so the running
// state, a reopened log, and a compacted log rank alike.
#[test]
fn issue_305_replay_and_compaction_keep_the_order_of_tombstones() {
    let candidates = [("別名", 6), ("仮字", 7), X, Y];
    let temporary = TemporaryLearningLog::new("issue-305-tombstone-replay");
    let path = &temporary.0;

    let service = LearningService::open(path).expect("open isolated learning");
    learn_x(&service, 8);
    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    service.maintain().expect("flush the tombstone");
    let live = service.preference(READING, LEFT, candidates);
    drop(service);
    let reopened = LearningService::open(path).expect("reopen after the tombstone");
    assert_eq!(
        [live, reopened.preference(READING, LEFT, candidates)],
        [NO_PREFERENCE; 2],
        "live, reopened"
    );
    assert!(rejected(&reopened, LEFT, X.0));

    let mut observed = Vec::new();
    learn_x(&reopened, 1);
    reopened.maintain().expect("flush the renewed choice");
    observed.push(reopened.preference(READING, LEFT, candidates));
    drop(reopened);
    let renewed = LearningService::open(path).expect("reopen after the renewed choice");
    observed.push(renewed.preference(READING, LEFT, candidates));
    {
        let mut state = renewed.lock_state();
        // The newest five frames: three choices of X, the tombstone, and the
        // renewed choice.
        compact_state(&mut state, 1_024, 5).expect("compact isolated learning");
    }
    observed.push(renewed.preference(READING, LEFT, candidates));
    drop(renewed);
    let compacted = LearningService::open(path).expect("reopen compacted learning");
    observed.push(compacted.preference(READING, LEFT, candidates));
    let retained = read_snapshot(path).expect("compacted snapshot");
    assert_eq!((retained.records.len(), retained.rejections.len()), (4, 1));
    assert!(!rejected(&compacted, LEFT, X.0));
    assert!(has_marker(&compacted, LEFT));
    assert_eq!(observed[0].exact, Some(2));
    assert!(
        observed.iter().all(|preference| *preference == observed[0]),
        "live, reopened, compacted, reopened again: {observed:?}"
    );
}

// L7: forgetting a pair forgets its tombstones durably, and a correction
// staged before the forget no longer rejects it.
#[test]
fn issue_305_forgetting_a_pair_forgets_its_tombstones() {
    let temporary = TemporaryLearningLog::new("issue-305-forget-tombstone");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open isolated learning");
    learn_x(&service, 8);
    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    assert!(rejected(&service, LEFT, X.0));
    assert_eq!(
        service
            .forget_prediction_exact(READING, X.0)
            .expect("forget X"),
        ForgetPredictionOutcome::Removed
    );
    assert!(!has_marker(&service, LEFT));

    learn_x(&service, 1);
    stage_correction(&service, OWNER, Y);
    assert_eq!(
        service
            .forget_prediction_exact(READING, X.0)
            .expect("forget X again"),
        ForgetPredictionOutcome::Removed
    );
    service.confirm_staged(OWNER);
    assert!(
        !has_marker(&service, LEFT),
        "the stripped correction rejected nothing"
    );
    assert!(
        history_contains(&service, READING, Y.0),
        "the retyped commit itself is still learned"
    );
    drop(service);

    let reopened = LearningService::open(path).expect("reopen after forget");
    assert!(!has_marker(&reopened, LEFT));
    let snapshot = read_snapshot(path).expect("snapshot after forget");
    assert!(snapshot.rejections.is_empty());
    assert_eq!(records_of(&snapshot.records, X.0), 0);
    // With nothing left to reject, Y is an ordinary commit at the left context
    // it was typed in only.
    assert_eq!(records_of(&snapshot.records, Y.0), 1);
}

// L7: a staged commit of the forgotten pair is no longer learned, but still
// rejects the spelling it corrected.
#[test]
fn issue_305_a_forgotten_staged_commit_keeps_only_its_rejections() {
    let temporary = TemporaryLearningLog::new("issue-305-forget-staged");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open isolated learning");
    learn_x(&service, 1);
    service.learn(READING, Y.0, LEFT, Y.1);
    service.stage(
        OWNER,
        READING,
        X.0,
        RETYPED_LEFT,
        X.1,
        Some(StagedCorrection::new(LEFT, [Y])),
    );
    assert_eq!(
        service
            .forget_prediction_exact(READING, X.0)
            .expect("forget X"),
        ForgetPredictionOutcome::Removed
    );
    service.confirm_staged(OWNER);
    assert!(rejected(&service, LEFT, Y.0));
    assert!(!history_contains(&service, READING, X.0));
    service.maintain().expect("flush the tombstone");
    let snapshot = read_snapshot(path).expect("snapshot after forget");
    assert_eq!(records_of(&snapshot.records, X.0), 0);
    assert_eq!(records_of(&snapshot.rejections, Y.0), 1);
}

// L8 + S2: a kana-transform correction writes one tombstone and no commit,
// however often its owner is confirmed. A snapshot keeps tombstones out of
// the commit table, and a malformed one out of both.
#[test]
fn issue_305_reject_only_record_writes_one_tombstone_and_no_commit() {
    let temporary = TemporaryLearningLog::new("issue-305-reject-only");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open isolated learning");
    learn_x(&service, 8);
    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    service.confirm_staged(OWNER);
    service.confirm_all_staged();
    service.maintain().expect("flush the tombstone");
    drop(service);

    let malformed = format!("{REJECTION_PREFIX}{READING}{}", X.0);
    let payload = encode_record("", &malformed, LEFT, X.1, 20_000).expect("malformed payload");
    append_test_frame(path, &payload, crc32(&payload), payload.len());
    let snapshot = read_snapshot(path).expect("verified snapshot");
    assert_eq!(snapshot.records.len(), 8);
    assert_eq!(records_of(&snapshot.records, X.0), 8);
    let rejections: Vec<_> = snapshot
        .rejections
        .iter()
        .map(|record| {
            (
                record.reading.as_str(),
                record.surface.as_str(),
                record.left_context,
                record.right_context,
            )
        })
        .collect();
    assert_eq!(rejections, [(READING, X.0, LEFT, X.1)]);
    let well_formed = format!("{REJECTION_PREFIX}{READING}{REJECTION_SEPARATOR}{}", X.0);
    assert!(!snapshot
        .records
        .iter()
        .any(|record| record.surface == malformed || record.surface == well_formed));

    let reopened = LearningService::open(path).expect("reopen with a malformed frame");
    assert!(rejected(&reopened, LEFT, X.0));
    assert_eq!(
        reopened.preference(READING, LEFT, CANDIDATES),
        NO_PREFERENCE
    );
}

// L8: an erased correction applies nothing; bulk confirmation and eviction
// each apply a correction exactly once.
#[test]
fn issue_305_staged_corrections_apply_exactly_once() {
    let temporary = TemporaryLearningLog::new("issue-305-exactly-once");
    let path = &temporary.0;
    let service = LearningService::open(path).expect("open isolated learning");
    learn_x(&service, 8);
    stage_correction(&service, OWNER, Y);
    service.discard_staged(OWNER);
    service.confirm_all_staged();
    assert!(
        !has_marker(&service, LEFT),
        "an erased correction rejects nothing"
    );

    stage_correction(&service, OWNER, Y);
    service.confirm_all_staged();
    service.confirm_staged(OWNER);
    service.confirm_all_staged();
    service.maintain().expect("flush the bulk confirmation");
    let snapshot = read_snapshot(path).expect("snapshot after bulk confirmation");
    assert_eq!(records_of(&snapshot.rejections, X.0), 1);
    // Learned where it was typed and where the erased commit was made.
    assert_eq!(records_of(&snapshot.records, Y.0), 2);

    stage_correction(&service, OWNER, Y);
    for other in 0..MAX_STAGED_COMMITS as u64 {
        service.stage(OWNER + 1 + other, "さくら", "桜", 0, 0, None);
    }
    service.maintain().expect("flush the eviction");
    let snapshot = read_snapshot(path).expect("snapshot after eviction");
    assert_eq!(records_of(&snapshot.rejections, X.0), 2);
    assert_eq!(records_of(&snapshot.records, Y.0), 4);

    service.confirm_staged(OWNER);
    service.confirm_all_staged();
    service.maintain().expect("flush the remaining records");
    let snapshot = read_snapshot(path).expect("final snapshot");
    assert_eq!(records_of(&snapshot.rejections, X.0), 2);
    assert_eq!(records_of(&snapshot.records, Y.0), 4);
    assert_eq!(
        snapshot
            .records
            .iter()
            .filter(|record| record.surface == "桜")
            .count(),
        MAX_STAGED_COMMITS
    );
}

// L9: a tombstone covers every right ID of the rejected spelling, so its
// other exact evidence neither wins nor counts as a competing choice.
#[test]
fn issue_305_tombstone_covers_every_right_id_of_the_spelling() {
    let other_right = 10;
    let older = ("可奈", 8);
    let candidates = [(X.0, other_right), ("別名", 6), ("仮字", 7), older];
    let service = LearningService::memory();
    service.learn(READING, older.0, LEFT, older.1);
    for _ in 0..8 {
        service.learn(READING, X.0, LEFT, other_right);
    }
    learn_x(&service, 1);
    assert_eq!(service.preference(READING, LEFT, candidates).exact, Some(0));

    stage_kana_correction(&service, OWNER);
    service.confirm_staged(OWNER);
    assert_eq!(service.preference(READING, LEFT, candidates), NO_PREFERENCE);
}

// S3: an engine from before tombstones replays one as a commit of an empty
// reading. The unchanged `Index::learn` and `PredictionHistory::learn` then
// leave every ranking and prediction as it was.
#[test]
fn issue_305_older_engines_replay_a_tombstone_without_effect() {
    fn observe(
        index: &Index,
        history: &PredictionHistory,
        day: u32,
    ) -> (Vec<LearningPreference>, Vec<(String, String)>) {
        let orders = [CANDIDATES, [Y, ("別名", 6), ("仮字", 7), X]];
        let mut preferences = Vec::new();
        for left_context in [LEFT, LEFT + 6] {
            for order in orders {
                preferences.push(index.preference(READING, left_context, order, day, None));
            }
        }
        let mut predictions = Vec::new();
        history.visit("", day, 10, |reading, surface, _, _| {
            predictions.push((reading.to_owned(), surface.to_owned()));
            true
        });
        (preferences, predictions)
    }

    let day = unix_day();
    let mut index = Index::new();
    let mut history = PredictionHistory::new();
    for sequence in 1..=8 {
        index.learn(LEFT, X.1, READING, X.0, day, sequence);
        history.learn(READING, X.0, X.1, day, sequence);
    }
    index.learn(LEFT, Y.1, READING, Y.0, day, 9);
    history.learn(READING, Y.0, Y.1, day, 9);
    let before = observe(&index, &history, day);
    assert_eq!(before.1.len(), 2);

    let tombstone = format!("{REJECTION_PREFIX}{READING}{REJECTION_SEPARATOR}{}", X.0);
    index.learn(LEFT, X.1, "", &tombstone, day, 10);
    history.learn("", &tombstone, X.1, day, 10);
    assert_eq!(observe(&index, &history, day), before);
}
