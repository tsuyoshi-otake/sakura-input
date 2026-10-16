use super::*;

const L: u16 = 1841;

fn rejected(correction: &Correction) -> Vec<(&str, u16)> {
    correction
        .rejected()
        .iter()
        .map(|item| (item.surface(), item.right_context()))
        .collect()
}

fn erase(trail: &mut CorrectionTrail, chars: usize) {
    for _ in 0..chars {
        trail.erase();
    }
}

#[test]
fn erasing_a_commit_and_retyping_another_surface_is_a_correction() {
    let mut trail = CorrectionTrail::default();
    assert_eq!(trail.commit("ずみ", "ズミ", L, 7), None);
    erase(&mut trail, 2);
    let correction = trail.commit("ずみ", "済み", 0, 9).expect("correction");
    assert_eq!(correction.origin_left(), L);
    assert_eq!(rejected(&correction), [("ズミ", 7)]);
}

#[test]
fn retyping_the_same_surface_is_not_a_correction() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    assert_eq!(trail.commit("ずみ", "ズミ", 0, 7), None);
}

#[test]
fn a_partial_or_excess_erase_never_matches() {
    for erased in [1, 3] {
        let mut trail = CorrectionTrail::default();
        trail.commit("かな", "仮", 0, 0);
        trail.commit("ずみ", "ズミ", L, 7);
        erase(&mut trail, erased);
        assert_eq!(trail.commit("ずみ", "済み", 0, 9), None, "{erased}");
    }
}

#[test]
fn a_different_reading_does_not_match_and_drops_the_erased_commit() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    assert_eq!(trail.commit("すみ", "済み", 0, 9), None);
    assert_eq!(trail.commit("ずみ", "済み", 0, 9), None);
}

#[test]
fn clearing_ends_the_erase_run() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 1);
    trail.clear();
    erase(&mut trail, 1);
    assert_eq!(trail.commit("ずみ", "済み", 0, 9), None);
}

#[test]
fn two_erased_commits_are_retyped_left_to_right() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    trail.commit("で", "で", 7, 3);
    erase(&mut trail, 3);
    let first = trail.commit("ずみ", "済み", 0, 9).expect("first retype");
    assert_eq!(first.origin_left(), L);
    assert_eq!(rejected(&first), [("ズミ", 7)]);
    let second = trail.commit("で", "出", 9, 4).expect("second retype");
    assert_eq!(second.origin_left(), 7);
    assert_eq!(rejected(&second), [("で", 3)]);
}

#[test]
fn a_retyped_correction_erased_again_rejects_both_surfaces() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    trail
        .commit("ずみ", "済み", 0, 9)
        .expect("first correction");
    erase(&mut trail, 2);
    let correction = trail
        .commit("ずみ", "澄み", 0, 5)
        .expect("second correction");
    assert_eq!(correction.origin_left(), L);
    assert_eq!(rejected(&correction), [("ズミ", 7), ("済み", 9)]);
}

#[test]
fn retyping_an_erased_correction_unchanged_keeps_its_rejection() {
    // The erased correction's staged record was discarded with it, so the
    // same surface typed again must carry the rejection forward.
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    trail.commit("ずみ", "済み", 0, 9).expect("correction");
    erase(&mut trail, 2);
    let again = trail.commit("ずみ", "済み", 0, 9).expect("carried");
    assert_eq!(again.origin_left(), L);
    assert_eq!(rejected(&again), [("ズミ", 7)]);
}

#[test]
fn returning_to_the_first_surface_rejects_only_the_one_in_between() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    trail.commit("ずみ", "済み", 0, 9).expect("correction");
    erase(&mut trail, 2);
    let back = trail.commit("ずみ", "ズミ", 0, 7).expect("back");
    assert_eq!(rejected(&back), [("済み", 9)]);
}

#[test]
fn a_long_chain_keeps_the_first_and_the_latest_rejection() {
    let mut trail = CorrectionTrail::default();
    trail.commit("かな", "仮名", L, 1);
    for (surface, right) in [("加奈", 2), ("香奈", 3), ("佳奈", 4)] {
        erase(&mut trail, 2);
        trail.commit("かな", surface, 0, right).expect("correction");
    }
    erase(&mut trail, 2);
    let last = trail.commit("かな", "カナ", 0, 5).expect("last");
    assert_eq!(rejected(&last), [("仮名", 1), ("佳奈", 4)]);
}

#[test]
fn an_applied_undo_erases_the_newest_commit_whole() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    trail.undo_applied("ズミ");
    let correction = trail.commit("ずみ", "済み", L, 9).expect("correction");
    assert_eq!(rejected(&correction), [("ズミ", 7)]);

    let mut mismatched = CorrectionTrail::default();
    mismatched.commit("ずみ", "ズミ", L, 7);
    mismatched.undo_applied("済み");
    assert_eq!(mismatched.commit("ずみ", "済み", L, 9), None);
}

#[test]
fn an_untracked_long_commit_is_counted_but_never_matches() {
    let long_reading = "あ".repeat(40);
    let long_surface = "亜".repeat(40);
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    trail.commit(&long_reading, &long_surface, 7, 1);
    erase(&mut trail, 40);
    assert_eq!(trail.commit(&long_reading, "別", 0, 1), None);

    // Counting the long commit off lets the earlier one match.
    let mut counted = CorrectionTrail::default();
    counted.commit("ずみ", "ズミ", L, 7);
    counted.commit(&long_reading, &long_surface, 7, 1);
    erase(&mut counted, 42);
    let correction = counted
        .commit("ずみ", "済み", 0, 9)
        .expect("ズミ is the pending top");
    assert_eq!(rejected(&correction), [("ズミ", 7)]);
}

#[test]
fn only_four_commits_are_remembered() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    for _ in 0..4 {
        trail.commit("か", "家", 0, 1);
    }
    // The fifth commit pushed ズミ out, so erasing it too overflows.
    erase(&mut trail, 6);
    assert_eq!(trail.commit("ずみ", "済み", 0, 9), None);
}

#[test]
fn forgetting_a_pair_strips_it_from_carried_rejections() {
    let mut trail = CorrectionTrail::default();
    trail.commit("ずみ", "ズミ", L, 7);
    erase(&mut trail, 2);
    trail.commit("ずみ", "済み", 0, 9).expect("correction");
    erase(&mut trail, 2);
    trail.forget("ずみ", "ズミ");
    assert_eq!(trail.commit("ずみ", "済み", 0, 9), None);
}

#[test]
fn the_trail_stays_small() {
    let bytes = core::mem::size_of::<CorrectionTrail>();
    println!("CorrectionTrail={bytes}");
    assert!(bytes <= 2 * 1024, "CorrectionTrail grew to {bytes} bytes");
}
