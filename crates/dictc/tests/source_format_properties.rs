//! Fixed-seed property campaigns for the Mozc shard parser and both
//! connection-matrix parsers, the formats Issue #49's defect class did not
//! reach.
//!
//! As elsewhere in this workspace there is no external PBT crate: the
//! generators are a xorshift64* PRNG over a corpus of hostile and well-formed
//! fields, and every failure prints the seed and iteration, which reproduces it
//! exactly. The ignored target takes the same shard/iteration environment
//! variables as the other long campaigns.

use dictc::{
    entries_to_tsv, parse_connection, parse_mozc_connection, parse_mozc_entries, ConnectionMatrix,
};
use sakura_core::dictionary::EntryFlags;

const MAX_FIELD_BYTES: usize = 1536;
/// The prediction budget and minimum reading length `parse_mozc_entries` uses.
const PREDICTION_COST_BUDGET: i32 = 6_000;
const MIN_PREDICTION_READING_CHARS: usize = 2;

const DEFAULT_MOZC_ITERATIONS: u64 = 4_000;
const DEFAULT_MATRIX_ITERATIONS: u64 = 800;
const DEFAULT_HOSTILE_MATRIX_ITERATIONS: u64 = 3_000;

/// Fragments a hostile row is damaged with: schema keywords, boundary numbers,
/// and the characters that decide a branch inside these parsers.
const FRAGMENTS: &[&str] = &[
    "",
    "あい",
    "藍",
    "SPELLING_CORRECTION",
    "spelling_correction",
    "RENDANGO",
    "0",
    "-1",
    "6000",
    "6001",
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
    "藍\r色",
    "classes",
    "default",
    "cost",
    "🌸",
    "a",
];

const SAFE_READINGS: &[&str] = &["あ", "あい", "あいう", "さくら", "ｱ", "🌸"];
const SAFE_SURFACES: &[&str] = &["亜", "藍", "藍色", "桜", "Sakura", "🌸"];

#[test]
fn hostile_mozc_rows_are_rejected_or_hold_every_schema_invariant() {
    mozc_entry_campaign(DEFAULT_MOZC_ITERATIONS, 0, 0);
}

#[test]
fn both_matrix_formats_describe_the_same_costs_cell_for_cell() {
    matrix_campaign(DEFAULT_MATRIX_ITERATIONS, 0, 0);
}

#[test]
fn hostile_matrices_are_rejected_without_panicking() {
    hostile_matrix_campaign(DEFAULT_HOSTILE_MATRIX_ITERATIONS, 0, 0);
}

#[test]
#[ignore = "long deterministic campaign; set SAKURA_FUZZ_ITERS, SAKURA_FUZZ_SHARD, and SAKURA_FUZZ_SEED"]
fn sharded_source_format_campaign() {
    let iterations = env_u64("SAKURA_FUZZ_ITERS").unwrap_or(200_000);
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let slice_seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    mozc_entry_campaign(iterations, shard, slice_seed);
    matrix_campaign(iterations / 20, shard, slice_seed);
    hostile_matrix_campaign(iterations / 2, shard, slice_seed);
}

