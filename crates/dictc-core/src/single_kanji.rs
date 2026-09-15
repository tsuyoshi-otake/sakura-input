//! Single-kanji table generation from the pinned Mozc single-kanji data.
//!
//! `src/data/single_kanji/single_kanji.tsv` maps a reading to the characters
//! that reading names, already in Mozc's preference order.  `variant_rule.txt`
//! groups characters under a relation such as 旧字体 or 印刷標準字体 together
//! with the character they vary from.
//!
//! Mozc consumes both through `rewriter/single_kanji_rewriter`, which appends
//! these characters to a finished candidate list.  Sakura keeps that shape: a
//! reading like こう names 315 characters, so giving each a lattice edge would
//! spend the entire node budget re-deriving an answer the n-best search has
//! already produced.  This module therefore compiles the two files into a
//! standalone lookup table rather than into dictionary entries.
//!
//! Parsing is strict.  A row this module cannot interpret is an error, not a
//! skip, with one bounded exception recorded in [`MAX_REJECTED_READINGS`].

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use sakura_core::dictionary::SingleKanjiVariantKind;

use crate::Error;

/// How many rows may be rejected for an untypable reading before the build
/// fails.
///
/// The pinned revision has exactly two: `はｎ` (a mistyped `はん`, whose 判 is
/// already listed under the correct reading, so dropping it loses nothing) and
/// `びん(表外)` (a reading with its own 表外 marker inlined, which no user can
/// type).  Keeping the allowance this tight means a source bump that
/// introduces new malformed rows stops the build instead of quietly shrinking
/// the table.
pub const MAX_REJECTED_READINGS: usize = 2;

/// Reading to the characters it names, in source preference order.
pub type SingleKanjiReadings = BTreeMap<String, Vec<char>>;

/// Variant character to the character it varies from and how.
pub type SingleKanjiVariants = BTreeMap<char, (char, SingleKanjiVariantKind)>;

/// A compiled single-kanji lookup, ready for the image encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleKanjiTable {
    /// Reading to characters in source preference order.  `BTreeMap` gives the
    /// encoder the sorted-by-reading order the image index requires.
    readings: SingleKanjiReadings,
    /// Variant character to the character it varies from and how.
    variants: SingleKanjiVariants,
    report: SingleKanjiReport,
}

/// What the build did with the source, for the build report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SingleKanjiReport {
    pub readings: usize,
    pub characters: usize,
    pub variants: usize,
    /// Readings rejected as untypable, kept verbatim so a source bump names
    /// what changed instead of only counting it.
    pub rejected_readings: Vec<String>,
    /// Characters listed under more than one variant rule.  The first rule in
    /// source order wins, matching the order Mozc's generator reads them in.
    pub variant_conflicts: usize,
}

impl SingleKanjiTable {
    pub fn readings(&self) -> &SingleKanjiReadings {
        &self.readings
    }

    pub fn variants(&self) -> &SingleKanjiVariants {
        &self.variants
    }

    pub fn report(&self) -> &SingleKanjiReport {
        &self.report
    }

    /// Parses both pinned source files into one table.
    pub fn build(
        single_kanji_source: &str,
        single_kanji_text: &str,
        variant_source: &str,
        variant_text: &str,
    ) -> Result<Self, Error> {
        let (readings, rejected_readings) =
            parse_single_kanji(single_kanji_source, single_kanji_text)?;
        if rejected_readings.len() > MAX_REJECTED_READINGS {
            return Err(Error::at(
                single_kanji_source,
                0,
                format!(
                    "{} rows have an untypable reading, more than the {MAX_REJECTED_READINGS} the \
                     pinned source is known to carry: {}",
                    rejected_readings.len(),
                    rejected_readings.join(", ")
                ),
            ));
        }
        let (variants, variant_conflicts) = parse_variant_rules(variant_source, variant_text)?;
        let characters = readings.values().map(Vec::len).sum();
        let report = SingleKanjiReport {
            readings: readings.len(),
            characters,
            variants: variants.len(),
            rejected_readings,
            variant_conflicts,
        };
        Ok(Self {
            readings,
            variants,
            report,
        })
    }
}

/// Whether a reading is one a user can actually produce with kana input.
///
/// The source is a hand-maintained file and carries a small number of rows
/// whose reading column holds an editorial marker or a typo.  Such a reading
/// can never be looked up, so admitting it would only grow the index.
fn is_typable_reading(reading: &str) -> bool {
    !reading.is_empty()
        && reading
            .chars()
            .all(|ch| matches!(ch, '\u{3041}'..='\u{3096}' | 'ー'))
}

