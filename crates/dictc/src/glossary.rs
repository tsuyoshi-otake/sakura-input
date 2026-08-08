//! Import of smile-chat's Japanese glossary into Sakura overlay entries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use sakura_core::dictionary::EntryFlags;
use sakura_proto::MAX_PREEDIT_BYTES;

use super::{validate_text, Error, SourceEntry};

const MAX_JSON_DEPTH: usize = 64;
/// Kana input should prefer a native Japanese surface when Mozc already has
/// one. Unmatched English glossary forms remain candidates, but their short
/// strings must not outrank established katakana or kanji spellings solely
/// because the shape default happens to be cheap.
const UNMATCHED_ASCII_COST: i32 = 2_000;
/// When the user's reading is itself the pronunciation of a kana surface, that
/// spelling is a stronger signal than an English or semantic alias attached to
/// the same glossary concept. Keep the adjustment bounded and inside the
/// overlay layer so user/profile/learning costs can still take precedence.
const PHONETIC_SURFACE_BONUS: i32 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossarySense {
    pub definition: String,
    pub reading: Option<String>,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryTerm {
    pub term: String,
    pub reading: Option<String>,
    pub aliases: Vec<String>,
    pub senses: Vec<GlossarySense>,
    source: Arc<str>,
    line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayDefaults {
    pub katakana_left_id: u16,
    pub katakana_right_id: u16,
    pub ascii_left_id: u16,
    pub ascii_right_id: u16,
    pub base_word_cost: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportReport {
    pub terms: usize,
    pub surfaces: usize,
    pub ascii_aliases: usize,
    pub ascii_only_terms: usize,
    pub matched_to_mozc: usize,
    pub defaulted: usize,
    pub duplicate_surfaces: usize,
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub entries: Vec<SourceEntry>,
    pub report: ImportReport,
}

/// Parses one smile-chat `ja_partN.json` array with bounded recursion.
pub fn parse_part(source: &str, text: &str) -> Result<Vec<GlossaryTerm>, Error> {
    Parser::new(source, text).parse_part()
}

/// Converts glossary terms to deduplicated IT overlay entries.
///
/// Exact `(reading, surface)` matches inherit Mozc's grammatical ids and
/// corpus-derived cost. The remainder use explicit shape-specific defaults;
/// the report makes every fallback and missing reading observable.
pub fn import(
    terms: &[GlossaryTerm],
    mozc_entries: &[SourceEntry],
    defaults: OverlayDefaults,
) -> Result<ImportResult, Error> {
    let mut importer = Importer::new(terms, defaults)?;
    importer.match_mozc(mozc_entries);
    Ok(importer.finish())
}

/// Incremental glossary importer for processing Mozc's dictionary shards.
///
/// Only the small set of pending glossary candidates is retained between
/// calls. A caller can parse, match, and discard each Mozc shard instead of
/// keeping the complete upstream dictionary in memory.
#[derive(Debug)]
pub struct Importer {
    pending: BTreeMap<String, BTreeMap<String, Pending>>,
    report: ImportReport,
    defaults: OverlayDefaults,
}

impl Importer {
    pub fn new(terms: &[GlossaryTerm], defaults: OverlayDefaults) -> Result<Self, Error> {
        let mut report = ImportReport {
            terms: terms.len(),
            ..ImportReport::default()
        };
        let mut pending = BTreeMap::<String, BTreeMap<String, Pending>>::new();

        for term in terms {
            let raw_reading = term
                .reading
                .as_deref()
                .or_else(|| {
                    term.senses
                        .iter()
                        .find_map(|sense| sense.reading.as_deref())
                })
                .or_else(|| is_kana_text(&term.term).then_some(term.term.as_str()));
            let annotation = annotation(term);
            let normalized_reading = raw_reading.and_then(normalize_reading);
            if let Some(reading) = normalized_reading.as_deref() {
                insert_pending(
                    &mut pending,
                    &mut report,
                    term,
                    CandidateInput {
                        reading,
                        surface: &term.term,
                        alias: false,
                        synthetic_phonetic: false,
                        annotation: &annotation,
                    },
                )?;
                for alias in &term.aliases {
                    insert_pending(
                        &mut pending,
                        &mut report,
                        term,
                        CandidateInput {
                            reading,
                            surface: alias,
                            alias: true,
                            synthetic_phonetic: false,
                            annotation: &annotation,
                        },
                    )?;
                }
                let has_ascii_spelling = is_ascii_spelling(&term.term)
                    || term.aliases.iter().any(|alias| is_ascii_spelling(alias));
                let has_phonetic_spelling = normalize_reading(&term.term).as_deref()
                    == Some(reading)
                    || term
                        .aliases
                        .iter()
                        .any(|alias| normalize_reading(alias).as_deref() == Some(reading));
                if has_ascii_spelling && !has_phonetic_spelling {
                    let surface = katakana_surface(reading);
                    insert_pending(
                        &mut pending,
                        &mut report,
                        term,
                        CandidateInput {
                            reading,
                            surface: &surface,
                            alias: true,
                            synthetic_phonetic: true,
                            annotation: &annotation,
                        },
                    )?;
                }
            }

            let ascii_aliases = insert_ascii_aliases(&mut pending, &mut report, term, &annotation)?;
            report.ascii_aliases = report.ascii_aliases.saturating_add(ascii_aliases);
            if normalized_reading.is_none() {
                if ascii_aliases == 0 {
                    match raw_reading {
                        Some(raw_reading) => report.gaps.push(format!(
                            "{}: unsupported reading '{}'",
                            term.term, raw_reading
                        )),
                        None => report.gaps.push(format!("{}: missing reading", term.term)),
                    }
                } else {
                    report.ascii_only_terms = report.ascii_only_terms.saturating_add(1);
                }
            }
        }

        Ok(Self {
            pending,
            report,
            defaults,
        })
    }

    /// Matches one parsed Mozc shard. Repeated calls are deterministic and
    /// retain the lowest-cost exact match across all shards.
    pub fn match_mozc(&mut self, mozc_entries: &[SourceEntry]) {
        for entry in mozc_entries {
            let Some(surfaces) = self.pending.get_mut(entry.reading.as_str()) else {
                continue;
            };
            let Some(candidate) = surfaces.get_mut(entry.surface.as_str()) else {
                continue;
            };
            if candidate
                .matched
                .is_none_or(|matched| entry.word_cost < matched.word_cost)
            {
                candidate.matched = Some(Matched {
                    left_id: entry.left_id,
                    right_id: entry.right_id,
                    word_cost: entry.word_cost,
                });
            }
        }
    }

    pub fn finish(mut self) -> ImportResult {
        let mut entries = Vec::new();
        for (reading, surfaces) in self.pending {
            for (surface, candidate) in surfaces {
                // A mechanical katakana rendering is only vocabulary evidence
                // when the pinned language corpus independently contains it.
                // Otherwise native Japanese readings such as `へんすう` would
                // acquire misleading `ヘンスウ` overlay entries.
                if candidate.synthetic_phonetic && candidate.matched.is_none() {
                    continue;
                }
                let (left_id, right_id, base_word_cost) = if let Some(matched) = candidate.matched {
                    self.report.matched_to_mozc += 1;
                    (
                        matched.left_id,
                        matched.right_id,
                        matched.word_cost.saturating_sub(400).max(0),
                    )
                } else {
                    self.report.defaulted += 1;
                    let (left_id, right_id) = if surface.is_ascii() {
                        (self.defaults.ascii_left_id, self.defaults.ascii_right_id)
                    } else {
                        (
                            self.defaults.katakana_left_id,
                            self.defaults.katakana_right_id,
                        )
                    };
                    let length_cost = i32::try_from(surface.chars().count().min(20))
                        .unwrap_or(20)
                        .saturating_mul(35);
                    let alias_cost = if candidate.alias { 120 } else { 0 };
                    let shape_cost = if surface.is_ascii() {
                        UNMATCHED_ASCII_COST
                    } else {
                        0
                    };
                    (
                        left_id,
                        right_id,
                        self.defaults
                            .base_word_cost
                            .saturating_add(length_cost)
                            .saturating_add(alias_cost)
                            .saturating_add(shape_cost),
                    )
                };
                let word_cost = if candidate.phonetic {
                    base_word_cost.saturating_sub(PHONETIC_SURFACE_BONUS).max(0)
                } else {
                    base_word_cost
                };
                entries.push(SourceEntry {
                    reading: reading.clone(),
                    surface,
                    left_id,
                    right_id,
                    word_cost,
                    prediction_cost: word_cost.saturating_add(300),
                    flags: EntryFlags::IT | EntryFlags::PREDICTION,
                    annotation: candidate.annotation,
                    source: candidate.source,
                    line: candidate.line,
                });
            }
        }
        self.report.surfaces = entries.len();
        ImportResult {
            entries,
            report: self.report,
        }
    }
}

/// Canonicalizes glossary pronunciations to the hiragana reading keyed by the
/// runtime trie. Parenthesized disambiguators are metadata, and spaces/middle
/// dots are pronunciation separators, so neither becomes an input keystroke.
pub fn normalize_reading(value: &str) -> Option<String> {
    let mut normalized = String::new();
    for c in value.chars() {
        let c = match c {
            '(' | '（' => break,
            ' ' | '\t' | '\r' | '\n' | '\u{3000}' | '・' | '･' | '/' | '／' => continue,
            'ァ'..='ヶ' => char::from_u32(u32::from(c) - 0x60)?,
            _ => c,
        };
        if !matches!(c, '\u{3040}'..='\u{309f}' | 'ー') {
            return None;
        }
        normalized.push(c);
    }
    (!normalized.is_empty()).then_some(normalized)
}

fn is_kana_text(value: &str) -> bool {
    normalize_reading(value).is_some()
        && value
            .chars()
            .all(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' | '･'))
}

fn is_ascii_spelling(value: &str) -> bool {
    value.is_ascii()
        && value
            .chars()
            .any(|character| character.is_ascii_alphabetic())
}

/// Produces deterministic keys that can be typed as one all-Shift ASCII run.
/// Spaces cannot be part of a conversion reading because Space starts
/// conversion, so phrase surfaces receive a first-word key as well as a
/// separator-free key. Punctuation is retained in the first key for terms
/// such as `C++` and `CI/CD`; an alphanumeric spelling is added when it is
/// useful and unambiguous.
fn ascii_reading_variants(surface: &str) -> Vec<String> {
    if !is_ascii_spelling(surface) {
        return Vec::new();
    }

    let compact_surface = surface
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    let compact_alphanumeric = compact_surface
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let first_word = surface
        .split(|character: char| character.is_ascii_whitespace() || matches!(character, '_' | '/'))
        .next()
        .unwrap_or_default()
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();

    let mut variants = Vec::with_capacity(3);
    for candidate in [compact_surface, compact_alphanumeric, first_word] {
        if candidate.chars().count() < 2
            || !candidate
                .chars()
                .any(|character| character.is_ascii_alphabetic())
            || variants.iter().any(|existing| existing == &candidate)
        {
            continue;
        }
        variants.push(candidate);
    }
    variants
}

fn katakana_surface(reading: &str) -> String {
    reading
        .chars()
        .map(|character| match character {
            '\u{3041}'..='\u{3096}' => {
                char::from_u32(u32::from(character) + 0x60).unwrap_or(character)
            }
            _ => character,
        })
        .collect()
}

fn annotation(term: &GlossaryTerm) -> String {
    let Some(sense) = term.senses.first() else {
        return String::new();
    };
    let mut value = String::new();
    if let Some(domain) = sense.domain.as_deref() {
        value.push('[');
        value.push_str(domain);
        value.push_str("] ");
    }
    value.push_str(&sense.definition);
    value = value.replace(['\t', '\r', '\n'], " ");
    if value.len() > MAX_PREEDIT_BYTES {
        let mut end = MAX_PREEDIT_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

fn insert_pending(
    pending: &mut BTreeMap<String, BTreeMap<String, Pending>>,
    report: &mut ImportReport,
    term: &GlossaryTerm,
    input: CandidateInput<'_>,
) -> Result<(), Error> {
    let CandidateInput {
        reading,
        surface,
        alias,
        synthetic_phonetic,
        annotation,
    } = input;
    validate_text(&term.source, term.line, "surface", surface)?;
    let surfaces = pending.entry(reading.to_string()).or_default();
    if surfaces.contains_key(surface) {
        report.duplicate_surfaces += 1;
        return Ok(());
    }
    surfaces.insert(
        surface.to_string(),
        Pending {
            annotation: annotation.to_string(),
            alias,
            phonetic: normalize_reading(surface).as_deref() == Some(reading),
            synthetic_phonetic,
            source: Arc::clone(&term.source),
            line: term.line,
            matched: None,
        },
    );
    Ok(())
}

fn insert_ascii_aliases(
    pending: &mut BTreeMap<String, BTreeMap<String, Pending>>,
    report: &mut ImportReport,
    term: &GlossaryTerm,
    annotation: &str,
) -> Result<usize, Error> {
    let mut seen = BTreeSet::<(String, String)>::new();
    let mut inserted = 0usize;
    for (surface, is_alias) in std::iter::once((term.term.as_str(), false))
        .chain(term.aliases.iter().map(|alias| (alias.as_str(), true)))
    {
        for (variant_index, reading) in ascii_reading_variants(surface).into_iter().enumerate() {
            if !seen.insert((reading.clone(), surface.to_string())) {
                continue;
            }
            insert_pending(
                pending,
                report,
                term,
                CandidateInput {
                    reading: &reading,
                    surface,
                    alias: is_alias || variant_index != 0,
                    synthetic_phonetic: false,
                    annotation,
                },
            )?;
            inserted = inserted.saturating_add(1);
        }
    }
    Ok(inserted)
}

struct CandidateInput<'a> {
    reading: &'a str,
    surface: &'a str,
    alias: bool,
    synthetic_phonetic: bool,
    annotation: &'a str,
}

#[derive(Debug)]
struct Pending {
    annotation: String,
    alias: bool,
    phonetic: bool,
    synthetic_phonetic: bool,
    source: Arc<str>,
    line: usize,
    matched: Option<Matched>,
}

#[derive(Debug, Clone, Copy)]
struct Matched {
    left_id: u16,
    right_id: u16,
    word_cost: i32,
}

struct Parser<'a> {
    source: &'a str,
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, text: &'a str) -> Self {
        Self {
            source,
            text,
            at: 0,
        }
    }

    fn parse_part(mut self) -> Result<Vec<GlossaryTerm>, Error> {
        self.skip_ws();
        self.expect(b'[')?;
        let mut terms = Vec::new();
        self.skip_ws();
        if self.take(b']') {
            self.finish()?;
            return Ok(terms);
        }
        loop {
            terms.push(self.parse_term()?);
            self.skip_ws();
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
        }
        self.finish()?;
        Ok(terms)
    }

    fn parse_term(&mut self) -> Result<GlossaryTerm, Error> {
        self.skip_ws();
        let line = self.line();
        self.expect(b'{')?;
        let mut term = None;
        let mut reading = None;
        let mut aliases = Vec::new();
        let mut senses = None;
        self.object_fields(|parser, key| {
            match key.as_str() {
                "term" => term = Some(parser.string_value()?),
                "reading" => reading = parser.optional_string()?,
                "aliases" => aliases = parser.string_array()?,
                "senses" => senses = Some(parser.sense_array()?),
                _ => parser.skip_value(0)?,
            }
            Ok(())
        })?;
        let term = term.ok_or_else(|| self.error("glossary entry is missing term"))?;
        if term.is_empty() {
            return Err(self.error("glossary term must not be empty"));
        }
        let senses = senses.ok_or_else(|| self.error("glossary entry is missing senses"))?;
        if senses.is_empty() {
            return Err(self.error("glossary senses must not be empty"));
        }
        Ok(GlossaryTerm {
            term,
            reading,
            aliases,
            senses,
            source: Arc::from(self.source),
            line,
        })
    }

    fn sense_array(&mut self) -> Result<Vec<GlossarySense>, Error> {
        self.skip_ws();
        self.expect(b'[')?;
        let mut senses = Vec::new();
        self.skip_ws();
        if self.take(b']') {
            return Ok(senses);
        }
        loop {
            senses.push(self.parse_sense()?);
            self.skip_ws();
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(senses)
    }

    fn parse_sense(&mut self) -> Result<GlossarySense, Error> {
        self.skip_ws();
        self.expect(b'{')?;
        let mut definition = None;
        let mut reading = None;
        let mut domain = None;
        self.object_fields(|parser, key| {
            match key.as_str() {
                "definition" => definition = Some(parser.string_value()?),
                "reading" => reading = parser.optional_string()?,
                "domain" => domain = parser.optional_string()?,
                _ => parser.skip_value(0)?,
            }
            Ok(())
        })?;
        let definition = definition.ok_or_else(|| self.error("sense is missing definition"))?;
        if definition.is_empty() {
            return Err(self.error("sense definition must not be empty"));
        }
        Ok(GlossarySense {
            definition,
            reading,
            domain,
        })
    }

    fn object_fields(
        &mut self,
        mut field: impl FnMut(&mut Self, String) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.skip_ws();
        if self.take(b'}') {
            return Ok(());
        }
        loop {
            let key = self.string_value()?;
            self.skip_ws();
            self.expect(b':')?;
            field(self, key)?;
            self.skip_ws();
            if self.take(b'}') {
                return Ok(());
            }
            self.expect(b',')?;
        }
    }

    fn string_array(&mut self) -> Result<Vec<String>, Error> {
        self.skip_ws();
        self.expect(b'[')?;
        let mut values = Vec::new();
        self.skip_ws();
        if self.take(b']') {
            return Ok(values);
        }
        loop {
            values.push(self.string_value()?);
            self.skip_ws();
            if self.take(b']') {
                break;
            }
            self.expect(b',')?;
        }
        Ok(values)
    }

    fn optional_string(&mut self) -> Result<Option<String>, Error> {
        self.skip_ws();
        if self.text[self.at..].starts_with("null") {
            self.at += 4;
            Ok(None)
        } else {
            self.string_value().map(Some)
        }
    }

    fn string_value(&mut self) -> Result<String, Error> {
        self.skip_ws();
        self.expect(b'\"')?;
        let mut value = String::new();
        loop {
            let byte = *self
                .text
                .as_bytes()
                .get(self.at)
                .ok_or_else(|| self.error("unterminated JSON string"))?;
            if byte == b'\"' {
                self.at += 1;
                return Ok(value);
            }
            if byte == b'\\' {
                self.at += 1;
                let escape = self
                    .next_byte()
                    .ok_or_else(|| self.error("unterminated JSON escape"))?;
                match escape {
                    b'\"' => value.push('\"'),
                    b'\\' => value.push('\\'),
                    b'/' => value.push('/'),
                    b'b' => value.push('\u{0008}'),
                    b'f' => value.push('\u{000c}'),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    b'u' => {
                        let first = self.hex4()?;
                        let scalar = if (0xd800..=0xdbff).contains(&first) {
                            if self.next_byte() != Some(b'\\') || self.next_byte() != Some(b'u') {
                                return Err(self.error("high surrogate without low surrogate"));
                            }
                            let second = self.hex4()?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return Err(self.error("invalid low surrogate"));
                            }
                            0x1_0000
                                + ((u32::from(first) - 0xd800) << 10)
                                + (u32::from(second) - 0xdc00)
                        } else if (0xdc00..=0xdfff).contains(&first) {
                            return Err(self.error("unexpected low surrogate"));
                        } else {
                            u32::from(first)
                        };
                        value.push(
                            char::from_u32(scalar)
                                .ok_or_else(|| self.error("invalid Unicode scalar"))?,
                        );
                    }
                    _ => return Err(self.error("unknown JSON escape")),
                }
                continue;
            }
            if byte < 0x20 {
                return Err(self.error("unescaped control character in JSON string"));
            }
            let c = self.text[self.at..]
                .chars()
                .next()
                .ok_or_else(|| self.error("unterminated JSON string"))?;
            value.push(c);
            self.at += c.len_utf8();
        }
    }

    fn hex4(&mut self) -> Result<u16, Error> {
        let mut value = 0u16;
        for _ in 0..4 {
            let digit = self
                .next_byte()
                .and_then(|byte| char::from(byte).to_digit(16))
                .ok_or_else(|| self.error("invalid JSON Unicode escape"))?;
            value = value * 16 + digit as u16;
        }
        Ok(value)
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), Error> {
        if depth >= MAX_JSON_DEPTH {
            return Err(self.error("JSON nesting exceeds 64 levels"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'\"') => {
                self.string_value()?;
            }
            Some(b'{') => {
                self.at += 1;
                self.skip_ws();
                if self.take(b'}') {
                    return Ok(());
                }
                loop {
                    self.string_value()?;
                    self.skip_ws();
                    self.expect(b':')?;
                    self.skip_value(depth + 1)?;
                    self.skip_ws();
                    if self.take(b'}') {
                        break;
                    }
                    self.expect(b',')?;
                }
            }
            Some(b'[') => {
                self.at += 1;
                self.skip_ws();
                if self.take(b']') {
                    return Ok(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    self.skip_ws();
                    if self.take(b']') {
                        break;
                    }
                    self.expect(b',')?;
                }
            }
            Some(b't') => self.literal("true")?,
            Some(b'f') => self.literal("false")?,
            Some(b'n') => self.literal("null")?,
            Some(b'-' | b'0'..=b'9') => self.number()?,
            _ => return Err(self.error("invalid JSON value")),
        }
        Ok(())
    }

    fn literal(&mut self, literal: &str) -> Result<(), Error> {
        if !self.text[self.at..].starts_with(literal) {
            return Err(self.error("invalid JSON literal"));
        }
        self.at += literal.len();
        Ok(())
    }

    fn number(&mut self) -> Result<(), Error> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.error("invalid JSON number"));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Error> {
        self.skip_ws();
        if self.at != self.text.len() {
            return Err(self.error("trailing data after JSON array"));
        }
        Ok(())
    }

    fn skip_ws(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.at += 1;
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), Error> {
        self.skip_ws();
        if self.take(wanted) {
            Ok(())
        } else {
            Err(self.error(format!("expected JSON byte 0x{wanted:02x}")))
        }
    }

    fn take(&mut self, wanted: u8) -> bool {
        if self.peek() == Some(wanted) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn next_byte(&mut self) -> Option<u8> {
        let value = self.peek()?;
        self.at += 1;
        Some(value)
    }

    fn line(&self) -> usize {
        self.text.as_bytes()[..self.at]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1
    }

    fn error(&self, message: impl Into<String>) -> Error {
        Error::at(self.source, self.line(), message)
    }
}