/// No Mozc shard, however malformed, may panic the parser; and any row it does
/// accept must satisfy the schema, including the derived prediction rule that
/// decides whether the row is offered as a prediction at all.
fn mozc_entry_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0005, shard, slice_seed);
    let mut accepted_rows = 0u64;
    let mut rejected = 0u64;
    let mut predicted = 0u64;
    let mut unpredicted = 0u64;
    let mut corrections = 0u64;
    let mut unwritable = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let text = hostile_mozc_shard(&mut random);
        let parsed = std::panic::catch_unwind(|| parse_mozc_entries("shard.txt", &text));
        let Ok(parsed) = parsed else {
            panic!("parser panicked at iteration {iteration}, seed {seed:#018x}, text {text:?}");
        };
        let Ok(entries) = parsed else {
            rejected += 1;
            continue;
        };
        accepted_rows += entries.len() as u64;
        for entry in &entries {
            let context = format!("iteration {iteration}, seed {seed:#018x}, entry {entry:?}");
            assert!(!entry.reading.is_empty(), "empty reading: {context}");
            assert!(!entry.surface.is_empty(), "empty surface: {context}");
            assert!(
                !entry.reading.chars().any(char::is_control),
                "control character in reading: {context}"
            );
            for field in [&entry.reading, &entry.surface] {
                assert!(!field.contains('\u{0}'), "NUL in a field: {context}");
                assert!(
                    field.len() <= MAX_FIELD_BYTES,
                    "field over the preedit budget: {context}"
                );
                assert!(
                    !field.contains('\t'),
                    "a tab reached a split field: {context}"
                );
            }
            assert!(entry.word_cost >= 0, "negative word_cost: {context}");
            assert!(
                entry.annotation.is_empty(),
                "the Mozc format carries no annotation: {context}"
            );

            // The prediction rule, restated. A row is prediction-worthy exactly
            // when it is not a spelling correction, is inside the cost budget,
            // and has a reading long enough to be worth predicting from.
            let correction = entry.flags.contains(EntryFlags::SPELLING_CORRECTION);
            let worthy = !correction
                && entry.word_cost <= PREDICTION_COST_BUDGET
                && entry.reading.chars().count() >= MIN_PREDICTION_READING_CHARS;
            assert_eq!(
                entry.flags.contains(EntryFlags::PREDICTION),
                worthy,
                "prediction flag disagrees with the rule: {context}"
            );
            assert_eq!(
                entry.prediction_cost,
                if worthy {
                    entry.word_cost + 1_200
                } else {
                    i32::MAX
                },
                "prediction cost disagrees with the rule: {context}"
            );
            assert!(
                !entry.flags.contains(EntryFlags::IT),
                "the Mozc format never sets the IT flag: {context}"
            );
            if correction {
                corrections += 1;
            }
            if worthy {
                predicted += 1;
            } else {
                unpredicted += 1;
            }
        }
        // A Mozc row may legitimately hold a bare CR inside a field -- only a
        // trailing one is trimmed, and only the reading rejects controls. Such
        // a row cannot be written back to a Sakura TSV, and the importers stop
        // there rather than emitting a file that no longer reparses.
        let writable = entries.iter().all(|entry| {
            !entry.reading.contains(['\t', '\r', '\n'])
                && !entry.surface.contains(['\t', '\r', '\n'])
        });
        if !writable {
            unwritable += 1;
        }
        assert_eq!(
            entries_to_tsv(&entries, "LicenseRef-Mozc-Dictionary").is_ok(),
            writable,
            "writer and parser disagree at iteration {iteration}, seed {seed:#018x}"
        );
    }
    // A campaign that only ever generated garbage, or only ever generated clean
    // rows, would pass while testing nothing.
    assert!(accepted_rows > 0, "no shard was ever accepted");
    assert!(rejected > 0, "no shard was ever rejected");
    assert!(predicted > 0, "no accepted row was prediction-worthy");
    assert!(
        unpredicted > 0,
        "no accepted row was held out of prediction"
    );
    assert!(corrections > 0, "no spelling-correction row was generated");
    assert!(
        unwritable > 0,
        "no accepted row ever carried a separator inside a field"
    );
}

/// The two matrix formats describe the same thing, so the same costs written
/// each way must read back identically for every cell. Neither parser can
/// reveal a row/column mix-up on its own.
fn matrix_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0006, shard, slice_seed);
    let mut overridden_cells = 0u64;
    let mut default_cells = 0u64;
    let mut multi_class = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let context = format!("iteration {iteration}, seed {seed:#018x}");
        let classes = 1 + random.usize(4);
        let default = random.usize(50) as u16;
        let mut cells = vec![default; classes * classes];
        let mut overrides = Vec::new();
        let mut overridden = vec![false; classes * classes];
        for _ in 0..random.usize(classes * classes + 1) {
            let right = random.usize(classes);
            let left = random.usize(classes);
            let index = right * classes + left;
            if overridden[index] {
                continue;
            }
            overridden[index] = true;
            let cost = 100 + random.usize(400) as u16;
            cells[index] = cost;
            overrides.push(format!("cost\t{right}\t{left}\t{cost}\n"));
        }
        if classes > 1 {
            multi_class += 1;
        }
        overridden_cells += overridden.iter().filter(|flag| **flag).count() as u64;
        default_cells += overridden.iter().filter(|flag| !**flag).count() as u64;

        let sakura = format!(
            "# license: MIT\nclasses\t{classes}\ndefault\t{default}\n{}",
            overrides.concat()
        );
        let mozc = format!(
            "{classes}\n{}",
            cells
                .iter()
                .map(|cost| format!("{cost}\n"))
                .collect::<String>()
        );
        let from_sakura = parse_connection("connection.tsv", &sakura, false)
            .unwrap_or_else(|error| panic!("Sakura matrix rejected: {error} ({context})"));
        let from_mozc = parse_mozc_connection("matrix.txt", &mozc, false)
            .unwrap_or_else(|error| panic!("Mozc matrix rejected: {error} ({context})"));

        let classes = classes as u16;
        assert_eq!(from_sakura.class_count(), classes, "class count {context}");
        assert_eq!(from_mozc.class_count(), classes, "class count {context}");
        for right in 0..classes {
            for left in 0..classes {
                let expected = cells[usize::from(right) * usize::from(classes) + usize::from(left)];
                assert_eq!(
                    from_sakura.cost(right, left),
                    Some(expected),
                    "Sakura cell ({right},{left}) {context}"
                );
                assert_eq!(
                    from_mozc.cost(right, left),
                    Some(expected),
                    "Mozc cell ({right},{left}) {context}"
                );
            }
        }
        // A lookup outside the taxonomy is absent rather than wrapped around to
        // a neighbouring row.
        for matrix in [&from_sakura, &from_mozc] {
            assert_eq!(matrix.cost(classes, 0), None, "row overrun {context}");
            assert_eq!(matrix.cost(0, classes), None, "column overrun {context}");
        }
        assert!(
            same_costs(&from_sakura, &from_mozc),
            "formats agree {context}"
        );
    }
    assert!(overridden_cells > 0, "no cell was ever overridden");
    assert!(default_cells > 0, "no cell ever kept the default");
    assert!(multi_class > 0, "every generated matrix had a single class");
}

