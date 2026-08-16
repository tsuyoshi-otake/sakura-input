//! Fixed-seed property campaign for the overlay path Issue #48 shipped through.
//!
//! The workspace adds no external PBT crate, so the generators here are a
//! xorshift64* PRNG over a corpus of hostile and well-formed fields. Every
//! failure prints the seed and iteration, which reproduces it exactly. The
//! ignored target takes the same shard/iteration environment variables as the
//! other long campaigns in this workspace.

use std::collections::{BTreeMap, BTreeSet};

use dictc::{
    compile, entries_to_category_tsv, entries_to_tsv, merge_entries, parse_category_entries,
    parse_connection, parse_entries, SourceEntry,
};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::{Dictionary, EntryFlags};
use sakura_core::ConversionMethod;

const LICENSE: &str = "LicenseRef-Sakura-InHouse";
const HEADER: &str =
    "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n";
const CONNECTION: &str = "# license: BSD-3-Clause\nclasses\t3\ndefault\t0\n";
const MAX_FIELD_BYTES: usize = 1536;

const DEFAULT_HOSTILE_ITERATIONS: u64 = 4_000;
const DEFAULT_ROUND_TRIP_ITERATIONS: u64 = 400;
const DEFAULT_MERGE_ITERATIONS: u64 = 600;
const DEFAULT_RANKING_ITERATIONS: u64 = 150;

/// Fragments a hostile document is assembled from: schema keywords, the exact
/// values this overlay ships, and the characters that decide a branch inside
/// the parser (tab, CR, NUL, control, oversized runs).
const FRAGMENTS: &[&str] = &[
    "",
    "いち",
    "はち",
    "1",
    "一",
    "2044",
    "3639",
    "4839",
    "predict",
    "it,predict",
    "predict,predict",
    "it,it",
    "correction",
    "boost",
    "-",
    "-1",
    "0",
    "65535",
    "65536",
    "2147483647",
    "2147483648",
    "99999999999999999999",
    " ",
    "\t",
    "\r",
    "\u{0}",
    "\u{7}",
    "#",
    "# license: MIT",
    "# license: LicenseRef-Unknown",
    "reading",
    "annotation",
    "🌸",
    "ｱ",
    "ー",
    "a",
];

/// Readings and surfaces a well-formed row may use. Every one of them is free
/// of tabs, newlines and NUL, so a generated row is always writable.
const SAFE_READINGS: &[&str] = &[
    "いち",
    "に",
    "さん",
    "よん",
    "ご",
    "ろく",
    "なな",
    "はち",
    "きゅう",
    "ぜろ",
    "れい",
    "かんじ",
    "sakura",
    "🌸",
];
const SAFE_SURFACES: &[&str] = &[
    "1", "2", "3", "0", "一", "二", "三", "零", "漢字", "カナ", "Sakura", "🌸",
];
const SAFE_ANNOTATIONS: &[&str] = &["", "note", "common spelling", "注記", "🌸"];
/// The ranking campaign types kana only. An ASCII or symbol reading also offers
/// its own spelling back as a candidate, which says nothing about lattice cost.
const KANA_READINGS: &[&str] = &[
    "いち",
    "に",
    "さん",
    "よん",
    "ご",
    "ろく",
    "なな",
    "はち",
    "きゅう",
    "ぜろ",
    "れい",
    "かんじ",
];

#[test]
fn hostile_documents_are_rejected_or_hold_every_schema_invariant() {
    hostile_campaign(DEFAULT_HOSTILE_ITERATIONS, 0, 0);
}

#[test]
fn well_formed_rows_survive_both_tsv_round_trips() {
    round_trip_campaign(DEFAULT_ROUND_TRIP_ITERATIONS, 0, 0);
}

#[test]
fn merging_two_layers_is_a_union_the_overlay_owns() {
    merge_campaign(DEFAULT_MERGE_ITERATIONS, 0, 0);
}

#[test]
fn repricing_an_edge_through_the_overlay_moves_it_down_the_candidate_list() {
    ranking_campaign(DEFAULT_RANKING_ITERATIONS, 0, 0);
}

