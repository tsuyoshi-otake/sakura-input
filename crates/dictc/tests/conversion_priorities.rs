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
/// The conversion cost above which the Mozc importer stops offering a word to
/// prediction at all. Mirrors `parse_mozc_entries`.
const PREDICTION_COST_LIMIT: i32 = 6_000;

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
fn every_row_keeps_the_prediction_status_its_own_cost_earns() {
    for entry in overlay() {
        let context = format!("{}\t{}", entry.reading, entry.surface);
        assert!(entry.word_cost >= 0, "negative word_cost: {context}");
        // The overlay replaces an edge rather than adding one, so a row that
        // dropped the prediction flag would remove the original edge from
        // prediction entirely instead of re-pricing it. The importer already
        // withholds prediction above `PREDICTION_COST_LIMIT`, so only a row
        // priced above that line can be non-predictive without hiding a loss:
        // Issue #94 re-prices 対案 and 禁則 to 6078 and 6776, still above the
        // line their upstream edges were already on.
        if entry.word_cost <= PREDICTION_COST_LIMIT {
            assert!(
                entry.flags.contains(EntryFlags::PREDICTION),
                "row is not offered to prediction: {context}"
            );
            assert_eq!(
                entry.prediction_cost,
                entry.word_cost + PREDICTION_OFFSET,
                "row does not keep upstream's prediction offset: {context}"
            );
        } else {
            assert!(
                !entry.flags.contains(EntryFlags::PREDICTION),
                "row is priced out of prediction yet claims it: {context}"
            );
            assert_eq!(
                entry.prediction_cost,
                i32::MAX,
                "a non-predictive row must carry no prediction cost: {context}"
            );
        }
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
        // The annotation column is shown to the user as a candidate note.
        // This emptiness is deliberate, not an omission.
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
        ("きのうかいぜん", "機能改善", 1841, 1841),
        ("きのうかいぞう", "機能改造", 1851, 1851),
        ("きのうかいはつ", "機能開発", 1841, 1841),
        ("きのうかくちょう", "機能拡張", 1841, 1841),
        ("きのうきょうか", "機能強化", 1841, 1841),
        ("きのうこんぽーねんと", "機能コンポーネント", 1851, 1851),
        ("きのうさくじょ", "機能削除", 1851, 1851),
        ("きのうさぶん", "機能差分", 1851, 1851),
        ("きのうじっそう", "機能実装", 1851, 1851),
        ("きのうしょうかい", "機能紹介", 1851, 1851),
        ("きのうじょう", "機能上", 1851, 1851),
        ("きのうしよう", "機能仕様", 1851, 1851),
        ("きのうせいげん", "機能制限", 1841, 1841),
        ("きのうせつめい", "機能説明", 1851, 1851),
        ("きのうていし", "機能停止", 1851, 1851),
        ("きのうてすと", "機能テスト", 1851, 1851),
        ("きのうついか", "機能追加", 1841, 1841),
        ("きのうてき", "機能的", 1841, 2032),
        ("きのうへんこう", "機能変更", 1851, 1851),
        ("きのうひょうか", "機能評価", 1851, 1851),
        ("きのうぶんるい", "機能分類", 1851, 1851),
        ("きのうめい", "機能名", 1851, 1851),
        ("きのうめん", "機能面", 1841, 1949),
        ("きのうようけん", "機能要件", 1841, 1851),
        ("きのうようぼう", "機能要望", 1851, 1851),
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

#[test]
fn business_compounds_outrank_homophone_splits() {
    let wanted = [
        ("いどうとどけ", "異動届", 1851, 1851),
        ("けっさいしゃ", "決裁者", 1841, 1986),
        ("けっさいしょ", "決裁書", 1851, 1851),
        ("けんしゅうかんりょう", "検収完了", 1851, 1851),
        ("けんしゅうしけん", "検収試験", 1851, 1851),
        ("けんしゅうしょ", "検収書", 1851, 1851),
        ("こうつうひせいさん", "交通費精算", 1851, 1851),
        ("しゃないきてい", "社内規程", 1851, 1841),
        ("しゅうぎょうきてい", "就業規程", 1851, 1851),
        ("しょうひょうしょるい", "証憑書類", 1851, 1851),
        ("はいふきんじゅん", "配賦基準", 1851, 1851),
        ("ふくむきてい", "服務規程", 1851, 1851),
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
            entry.word_cost <= 3600,
            "{surface} is still too expensive to beat a homophone split"
        );
    }
}

#[test]
fn overlay_rows_say_nothing_to_the_user() {
    for entry in overlay() {
        let context = format!("{}\t{}", entry.reading, entry.surface);
        assert!(
            entry.annotation.is_empty(),
            "overlay row carries a user-visible note: {context}"
        );
    }
}
