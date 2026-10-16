//! Issue #335: the verb-collocation edges are generated data, so the rules that
//! keep them safe live here. A pair table (`data/verb-collocations.tsv`: noun,
//! particle, verb) expands into one compound edge per verb form
//! (`data/verb-collocation-edges.tsv`, written by
//! `scripts/generate-verb-collocations.py`). Each edge decides what the IME
//! offers first for a whole phrase such as 窓を閉めて, so a row that drifted
//! from its pair, repeated an edge or spoke to the user would change
//! conversion silently.
//!
//! These checks read only the source files, so they run in the default test
//! pass; the ranking claims that need the compiled dictionary stay in the
//! ignored `sakura-engine` tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dictc_core::{merge_entries, parse_entries, SourceEntry};
use sakura_core::dictionary::EntryFlags;

/// A godan verb has at most seventeen forms in the expansion (six stems, six
/// te/ta forms, three polite forms, two negative forms); an ichidan verb has
/// thirteen.
const MAX_FORMS_PER_PAIR: usize = 17;
/// The longest tail after the verb stem is なかった (a stem kana plus なかった
/// is at most five kana).
const MAX_TAIL_CHARS: usize = 5;

/// The reading and surface prefix every edge of one pair shares: noun, particle
/// and verb stem.
struct Pair {
    prefix_reading: String,
    prefix_surface: String,
}

fn read_data(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn without_last_char(text: &str) -> &str {
    let (index, _) = text.char_indices().next_back().expect("a non-empty verb");
    &text[..index]
}

fn pair_lines() -> Vec<String> {
    read_data("verb-collocations.tsv")
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| {
            !line.trim().is_empty() && !line.starts_with('#') && !line.starts_with("noun_reading\t")
        })
        .map(str::to_owned)
        .collect()
}

fn pairs() -> Vec<Pair> {
    pair_lines()
        .iter()
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            assert_eq!(columns.len(), 5, "pair row needs five columns: {line}");
            let (noun_reading, noun_surface, particle) = (columns[0], columns[1], columns[2]);
            let (verb_reading, verb_surface) = (columns[3], columns[4]);
            Pair {
                prefix_reading: format!(
                    "{noun_reading}{particle}{}",
                    without_last_char(verb_reading)
                ),
                prefix_surface: format!(
                    "{noun_surface}{particle}{}",
                    without_last_char(verb_surface)
                ),
            }
        })
        .collect()
}

fn edges() -> Vec<SourceEntry> {
    let text = read_data("verb-collocation-edges.tsv");
    parse_entries("verb-collocation-edges.tsv", &text)
        .unwrap_or_else(|error| panic!("parse verb-collocation-edges.tsv: {error}"))
}

/// The verb-form tail when `entry` is an edge of `pair`: reading and surface
/// continue the shared prefix with the same kana.
fn tail_of<'a>(pair: &Pair, entry: &'a SourceEntry) -> Option<&'a str> {
    let reading_tail = entry.reading.strip_prefix(pair.prefix_reading.as_str())?;
    let surface_tail = entry.surface.strip_prefix(pair.prefix_surface.as_str())?;
    (reading_tail == surface_tail).then_some(reading_tail)
}

#[test]
fn the_pair_table_lists_each_collocation_once() {
    let lines = pair_lines();
    assert!(!lines.is_empty(), "the pair table ships no rows");
    let mut seen = BTreeSet::new();
    for line in &lines {
        assert!(seen.insert(line), "duplicate pair row: {line}");
    }
}

#[test]
fn the_expansion_has_no_duplicate_edge() {
    let entries = edges();
    assert!(!entries.is_empty(), "the edge file ships no rows");
    // The merge key is (reading, surface, left_id, right_id); a repeat inside
    // one layer is rejected outright, as the dictionary build would.
    let merged = merge_entries(Vec::new(), entries.clone()).expect("no duplicate edge");
    assert_eq!(merged.len(), entries.len());
}