#[test]
#[ignore = "long deterministic campaign; set SAKURA_FUZZ_ITERS, SAKURA_FUZZ_SHARD, and SAKURA_FUZZ_SEED"]
fn sharded_overlay_campaign() {
    let iterations = env_u64("SAKURA_FUZZ_ITERS").unwrap_or(200_000);
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let slice_seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    hostile_campaign(iterations, shard, slice_seed);
    round_trip_campaign(iterations / 10, shard, slice_seed);
    merge_campaign(iterations / 10, shard, slice_seed);
    ranking_campaign(iterations / 50, shard, slice_seed);
}

/// No document, however malformed, may panic the parser; and any document it
/// does accept must satisfy every invariant the schema promises downstream.
fn hostile_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0001, shard, slice_seed);
    let mut accepted_rows = 0u64;
    let mut rejected = 0u64;
    let mut unwritable = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let text = hostile_document(&mut random);
        for licensed in [true, false] {
            let parsed = std::panic::catch_unwind(|| {
                if licensed {
                    parse_entries("hostile.tsv", &text)
                } else {
                    parse_category_entries("hostile.tsv", &text)
                }
            });
            let Ok(parsed) = parsed else {
                panic!("parser panicked at iteration {iteration}, seed {seed:#018x}, licensed {licensed}, text {text:?}");
            };
            let Ok(entries) = parsed else {
                rejected += 1;
                continue;
            };
            accepted_rows += entries.len() as u64;
            for entry in &entries {
                let context = format!(
                    "iteration {iteration}, seed {seed:#018x}, licensed {licensed}, entry {entry:?}"
                );
                assert!(!entry.reading.is_empty(), "empty reading: {context}");
                assert!(!entry.surface.is_empty(), "empty surface: {context}");
                assert!(
                    !entry.reading.chars().any(char::is_control),
                    "control character in reading: {context}"
                );
                for field in [&entry.reading, &entry.surface, &entry.annotation] {
                    assert!(!field.contains('\u{0}'), "NUL in a field: {context}");
                    assert!(
                        field.len() <= MAX_FIELD_BYTES,
                        "field over the preedit budget: {context}"
                    );
                }
                assert!(entry.word_cost >= 0, "negative word_cost: {context}");
                assert!(
                    entry.prediction_cost >= 0,
                    "negative prediction_cost: {context}"
                );
                assert!(
                    !entry.reading.contains('\t') && !entry.surface.contains('\t'),
                    "a tab cannot reach the first two columns: {context}"
                );
            }
            // The writer's acceptance set must be exactly the parser's, minus
            // the rows whose fields hold a separator. Anything else would mean
            // a category file could be produced that no longer reparses.
            let writable = entries.iter().all(|entry| {
                !entry.reading.contains(['\t', '\r', '\n'])
                    && !entry.surface.contains(['\t', '\r', '\n'])
                    && !entry.annotation.contains(['\t', '\r', '\n'])
            });
            if !writable {
                unwritable += 1;
            }
            assert_eq!(
                entries_to_category_tsv(&entries).is_ok(),
                writable,
                "writer and parser disagree at iteration {iteration}, seed {seed:#018x}"
            );
        }
    }
    // A campaign that only ever generated garbage, or only ever generated
    // clean rows, would pass while testing nothing.
    assert!(accepted_rows > 0, "no document was ever accepted");
    assert!(rejected > 0, "no document was ever rejected");
    assert!(
        unwritable > 0,
        "no accepted row ever carried a separator inside a field"
    );
}

