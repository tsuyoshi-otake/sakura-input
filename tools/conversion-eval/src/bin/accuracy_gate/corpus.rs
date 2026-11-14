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
const CONTEXT_HEADER: &str =
    "id\tpair\toneshot_reading\toneshot_expected\tclue_reading\tclue_surface\treading\texpected\tvariants";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionRow {
    pub id: String,
    pub slice: usize,
    pub reading: String,
    pub expected: String,
    pub variants: Vec<String>,
}

/// One side of a homophone pair measured twice (#471): once as the whole
/// `oneshot_reading` in a fresh session, and once after the engine has
/// committed `clue_reading` as `clue_surface` — the production carry of
/// `initial_right_id` plus the cross-commit bridge is applied to `reading`.
/// `pair` joins the two rows whose contexts point at opposite answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRow {
    pub id: String,
    pub pair: String,
    pub oneshot_reading: String,
    pub oneshot_expected: String,
    pub clue_reading: String,
    pub clue_surface: String,
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
    /// Empty when the corpus directory has no context file.
    pub context: Vec<ContextRow>,
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

/// `context.tsv` rows. A pair is exactly two rows; the loader rejects shapes
/// that would inflate or silently drop a side: duplicate ids, a pair with
/// one or three rows, an empty clue (the whole point of the file), and a
/// variant equal to the reading.
pub fn parse_context(source: &str) -> Result<Vec<ContextRow>, String> {
    let file = "context.tsv";
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut rows = Vec::new();
    for (number, fields) in data_lines(source, CONTEXT_HEADER, file)? {
        let context = format!("{file}:{number}");
        let [id, pair, oneshot_reading, oneshot_expected, clue_reading, clue_surface, reading, expected, variants] =
            fields[..]
        else {
            return Err(format!(
                "{context}: expected 9 columns, found {}",
                fields.len()
            ));
        };
        if [
            id,
            pair,
            oneshot_reading,
            oneshot_expected,
            clue_reading,
            clue_surface,
            reading,
            expected,
        ]
        .iter()
        .any(|field| field.is_empty())
        {
            return Err(format!("{context}: only variants may be empty"));
        }
        if !ids.insert(id) {
            return Err(format!("{context}: duplicate id '{id}'"));
        }
        if !keys.insert((clue_reading, clue_surface, reading)) {
            return Err(format!("{context}: duplicate clue and reading in '{id}'"));
        }
        *pairs.entry(pair.to_owned()).or_default() += 1;
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
        rows.push(ContextRow {
            id: id.to_owned(),
            pair: pair.to_owned(),
            oneshot_reading: oneshot_reading.to_owned(),
            oneshot_expected: oneshot_expected.to_owned(),
            clue_reading: clue_reading.to_owned(),
            clue_surface: clue_surface.to_owned(),
            reading: reading.to_owned(),
            expected: expected.to_owned(),
            variants,
        });
    }
    for (pair, count) in pairs {
        if count != 2 {
            return Err(format!(
                "{file}: pair '{pair}' has {count} rows; a pair is exactly two"
            ));
        }
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

    const CONTEXT: &str =
        "id\tpair\toneshot_reading\toneshot_expected\tclue_reading\tclue_surface\treading\texpected\tvariants\n";

    fn context(rows: &str) -> Result<Vec<ContextRow>, String> {
        parse_context(&format!("{CONTEXT}{rows}"))
    }

    #[test]
    fn context_rows_parse_a_pair_of_two() {
        let rows = context(
            "x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x2\tp1\tしょくじのはし\t食事の箸\tしょくじの\t食事の\tはし\t箸\t橋\n",
        )
        .expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pair, "p1");
        assert_eq!(rows[1].variants, ["橋"]);
    }

    #[test]
    fn context_rejects_pairs_that_are_not_exactly_two_rows() {
        let error =
            context("x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n").expect_err("one row");
        assert!(error.contains("exactly two"), "{error}");
        let error = context(
            "x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x2\tp1\tしょくじのはし\t食事の箸\tしょくじの\t食事の\tはし\t箸\t\n\
             x3\tp1\tちからのはし\t力の端\tちからの\t力の\tはし\t端\t\n",
        )
        .expect_err("three rows");
        assert!(error.contains("exactly two"), "{error}");
    }

    #[test]
    fn context_rejects_duplicate_ids_keys_and_empty_fields() {
        let error = context(
            "x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x1\tp1\tしょくじのはし\t食事の箸\tしょくじの\t食事の\tはし\t箸\t\n",
        )
        .expect_err("duplicate id");
        assert!(error.contains("duplicate id"), "{error}");
        let error = context(
            "x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\t\n\
             x2\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t箸\t\n",
        )
        .expect_err("duplicate key");
        assert!(error.contains("duplicate clue and reading"), "{error}");
        let error = context("x1\tp1\t\t川の橋\tかわの\t川の\tはし\t橋\t\n")
            .expect_err("empty oneshot reading");
        assert!(error.contains("only variants may be empty"), "{error}");
        let error = context("x1\tp1\tかわのはし\t川の橋\tかわの\t川の\tはし\t橋\tはし\n")
            .expect_err("variant equal to the reading");
        assert!(error.contains("equals the reading"), "{error}");
    }
}
