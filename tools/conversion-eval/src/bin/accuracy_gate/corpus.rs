//! The committed accuracy corpora and their loader rules.
//!
//! `conversion.tsv` is `id, slice, reading, expected, variants` (variants are
//! `|`-separated). `prediction.tsv` is `id, reading, surface, slice, pos`. Both
//! start with a header line. The loader rejects what would silently inflate or
//! hide a measurement: a variant equal to the reading (a kana answer that
//! passes for a conversion that never happened), duplicate ids, duplicate
//! readings, unknown slices and wrong column counts.

use std::collections::BTreeSet;

pub const SLICES: [&str; 8] = [
    "chat",
    "email",
    "general",
    "grammar",
    "homophone",
    "it",
    "loanword",
    "numbers",
];

/// Index of the all-slices row in every per-slice table.
pub const ALL: usize = SLICES.len();

const CONVERSION_HEADER: &str = "id\tslice\treading\texpected\tvariants";
const PREDICTION_HEADER: &str = "id\treading\tsurface\tslice\tpos";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionRow {
    pub id: String,
    pub slice: usize,
    pub reading: String,
    pub expected: String,
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionRow {
    pub id: String,
    pub reading: String,
    pub surface: String,
    pub slice: usize,
    pub pos: String,
}

impl PredictionRow {
    /// Keystrokes needed to type the whole reading.
    pub fn keystrokes(&self) -> usize {
        self.reading.chars().count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corpus {
    pub conversion: Vec<ConversionRow>,
    pub prediction: Vec<PredictionRow>,
}

pub fn label(slice: usize) -> &'static str {
    SLICES.get(slice).copied().unwrap_or("ALL")
}

fn parse_slice(name: &str, context: &str) -> Result<usize, String> {
    SLICES
        .iter()
        .position(|slice| *slice == name)
        .ok_or_else(|| format!("{context}: unknown slice '{name}'"))
}

fn data_lines<'a>(
    source: &'a str,
    header: &str,
    file: &str,
) -> Result<impl Iterator<Item = (usize, Vec<&'a str>)>, String> {
    let mut lines = source.lines().enumerate();
    match lines.next() {
        Some((_, first)) if first == header => {}
        _ => return Err(format!("{file}:1: the header must be '{header}'")),
    }
    Ok(lines.map(|(index, line)| (index + 1, line.split('\t').collect())))
}

pub fn parse_conversion(source: &str) -> Result<Vec<ConversionRow>, String> {
    let file = "conversion.tsv";
    let mut ids = BTreeSet::new();
    let mut readings = BTreeSet::new();
    let mut rows = Vec::new();
    for (number, fields) in data_lines(source, CONVERSION_HEADER, file)? {
        let context = format!("{file}:{number}");
        let [id, slice, reading, expected, variants] = fields[..] else {
            return Err(format!(
                "{context}: expected 5 columns, found {}",
                fields.len()
            ));
        };
        if id.is_empty() || reading.is_empty() || expected.is_empty() {
            return Err(format!(
                "{context}: id, reading and expected must not be empty"
            ));
        }
        let slice = parse_slice(slice, &context)?;
        if !ids.insert(id) {
            return Err(format!("{context}: duplicate id '{id}'"));
        }
        if !readings.insert(reading) {
            return Err(format!("{context}: duplicate reading '{reading}' ({id})"));
        }
        let variants: Vec<String> = if variants.is_empty() {
            Vec::new()
        } else {
            variants.split('|').map(str::to_owned).collect()
        };
        for variant in &variants {
            if variant.is_empty() {
                return Err(format!("{context}: empty variant in '{id}'"));
            }
            if variant == reading {
                return Err(format!(
                    "{context}: variant of '{id}' equals the reading; a kana answer is a miss, not a spelling variant"
                ));
            }
        }
        rows.push(ConversionRow {
            id: id.to_owned(),
            slice,
            reading: reading.to_owned(),
            expected: expected.to_owned(),
            variants,
        });
    }
    Ok(rows)
}

pub fn parse_prediction(source: &str) -> Result<Vec<PredictionRow>, String> {
    let file = "prediction.tsv";
    let mut ids = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    let mut rows = Vec::new();
    for (number, fields) in data_lines(source, PREDICTION_HEADER, file)? {
        let context = format!("{file}:{number}");
        let [id, reading, surface, slice, pos] = fields[..] else {
            return Err(format!(
                "{context}: expected 5 columns, found {}",
                fields.len()
            ));
        };
        if id.is_empty() || reading.is_empty() || surface.is_empty() || pos.is_empty() {
            return Err(format!("{context}: no column may be empty"));
        }
        let slice = parse_slice(slice, &context)?;
        if !ids.insert(id) {
            return Err(format!("{context}: duplicate id '{id}'"));
        }
        if !pairs.insert((reading, surface)) {
            return Err(format!(
                "{context}: duplicate reading and surface in '{id}'"
            ));
        }
        rows.push(PredictionRow {
            id: id.to_owned(),
            reading: reading.to_owned(),
            surface: surface.to_owned(),
            slice,
            pos: pos.to_owned(),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "id\tslice\treading\texpected\tvariants\n";

    fn conversion(rows: &str) -> Result<Vec<ConversionRow>, String> {
        parse_conversion(&format!("{HEADER}{rows}"))
    }

    #[test]
    fn accepts_rows_with_and_without_variants() {
        let rows = conversion(
            "a-1\tchat\tわかる\t分かる\t解る|判る\n\
             a-2\tit\tぷっしゅ\tプッシュ\t\n",
        )
        .expect("rows");
        assert_eq!(rows[0].variants, ["解る", "判る"]);
        assert!(rows[1].variants.is_empty());
        assert_eq!(label(rows[1].slice), "it");
    }

    #[test]
    fn rejects_a_variant_equal_to_the_reading() {
        let error = conversion("a-1\tchat\tわかりました\t分かりました\tわかりました\n")
            .expect_err("variant equal to the reading");
        assert!(error.contains("equals the reading"), "{error}");
        let error = conversion("a-1\tchat\tわかる\t分かる\t解る|わかる\n")
            .expect_err("second variant equal to the reading");
        assert!(error.contains("equals the reading"), "{error}");
    }

    #[test]
    fn rejects_duplicate_ids_and_duplicate_readings() {
        let error = conversion("a-1\tchat\tあ\t亜\t\na-1\tchat\tい\t伊\t\n").expect_err("id");
        assert!(error.contains("duplicate id"), "{error}");
        let error = conversion("a-1\tchat\tあ\t亜\t\na-2\temail\tあ\t阿\t\n").expect_err("reading");
        assert!(error.contains("duplicate reading"), "{error}");
    }

    #[test]
    fn rejects_unknown_slices_columns_and_header() {
        let error = conversion("a-1\tsports\tあ\t亜\t\n").expect_err("slice");
        assert!(error.contains("unknown slice"), "{error}");
        let error = conversion("a-1\tchat\tあ\t亜\n").expect_err("columns");
        assert!(error.contains("expected 5 columns"), "{error}");
        let error = parse_conversion("a-1\tchat\tあ\t亜\t\n").expect_err("header");
        assert!(error.contains("header"), "{error}");
        let error = conversion("a-1\tchat\tあ\t亜\t|\n").expect_err("empty variant");
        assert!(error.contains("empty variant"), "{error}");
    }

    #[test]
    fn prediction_allows_one_reading_with_two_surfaces_but_not_twice() {
        let header = "id\treading\tsurface\tslice\tpos\n";
        let rows = parse_prediction(&format!(
            "{header}p1\tはやい\t早い\tchat\tadj\np2\tはやい\t速い\tchat\tadj\n"
        ))
        .expect("rows");
        assert_eq!(rows[1].keystrokes(), 3);
        let error = parse_prediction(&format!(
            "{header}p1\tはやい\t早い\tchat\tadj\np2\tはやい\t早い\tit\tadj\n"
        ))
        .expect_err("duplicate pair");
        assert!(error.contains("duplicate reading and surface"), "{error}");
        let error = parse_prediction(&format!("{header}p1\tあ\t亜\tchat\n")).expect_err("columns");
        assert!(error.contains("expected 5 columns"), "{error}");
    }
}