/// A well-formed layer must survive `parse -> write -> parse` on both the
/// licensed and the category writer without losing a single field. The empty
/// annotation is the interesting one: it is a required trailing tab, and
/// dropping it turns every row into the seven-column error.
fn round_trip_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0002, shard, slice_seed);
    let mut empty_annotations = 0u64;
    let mut unpredicted = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let rows = 1 + random.usize(12);
        let text = well_formed_document(&mut random, rows);
        let context = format!("iteration {iteration}, seed {seed:#018x}");
        let parsed = parse_entries("generated.tsv", &text)
            .unwrap_or_else(|error| panic!("generated document rejected: {error} ({context})"));
        empty_annotations += parsed
            .iter()
            .filter(|entry| entry.annotation.is_empty())
            .count() as u64;
        unpredicted += parsed
            .iter()
            .filter(|entry| entry.prediction_cost == i32::MAX)
            .count() as u64;
        let expected = rendered(&parsed);

        let licensed = entries_to_tsv(&parsed, LICENSE)
            .unwrap_or_else(|error| panic!("licensed writer refused: {error} ({context})"));
        let reparsed = parse_entries("round-trip.tsv", &licensed)
            .unwrap_or_else(|error| panic!("licensed round trip rejected: {error} ({context})"));
        assert_eq!(
            rendered(&reparsed),
            expected,
            "licensed round trip {context}"
        );

        let category = entries_to_category_tsv(&parsed)
            .unwrap_or_else(|error| panic!("category writer refused: {error} ({context})"));
        assert!(
            !category.starts_with('#'),
            "a category file carries no metadata {context}"
        );
        let recategorized = parse_category_entries("round-trip.tsv", &category)
            .unwrap_or_else(|error| panic!("category round trip rejected: {error} ({context})"));
        assert_eq!(
            rendered(&recategorized),
            expected,
            "category round trip {context}"
        );
        for line in category.lines().skip(1) {
            assert_eq!(
                line.split('\t').count(),
                8,
                "every written row keeps all eight columns {context}"
            );
        }
    }
    // The two columns whose empty spelling is easy to lose on the way out.
    assert!(
        empty_annotations > 0,
        "no row with an empty annotation was generated"
    );
    assert!(
        unpredicted > 0,
        "no row with a '-' prediction was generated"
    );
}

/// `merge_entries` must behave as a union keyed on the lattice edge: the
/// overlay owns every shared edge, nothing else moves, and the result stays
/// sorted and duplicate-free however the two layers overlap.
fn merge_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0003, shard, slice_seed);
    let mut replacements = 0u64;
    let mut additions = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let context = format!("iteration {iteration}, seed {seed:#018x}");
        // Both layers draw from one small identity pool, so shared edges are
        // the common case rather than a rare coincidence.
        let system_rows = random.usize(10);
        let system = parse_entries(
            "system.tsv",
            &well_formed_document(&mut random, system_rows),
        )
        .unwrap_or_else(|error| panic!("system layer rejected: {error} ({context})"));
        let overlay_rows = random.usize(10);
        let overlay = parse_entries(
            "overlay.tsv",
            &well_formed_document(&mut random, overlay_rows),
        )
        .unwrap_or_else(|error| panic!("overlay layer rejected: {error} ({context})"));

        let system_by_identity = by_identity(&system);
        let overlay_by_identity = by_identity(&overlay);
        let merged = merge_entries(system.clone(), overlay.clone())
            .unwrap_or_else(|error| panic!("merge rejected: {error} ({context})"));

        let shared = system_by_identity
            .keys()
            .filter(|identity| overlay_by_identity.contains_key(*identity))
            .count();
        replacements += shared as u64;
        additions += (overlay.len() - shared) as u64;
        assert_eq!(
            merged.len(),
            system.len() + overlay.len() - shared,
            "entry count is the union of the two layers {context}"
        );

        let mut previous: Option<Identity> = None;
        for entry in &merged {
            let identity = identity(entry);
            if let Some(previous) = &previous {
                assert!(
                    *previous < identity,
                    "merged output is strictly sorted by edge {context}"
                );
            }
            let expected = overlay_by_identity
                .get(&identity)
                .or_else(|| system_by_identity.get(&identity))
                .unwrap_or_else(|| panic!("merged row came from neither layer {context}"));
            assert_eq!(
                render(entry),
                render(expected),
                "the overlay owns every shared edge {context}"
            );
            previous = Some(identity);
        }

        // Re-applying the same overlay changes nothing, so a rebuild that
        // replays the layer stack is stable.
        let again = merge_entries(merged.clone(), overlay)
            .unwrap_or_else(|error| panic!("re-merge rejected: {error} ({context})"));
        assert_eq!(
            rendered(&again),
            rendered(&merged),
            "overlay is idempotent {context}"
        );
    }
    // Both merge outcomes have to occur, or the union property is untested.
    assert!(
        replacements > 0,
        "no overlay row ever replaced a system edge"
    );
    assert!(additions > 0, "no overlay row ever added a new edge");
}