fn parse_single_kanji(
    source: &str,
    text: &str,
) -> Result<(SingleKanjiReadings, Vec<String>), Error> {
    let mut readings: SingleKanjiReadings = BTreeMap::new();
    let mut rejected = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (reading, characters) = line
            .split_once('\t')
            .ok_or_else(|| Error::at(source, number, "expected 'reading<TAB>characters'"))?;
        if characters.contains('\t') {
            return Err(Error::at(source, number, "row has more than two columns"));
        }
        if characters.is_empty() {
            return Err(Error::at(
                source,
                number,
                format!("reading '{reading}' lists no characters"),
            ));
        }
        if !is_typable_reading(reading) {
            rejected.push(reading.to_owned());
            continue;
        }
        let mut seen = Vec::with_capacity(characters.chars().count());
        for character in characters.chars() {
            // A repeated character would produce a duplicate candidate, and a
            // whitespace or control character would produce an unselectable
            // one.  Both mean the row is not what this parser assumes.
            if character.is_whitespace() || character.is_control() {
                return Err(Error::at(
                    source,
                    number,
                    format!("reading '{reading}' lists a whitespace or control character"),
                ));
            }
            if seen.contains(&character) {
                return Err(Error::at(
                    source,
                    number,
                    format!("reading '{reading}' lists '{character}' twice"),
                ));
            }
            seen.push(character);
        }
        if readings.insert(reading.to_owned(), seen).is_some() {
            return Err(Error::at(
                source,
                number,
                format!("duplicate reading '{reading}'"),
            ));
        }
    }
    if readings.is_empty() {
        return Err(Error::at(source, 0, "source lists no single kanji"));
    }
    Ok((readings, rejected))
}

fn parse_variant_rules(source: &str, text: &str) -> Result<(SingleKanjiVariants, usize), Error> {
    let mut variants: SingleKanjiVariants = BTreeMap::new();
    let mut conflicts = 0usize;
    let mut kind = None;
    for (index, line) in text.lines().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let number = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((variant, original)) = line.split_once('\t') else {
            // A line without a tab opens a new rule group.
            kind = Some(parse_variant_kind(source, number, line)?);
            continue;
        };
        let kind = kind.ok_or_else(|| {
            Error::at(source, number, "variant pair appears before any rule group")
        })?;
        let variant = single_character(source, number, variant, "variant")?;
        let original = single_character(source, number, original, "original")?;
        if variant == original {
            return Err(Error::at(
                source,
                number,
                format!("'{variant}' is listed as a variant of itself"),
            ));
        }
        // The source lists some characters under more than one rule, and under
        // one rule with more than one original.  Keeping the first occurrence
        // matches the order the generator reads the file in, and guarantees a
        // candidate can never carry two contradictory notes.
        match variants.entry(variant) {
            Entry::Vacant(slot) => {
                slot.insert((original, kind));
            }
            Entry::Occupied(_) => conflicts += 1,
        }
    }
    if variants.is_empty() {
        return Err(Error::at(source, 0, "source lists no variant rules"));
    }
    Ok((variants, conflicts))
}

fn parse_variant_kind(
    source: &str,
    line: usize,
    name: &str,
) -> Result<SingleKanjiVariantKind, Error> {
    use SingleKanjiVariantKind as Kind;

    // Matching on the exact source names keeps a renamed or added upstream rule
    // group a build error rather than an unlabelled note.
    Ok(match name {
        "異体字" => Kind::Itaiji,
        "印刷標準字体" => Kind::PrintStandard,
        "簡易慣用字体" => Kind::SimplifiedConventional,
        "旧字体" => Kind::OldForm,
        "略字" => Kind::Abbreviated,
        "正字" => Kind::OrthodoxForm,
        "俗字" => Kind::PopularForm,
        "別字" => Kind::DistinctCharacter,
        "本字" => Kind::OriginalForm,
        other => {
            return Err(Error::at(
                source,
                line,
                format!("unknown variant rule group '{other}'"),
            ))
        }
    })
}

