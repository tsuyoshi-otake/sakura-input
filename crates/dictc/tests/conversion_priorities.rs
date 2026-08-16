//! The shipped calibration overlay is data rather than code, so the rules that
//! keep it safe live here. Every one of them decides what the IME offers first,
//! and a row that quietly breaks one would reopen Issue #48: typing はち put the
//! bare digit 8 ahead of 鉢, 蜂 and 八.
//!
//! These checks read only the source TSV, so they run in the default test pass;
//! the ranking claims that need the compiled dictionary stay in the ignored
//! `sakura-engine` tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dictc::{merge_entries, parse_entries, SourceEntry};
use sakura_core::dictionary::EntryFlags;

/// Upstream files every Arabic digit under one numeral class.
const NUMERAL_CLASS: u16 = 2044;
/// Upstream's offset between a word's conversion cost and its prediction cost.
const PREDICTION_OFFSET: i32 = 1_200;

fn overlay_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/conversion-priorities.tsv")
}

fn overlay() -> Vec<SourceEntry> {
    let path = overlay_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    parse_entries("conversion-priorities.tsv", &text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// A digit row re-prices the numeral edge; a calibration row re-prices a word.
fn is_digit_row(entry: &SourceEntry) -> bool {
    entry.surface.len() == 1 && entry.surface.as_bytes()[0].is_ascii_digit()
}

#[test]
fn the_overlay_parses_and_carries_no_duplicate_edge() {
    let entries = overlay();
    assert!(!entries.is_empty(), "the overlay ships no rows");
    // The merge key is (reading, surface, left_id, right_id). A repeat inside
    // the layer is rejected outright, so this is the strict-duplicate check the
    // dictionary build would fail on.
    let merged = merge_entries(Vec::new(), entries.clone()).expect("no duplicate overlay edge");
    assert_eq!(
        merged.len(),
        entries.len(),
        "merging the overlay onto nothing must keep every row"
    );
}

#[test]
fn every_row_stays_reachable_from_prediction() {
    for entry in overlay() {
        let context = format!("{}\t{}", entry.reading, entry.surface);
        // The overlay replaces an edge rather than adding one. Dropping the
        // prediction flag would remove the original edge from prediction
        // entirely instead of re-pricing it.
        assert!(
            entry.flags.contains(EntryFlags::PREDICTION),
            "row is not offered to prediction: {context}"
        );
        assert_eq!(
            entry.prediction_cost,
            entry.word_cost + PREDICTION_OFFSET,
            "row does not keep upstream's prediction offset: {context}"
        );
        assert!(entry.word_cost >= 0, "negative word_cost: {context}");
    }
}

#[test]
fn digit_rows_replace_the_numeral_edge_and_say_nothing_to_the_user() {
    let entries = overlay();
    for entry in &entries {
        if !is_digit_row(entry) {
            // Only a digit belongs in the numeral class. A calibration row that
            // landed there would re-price a numeral edge by accident.
            assert_ne!(
                (entry.left_id, entry.right_id),
                (NUMERAL_CLASS, NUMERAL_CLASS),
                "a non-digit row claims the numeral class: {}\t{}",
                entry.reading,
                entry.surface
            );
            continue;
        }
        let context = format!("{}\t{}", entry.reading, entry.surface);
        assert_eq!(
            (entry.left_id, entry.right_id),
            (NUMERAL_CLASS, NUMERAL_CLASS),
            "digit row leaves the numeral class: {context}"
        );
        // The annotation column is shown to the user as a candidate note, and a
        // digit needs no gloss. This emptiness is deliberate, not an omission.
        assert!(
            entry.annotation.is_empty(),
            "digit row carries a user-visible note: {context}"
        );
    }
}

#[test]
fn every_arabic_digit_is_calibrated_and_no_reading_is_calibrated_twice() {
    let entries = overlay();
    let mut by_reading: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut digits: BTreeSet<&str> = BTreeSet::new();
    for entry in &entries {
        if !is_digit_row(entry) {
            continue;
        }
        digits.insert(entry.surface.as_str());
        by_reading
            .entry(entry.reading.as_str())
            .or_default()
            .push(entry.surface.as_str());
    }
    let expected: BTreeSet<&str> = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
        .into_iter()
        .collect();
    assert_eq!(
        digits, expected,
        "a digit reading was left at its upstream price"
    );
    // ぜろ and れい both spell 0, but one reading may not offer two digits: the
    // second would compete with the first for the same top-1 slot.
    for (reading, surfaces) in &by_reading {
        assert_eq!(
            surfaces.len(),
            1,
            "reading {reading} calibrates more than one digit: {surfaces:?}"
        );
    }
}

#[test]
fn yesterday_keeps_its_standalone_price() {
    let yesterday: Vec<_> = overlay()
        .into_iter()
        .filter(|entry| entry.reading == "きのう" && entry.surface == "昨日")
        .collect();
    assert_eq!(
        yesterday.len(),
        2,
        "昨日 must keep both the noun-connection and standalone edges"
    );
    for entry in &yesterday {
        assert_eq!(entry.word_cost, 1100, "Issue #62 must not retune 昨日");
        assert!(!entry.flags.contains(EntryFlags::IT));
    }
}

#[test]
fn function_compounds_outrank_the_yesterday_split() {
    let wanted = [
        ("きのうしょうかい", "機能紹介", 1851, 1851),
        ("きのうようけん", "機能要件", 1841, 1851),
        ("きのうこんぽーねんと", "機能コンポーネント", 1851, 1851),
    ];
    let overlay = overlay();
    for (reading, surface, left_id, right_id) in wanted {
        let entry = overlay
            .iter()
            .find(|entry| {
                entry.reading == reading
                    && entry.surface == surface
                    && entry.left_id == left_id
                    && entry.right_id == right_id
            })
            .unwrap_or_else(|| panic!("missing {reading} -> {surface}"));
        assert!(
            entry.flags.contains(EntryFlags::IT),
            "{surface} is not marked IT"
        );
        assert!(
            entry.word_cost <= 3600,
            "{surface} is still too expensive to beat a 昨日 split"
        );
    }
}