/// The mechanism Issue #48 relies on: an overlay row that re-prices one edge
/// above its rival moves that surface below the rival in the candidate list,
/// and does not remove it. Costs here are word costs on a zero-cost connection
/// matrix, so the candidate order is the word-cost order.
fn ranking_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0004, shard, slice_seed);
    let matrix = parse_connection("matrix.tsv", CONNECTION, false).expect("fixture matrix");
    for iteration in 0..iterations {
        let seed = random.state;
        let context = format!("iteration {iteration}, seed {seed:#018x}");
        let reading = KANA_READINGS[random.usize(KANA_READINGS.len())];
        let digit = SAFE_SURFACES[random.usize(4)];
        let word = SAFE_SURFACES[4 + random.usize(SAFE_SURFACES.len() - 4)];
        // Upstream's shape: the digit is the cheap edge, so it leads.
        let cheap = 100 + random.i32(400);
        let dear = 1_000 + random.i32(400);
        let system = parse_entries(
            "system.tsv",
            &format!(
                "# license: {LICENSE}\n{HEADER}\
                 {reading}\t{digit}\t1\t1\t{cheap}\t{}\t\t\n\
                 {reading}\t{word}\t1\t1\t{dear}\t{}\t\t\n",
                cheap + 1_200,
                dear + 1_200
            ),
        )
        .unwrap_or_else(|error| panic!("system layer rejected: {error} ({context})"));
        let before = top_candidates(&system, &matrix, reading);
        assert!(
            rank(&before, digit, &context) < rank(&before, word, &context),
            "the cheaper edge leads before the overlay {context}"
        );

        // The overlay re-prices that one edge above its rival, exactly as the
        // digit rows in `data/conversion-priorities.tsv` do.
        let raised = dear + 60 + random.i32(400);
        let overlay = parse_entries(
            "overlay.tsv",
            &format!(
                "# license: {LICENSE}\n{HEADER}{reading}\t{digit}\t1\t1\t{raised}\t{}\tpredict\t\n",
                raised + 1_200
            ),
        )
        .unwrap_or_else(|error| panic!("overlay rejected: {error} ({context})"));
        let merged = merge_entries(system, overlay)
            .unwrap_or_else(|error| panic!("merge rejected: {error} ({context})"));
        assert_eq!(merged.len(), 2, "re-pricing replaces an edge {context}");
        let after = top_candidates(&merged, &matrix, reading);
        assert!(
            rank(&after, word, &context) < rank(&after, digit, &context),
            "the re-priced edge follows its rival and stays reachable {context}"
        );
    }
}

/// Where a surface sits in the candidate list. Missing is a failure: the
/// overlay re-prices an edge, it never removes one.
fn rank(candidates: &[String], surface: &str, context: &str) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate == surface)
        .unwrap_or_else(|| panic!("'{surface}' is missing from {candidates:?} ({context})"))
}