fn single_character(source: &str, line: usize, text: &str, field: &str) -> Result<char, Error> {
    let mut characters = text.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) if !character.is_whitespace() && !character.is_control() => {
            Ok(character)
        }
        _ => Err(Error::at(
            source,
            line,
            format!("{field} field must hold exactly one character, got '{text}'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KANJI_SOURCE: &str = "single_kanji.tsv";
    const VARIANT_SOURCE: &str = "variant_rule.txt";

    const VARIANTS: &str = "# comment\n異体字\n髙\t高\n\n旧字体\n緣\t縁\n";

    fn build(kanji: &str, variants: &str) -> Result<SingleKanjiTable, Error> {
        SingleKanjiTable::build(KANJI_SOURCE, kanji, VARIANT_SOURCE, variants)
    }

    #[test]
    fn readings_keep_their_source_preference_order() {
        let table = build("ひ\t火日比\nこう\t口工公\n", VARIANTS).expect("table");
        assert_eq!(table.readings()["ひ"], ['火', '日', '比']);
        assert_eq!(table.readings()["こう"], ['口', '工', '公']);
        assert_eq!(table.report().characters, 6);
        assert_eq!(table.report().readings, 2);
    }

    #[test]
    fn variant_rules_carry_their_group_and_original() {
        let table = build("ひ\t火\n", VARIANTS).expect("table");
        assert_eq!(
            table.variants()[&'髙'],
            ('高', SingleKanjiVariantKind::Itaiji)
        );
        assert_eq!(
            table.variants()[&'緣'],
            ('縁', SingleKanjiVariantKind::OldForm)
        );
        assert_eq!(table.report().variants, 2);
        assert_eq!(table.report().variant_conflicts, 0);
    }

    #[test]
    fn a_character_under_two_rules_keeps_the_first_and_is_counted() {
        let variants = "俗字\n駈\t駆\n\n異体字\n駈\t驅\n";
        let table = build("ひ\t火\n", variants).expect("table");
        assert_eq!(
            table.variants()[&'駈'],
            ('駆', SingleKanjiVariantKind::PopularForm)
        );
        assert_eq!(table.report().variant_conflicts, 1);
    }

    #[test]
    fn an_untypable_reading_is_rejected_and_named() {
        let table = build("はｎ\t判\nひ\t火\n", VARIANTS).expect("table");
        assert!(!table.readings().contains_key("はｎ"));
        assert_eq!(table.report().rejected_readings, ["はｎ"]);
    }

    #[test]
    fn too_many_untypable_readings_fail_the_build() {
        let kanji = "はｎ\t判\nびん(表外)\t民\nこ(表外)\t子\nひ\t火\n";
        let error = build(kanji, VARIANTS).expect_err("rejected");
        assert!(error.to_string().contains("untypable reading"), "{error}");
    }

    #[test]
    fn a_repeated_character_is_an_error_not_a_silent_dedupe() {
        let error = build("ひ\t火日火\n", VARIANTS).expect_err("rejected");
        assert!(error.to_string().contains("lists '火' twice"), "{error}");
    }

    #[test]
    fn an_unknown_rule_group_is_an_error() {
        let error = build("ひ\t火\n", "新字体\n体\t體\n").expect_err("rejected");
        assert!(
            error.to_string().contains("unknown variant rule group"),
            "{error}"
        );
    }

    #[test]
    fn a_pair_before_any_group_is_an_error() {
        let error = build("ひ\t火\n", "髙\t高\n").expect_err("rejected");
        assert!(
            error.to_string().contains("before any rule group"),
            "{error}"
        );
    }

    #[test]
    fn a_self_referential_variant_is_an_error() {
        let error = build("ひ\t火\n", "異体字\n高\t高\n").expect_err("rejected");
        assert!(error.to_string().contains("variant of itself"), "{error}");
    }

    #[test]
    fn a_multi_character_variant_field_is_an_error() {
        let error = build("ひ\t火\n", "異体字\n髙々\t高\n").expect_err("rejected");
        assert!(
            error.to_string().contains("exactly one character"),
            "{error}"
        );
    }

    #[test]
    fn a_reading_with_no_characters_is_an_error() {
        let error = build("ひ\t\n", VARIANTS).expect_err("rejected");
        assert!(error.to_string().contains("lists no characters"), "{error}");
    }

    #[test]
    fn a_row_without_a_tab_is_an_error() {
        let error = build("ひ火\n", VARIANTS).expect_err("rejected");
        assert!(
            error.to_string().contains("reading<TAB>characters"),
            "{error}"
        );
    }

    #[test]
    fn non_bmp_characters_survive_parsing() {
        let table = build("しか\t𠮟叱\n", VARIANTS).expect("table");
        assert_eq!(table.readings()["しか"], ['𠮟', '叱']);
    }
}