#[test]
fn compound_edges_never_replace_a_priority_row() {
    let text = read_data("conversion-priorities.tsv");
    let priorities =
        parse_entries("conversion-priorities.tsv", &text).expect("parse conversion-priorities");
    let edges = edges();
    let expected = priorities.len() + edges.len();
    let merged = merge_entries(priorities, edges).expect("merge the edges onto the priorities");
    assert_eq!(
        merged.len(),
        expected,
        "a compound edge shares its identity with a calibrated row and would replace it"
    );
}

#[test]
fn every_edge_expands_a_pair_into_a_short_verb_form() {
    let pairs = pairs();
    let entries = edges();
    for entry in &entries {
        let context = format!("{}\t{}", entry.reading, entry.surface);
        let tail = pairs.iter().find_map(|pair| tail_of(pair, entry));
        let tail = tail.unwrap_or_else(|| panic!("edge traces to no pair: {context}"));
        assert!(
            tail.chars().count() <= MAX_TAIL_CHARS,
            "tail is too long for a verb form: {context}"
        );
    }
    // One pair expands to at most one reading per verb form. A pair may expand
    // to none: a form that already led its reading keeps no row.
    for pair in &pairs {
        let readings: BTreeSet<&str> = entries
            .iter()
            .filter(|entry| tail_of(pair, entry).is_some())
            .map(|entry| entry.reading.as_str())
            .collect();
        assert!(
            readings.len() <= MAX_FORMS_PER_PAIR,
            "{} expands to {} readings",
            pair.prefix_surface,
            readings.len()
        );
    }
}

#[test]
fn compound_rows_copy_the_noun_it_flag() {
    // The flag comes from the noun's own edge, so it is constant over the forms
    // of one noun + particle + verb. A row that lost it would drop the IT bonus
    // the noun carries; a row that gained it would add one the noun never had.
    let pairs = pairs();
    let entries = edges();
    let mut flag_by_noun_edge: BTreeMap<(&str, &str, u16), EntryFlags> = BTreeMap::new();
    for entry in &entries {
        assert!(
            entry.flags == EntryFlags::NONE || entry.flags == EntryFlags::IT,
            "compound row carries a flag other than it: {}\t{}",
            entry.reading,
            entry.surface
        );
        let pair = pairs
            .iter()
            .find(|pair| tail_of(pair, entry).is_some())
            .expect("every edge traces to a pair");
        let key = (
            pair.prefix_reading.as_str(),
            pair.prefix_surface.as_str(),
            entry.left_id,
        );
        let flags = *flag_by_noun_edge.entry(key).or_insert(entry.flags);
        assert_eq!(
            flags, entry.flags,
            "forms of one noun edge disagree on the it flag: {}\t{}",
            entry.reading, entry.surface
        );
    }
    assert!(
        entries.iter().any(|entry| entry.flags == EntryFlags::IT),
        "no compound row carries the IT flag of an IT noun"
    );
    assert!(
        entries.iter().any(|entry| entry.flags == EntryFlags::NONE),
        "every compound row claims the IT flag"
    );
}

#[test]
fn compound_rows_say_nothing_to_the_user() {
    for entry in edges() {
        let context = format!("{}\t{}", entry.reading, entry.surface);
        assert!(entry.word_cost > 0, "non-positive word_cost: {context}");
        // A compound is only a path through the lattice. Offering it to
        // prediction would surface half a sentence as a suggestion.
        assert!(
            !entry.flags.contains(EntryFlags::PREDICTION),
            "compound row is offered to prediction: {context}"
        );
        assert_eq!(
            entry.prediction_cost,
            i32::MAX,
            "compound row carries a prediction cost: {context}"
        );
        // The annotation column is shown to the user as a candidate note.
        assert!(
            entry.annotation.is_empty(),
            "compound row carries a user-visible note: {context}"
        );
    }
}