fn top_candidates(
    entries: &[SourceEntry],
    matrix: &dictc::ConnectionMatrix,
    reading: &str,
) -> Vec<String> {
    let image = compile(entries, matrix).expect("fixture image");
    let dictionary = Dictionary::parse(&image).expect("fixture dictionary");
    let mut converter = Converter::new();
    converter
        .convert(
            &dictionary,
            reading,
            ConversionOptions {
                max_candidates: 4,
                method: ConversionMethod::MultiSegment,
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion")
        .iter()
        .map(|candidate| candidate.text().to_owned())
        .collect()
}

type Identity = (String, String, u16, u16);

fn identity(entry: &SourceEntry) -> Identity {
    (
        entry.reading.clone(),
        entry.surface.clone(),
        entry.left_id,
        entry.right_id,
    )
}

fn by_identity(entries: &[SourceEntry]) -> BTreeMap<Identity, SourceEntry> {
    entries
        .iter()
        .map(|entry| (identity(entry), entry.clone()))
        .collect()
}

fn render(entry: &SourceEntry) -> String {
    let mut flags = String::new();
    for (flag, name) in [
        (EntryFlags::IT, "it"),
        (EntryFlags::PREDICTION, "predict"),
        (EntryFlags::SPELLING_CORRECTION, "correction"),
    ] {
        if entry.flags.contains(flag) {
            if !flags.is_empty() {
                flags.push(',');
            }
            flags.push_str(name);
        }
    }
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        entry.reading,
        entry.surface,
        entry.left_id,
        entry.right_id,
        entry.word_cost,
        entry.prediction_cost,
        flags,
        entry.annotation
    )
}

/// Rendered rows in a stable order, so two layers are comparable regardless of
/// the order their rows were generated in.
fn rendered(entries: &[SourceEntry]) -> Vec<String> {
    let mut rows: Vec<String> = entries.iter().map(render).collect();
    rows.sort();
    rows
}

fn hostile_document(random: &mut Random) -> String {
    let mut text = String::new();
    match random.usize(5) {
        0 => {}
        1 => text.push_str("# license: LicenseRef-Unknown-Proprietary\n"),
        2 => text.push_str("# license: MIT\n# license: MIT\n"),
        _ => text.push_str(&format!("# license: {LICENSE}\n")),
    }
    if random.usize(8) != 0 {
        text.push_str(HEADER);
    }
    let rows = random.usize(5);
    for _ in 0..rows {
        match random.usize(8) {
            0 => text.push('\n'),
            1 => text.push_str("# a note\n"),
            _ => {
                // Start from a row that parses, then damage it. Rows built
                // purely from random fragments are rejected every single time,
                // which would leave the acceptance path untested.
                let mut columns = well_formed_columns(random);
                for _ in 0..random.usize(3) {
                    let at = random.usize(columns.len());
                    columns[at] = FRAGMENTS[random.usize(FRAGMENTS.len())].to_owned();
                }
                match random.usize(8) {
                    0 => {
                        columns.pop();
                    }
                    1 => columns.push(FRAGMENTS[random.usize(FRAGMENTS.len())].to_owned()),
                    _ => {}
                }
                text.push_str(&columns.join("\t"));
                text.push('\n');
            }
        }
    }
    if random.usize(16) == 0 {
        text = text.replace('\n', "\r\n");
    }
    if random.usize(32) == 0 {
        text.push_str(&"a".repeat(MAX_FIELD_BYTES + random.usize(4)));
        text.push('\n');
    }
    text
}

/// The eight columns of a row that parses, drawn from a small identity pool so
/// that two independently generated layers overlap often.
fn well_formed_columns(random: &mut Random) -> Vec<String> {
    let word_cost = random.i32(9_000);
    let prediction = match random.usize(4) {
        0 => "-".to_owned(),
        _ => (word_cost + 1_200).to_string(),
    };
    vec![
        SAFE_READINGS[random.usize(SAFE_READINGS.len())].to_owned(),
        SAFE_SURFACES[random.usize(SAFE_SURFACES.len())].to_owned(),
        random.usize(3).to_string(),
        random.usize(3).to_string(),
        word_cost.to_string(),
        prediction,
        ["", "predict", "it,predict", "it", "correction"][random.usize(5)].to_owned(),
        SAFE_ANNOTATIONS[random.usize(SAFE_ANNOTATIONS.len())].to_owned(),
    ]
}

/// A licensed document whose rows all parse.
fn well_formed_document(random: &mut Random, rows: usize) -> String {
    let mut text = format!("# license: {LICENSE}\n{HEADER}");
    let mut seen = BTreeSet::new();
    for _ in 0..rows {
        let columns = well_formed_columns(random);
        let identity = (
            columns[0].clone(),
            columns[1].clone(),
            columns[2].clone(),
            columns[3].clone(),
        );
        if !seen.insert(identity) {
            continue;
        }
        text.push_str(&columns.join("\t"));
        text.push('\n');
    }
    text
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// A minimal xorshift64* PRNG, the same shape the other deterministic
/// campaigns in this workspace use. Reproducing a failure needs only the
/// printed seed.
struct Random {
    state: u64,
}

impl Random {
    fn seeded(seed: u64, shard: u64, slice_seed: u64) -> Self {
        let state = seed ^ shard.rotate_left(17) ^ slice_seed.rotate_left(31);
        Self {
            // xorshift64* requires a non-zero state.
            state: if state == 0 { seed | 1 } else { state },
        }
    }

    fn next(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn usize(&mut self, exclusive_end: usize) -> usize {
        if exclusive_end == 0 {
            0
        } else {
            (self.next() as usize) % exclusive_end
        }
    }

    fn i32(&mut self, exclusive_end: i32) -> i32 {
        (self.next() % u64::try_from(exclusive_end).expect("positive bound")) as i32
    }
}