/// Neither matrix parser may panic on a damaged document, whichever format the
/// bytes were meant to be.
fn hostile_matrix_campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let mut random = Random::seeded(0x0a11_5eed_00c2_0007, shard, slice_seed);
    let mut accepted = 0u64;
    let mut rejected = 0u64;
    for iteration in 0..iterations {
        let seed = random.state;
        let text = hostile_matrix(&mut random);
        for frozen in [true, false] {
            for sakura in [true, false] {
                let parsed = std::panic::catch_unwind(|| {
                    if sakura {
                        parse_connection("connection.tsv", &text, frozen).map(|m| m.class_count())
                    } else {
                        parse_mozc_connection("matrix.txt", &text, frozen).map(|m| m.class_count())
                    }
                });
                let Ok(parsed) = parsed else {
                    panic!("matrix parser panicked at iteration {iteration}, seed {seed:#018x}, sakura {sakura}, frozen {frozen}, text {text:?}");
                };
                match parsed {
                    Ok(classes) => {
                        accepted += 1;
                        assert!(
                            classes > 0,
                            "an accepted matrix has at least one class at iteration {iteration}, seed {seed:#018x}"
                        );
                    }
                    Err(_) => rejected += 1,
                }
            }
        }
    }
    assert!(accepted > 0, "no matrix was ever accepted");
    assert!(rejected > 0, "no matrix was ever rejected");
}

fn same_costs(left: &ConnectionMatrix, right: &ConnectionMatrix) -> bool {
    if left.class_count() != right.class_count() {
        return false;
    }
    let classes = left.class_count();
    (0..classes)
        .all(|row| (0..classes).all(|column| left.cost(row, column) == right.cost(row, column)))
}

/// The six columns of a Mozc row that parses.
fn well_formed_mozc_columns(random: &mut Random) -> Vec<String> {
    let mut columns = vec![
        SAFE_READINGS[random.usize(SAFE_READINGS.len())].to_owned(),
        random.usize(4).to_string(),
        random.usize(4).to_string(),
        random.usize(9_000).to_string(),
        SAFE_SURFACES[random.usize(SAFE_SURFACES.len())].to_owned(),
    ];
    match random.usize(4) {
        0 => columns.push(String::new()),
        1 => columns.push("SPELLING_CORRECTION".to_owned()),
        _ => {}
    }
    columns
}

/// A shard built from rows that parse, then damaged. Rows assembled purely from
/// random fragments are rejected every time, which would leave the acceptance
/// path -- and the prediction rule with it -- untested.
fn hostile_mozc_shard(random: &mut Random) -> String {
    let mut text = String::new();
    for _ in 0..random.usize(4) {
        match random.usize(8) {
            0 => text.push('\n'),
            1 => text.push_str("# upstream note\n"),
            _ => {
                let mut columns = well_formed_mozc_columns(random);
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
    text
}

/// A document that is plausibly either matrix format, then damaged.
fn hostile_matrix(random: &mut Random) -> String {
    let classes = 1 + random.usize(3);
    let mut lines: Vec<String> = Vec::new();
    if random.usize(4) != 0 {
        lines.push("# license: MIT".to_owned());
    }
    if random.usize(2) == 0 {
        // Sakura shape.
        lines.push(format!("classes\t{classes}"));
        lines.push(format!("default\t{}", random.usize(50)));
        for _ in 0..random.usize(4) {
            lines.push(format!(
                "cost\t{}\t{}\t{}",
                random.usize(classes + 1),
                random.usize(classes + 1),
                random.usize(600)
            ));
        }
    } else {
        // Mozc shape.
        lines.push(classes.to_string());
        for _ in 0..(classes * classes) {
            lines.push(random.usize(600).to_string());
        }
    }
    for _ in 0..random.usize(3) {
        let at = random.usize(lines.len().max(1));
        if at < lines.len() {
            lines[at] = FRAGMENTS[random.usize(FRAGMENTS.len())].to_owned();
        }
    }
    if random.usize(8) == 0 && !lines.is_empty() {
        lines.remove(random.usize(lines.len()));
    }
    if random.usize(8) == 0 {
        lines.push(FRAGMENTS[random.usize(FRAGMENTS.len())].to_owned());
    }
    let mut text = lines.join("\n");
    text.push('\n');
    if random.usize(16) == 0 {
        text = text.replace('\n', "\r\n");
    }
    text
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// A minimal xorshift64* PRNG, the same shape the other deterministic campaigns
/// in this workspace use. Reproducing a failure needs only the printed seed.
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
}
