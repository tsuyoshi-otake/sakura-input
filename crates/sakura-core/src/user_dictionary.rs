//! Human-editable user dictionary and its compact in-memory reading trie.

use core::fmt;
#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;

use sakura_proto::MAX_PREEDIT_BYTES;

use crate::dictionary::EntryFlags;

pub const MAX_USER_DICTIONARY_ENTRIES: usize = 10_000;
pub const USER_DICTIONARY_FORMAT_VERSION: u16 = 1;

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PredictiveSearchComparisons {
    lower_bound: usize,
    prefix: usize,
}

#[cfg(test)]
impl PredictiveSearchComparisons {
    const EMPTY: Self = Self {
        lower_bound: 0,
        prefix: 0,
    };

    const fn total(self) -> usize {
        self.lower_bound + self.prefix
    }
}

#[cfg(test)]
std::thread_local! {
    static PREDICTIVE_SEARCH_COMPARISONS: Cell<PredictiveSearchComparisons> = const {
        Cell::new(PredictiveSearchComparisons::EMPTY)
    };
}

#[cfg(test)]
fn record_lower_bound_comparison() {
    PREDICTIVE_SEARCH_COMPARISONS.with(|comparisons| {
        let mut value = comparisons.get();
        value.lower_bound += 1;
        comparisons.set(value);
    });
}

#[cfg(test)]
fn record_prefix_comparison() {
    PREDICTIVE_SEARCH_COMPARISONS.with(|comparisons| {
        let mut value = comparisons.get();
        value.prefix += 1;
        comparisons.set(value);
    });
}

#[cfg(test)]
fn reset_predictive_search_comparisons() {
    PREDICTIVE_SEARCH_COMPARISONS.with(|comparisons| {
        comparisons.set(PredictiveSearchComparisons::EMPTY);
    });
}

#[cfg(test)]
fn take_predictive_search_comparisons() -> PredictiveSearchComparisons {
    PREDICTIVE_SEARCH_COMPARISONS
        .with(|comparisons| comparisons.replace(PredictiveSearchComparisons::EMPTY))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPartOfSpeech {
    Noun,
    ProperNoun,
    PersonalName,
    FamilyName,
    FirstName,
    Organization,
    Place,
    SaNoun,
    AdjectivalNoun,
    Number,
    Alphabet,
    Symbol,
    Adverb,
    PrenounAdjectival,
    Conjunction,
    Interjection,
    Prefix,
    CounterSuffix,
    GenericSuffix,
    PersonNameSuffix,
    PlaceNameSuffix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserPosSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub left_id: u16,
    pub right_id: u16,
    pub word_cost: i32,
}

impl UserPartOfSpeech {
    pub const ALL: [Self; 21] = [
        Self::Noun,
        Self::ProperNoun,
        Self::PersonalName,
        Self::FamilyName,
        Self::FirstName,
        Self::Organization,
        Self::Place,
        Self::SaNoun,
        Self::AdjectivalNoun,
        Self::Number,
        Self::Alphabet,
        Self::Symbol,
        Self::Adverb,
        Self::PrenounAdjectival,
        Self::Conjunction,
        Self::Interjection,
        Self::Prefix,
        Self::CounterSuffix,
        Self::GenericSuffix,
        Self::PersonNameSuffix,
        Self::PlaceNameSuffix,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|part_of_speech| part_of_speech.spec().name == name)
    }

    /// Mapping pinned to Mozc dictionary revision
    /// `3f235b4eb6fcff7d14ef5f0fb8ee56de7ee4c732`. These are exact generic
    /// class rows from `src/data/dictionary_oss/id.def`, paired with the
    /// default costs in `src/data/rules/user_pos.def`.
    pub const fn spec(self) -> UserPosSpec {
        match self {
            Self::Noun => spec("noun", "名詞", 1851, 2500),
            Self::ProperNoun => spec("proper-noun", "固有名詞", 1920, 1500),
            Self::PersonalName => spec("personal-name", "人名", 1921, 1500),
            Self::FamilyName => spec("family-name", "姓", 1923, 2500),
            Self::FirstName => spec("first-name", "名", 1922, 2500),
            Self::Organization => spec("organization", "組織", 1929, 2000),
            Self::Place => spec("place", "地名", 1924, 2000),
            Self::SaNoun => spec("sa-noun", "名詞サ変", 1841, 2500),
            Self::AdjectivalNoun => spec("adjectival-noun", "名詞形動", 1931, 1500),
            Self::Number => spec("number", "数", 2044, 1000),
            Self::Alphabet => spec("alphabet", "アルファベット", 2643, 2500),
            Self::Symbol => spec("symbol", "記号", 2644, 500),
            Self::Adverb => spec("adverb", "副詞", 12, 1500),
            Self::PrenounAdjectival => spec("prenoun-adjectival", "連体詞", 2659, 1000),
            Self::Conjunction => spec("conjunction", "接続詞", 2593, 1000),
            Self::Interjection => spec("interjection", "感動詞", 2591, 1000),
            Self::Prefix => spec("prefix", "接頭語", 2600, 1500),
            Self::CounterSuffix => spec("counter-suffix", "助数詞", 2011, 1000),
            Self::GenericSuffix => spec("generic-suffix", "接尾一般", 1949, 1500),
            Self::PersonNameSuffix => spec("person-name-suffix", "接尾人名", 1999, 1000),
            Self::PlaceNameSuffix => spec("place-name-suffix", "接尾地名", 2019, 1000),
        }
    }
}

const fn spec(name: &'static str, label: &'static str, id: u16, cost: i32) -> UserPosSpec {
    UserPosSpec {
        name,
        label,
        left_id: id,
        right_id: id,
        word_cost: cost,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDictionaryEntry {
    pub reading: String,
    pub surface: String,
    pub part_of_speech: UserPartOfSpeech,
    pub comment: String,
}

impl UserDictionaryEntry {
    pub fn left_id(&self) -> u16 {
        self.part_of_speech.spec().left_id
    }

    pub fn right_id(&self) -> u16 {
        self.part_of_speech.spec().right_id
    }

    pub fn word_cost(&self) -> i32 {
        self.part_of_speech.spec().word_cost
    }

    pub fn flags(&self) -> EntryFlags {
        EntryFlags::NONE
    }
}

#[derive(Debug, Clone, Default)]
struct TrieNode {
    children: Vec<(char, usize)>,
    entries: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct UserDictionary {
    entries: Vec<UserDictionaryEntry>,
    trie: Vec<TrieNode>,
}

impl UserDictionary {
    pub fn parse_tsv(source: &str) -> Result<Self, UserDictionaryError> {
        let entries = parse_entries(source)?;
        Self::from_entries(entries)
    }

    pub fn from_entries(
        mut entries: Vec<UserDictionaryEntry>,
    ) -> Result<Self, UserDictionaryError> {
        if entries.len() > MAX_USER_DICTIONARY_ENTRIES {
            return Err(UserDictionaryError::new(
                0,
                UserDictionaryErrorKind::TooManyEntries,
            ));
        }
        for (index, entry) in entries.iter().enumerate() {
            validate_entry(entry).map_err(|kind| UserDictionaryError::new(index + 1, kind))?;
        }
        entries.sort_by(|left, right| {
            (&left.reading, left.word_cost(), &left.surface).cmp(&(
                &right.reading,
                right.word_cost(),
                &right.surface,
            ))
        });
        if entries
            .windows(2)
            .any(|pair| pair[0].reading == pair[1].reading && pair[0].surface == pair[1].surface)
        {
            return Err(UserDictionaryError::new(
                0,
                UserDictionaryErrorKind::DuplicateEntry,
            ));
        }

        let mut building = vec![BTreeMap::<char, usize>::new()];
        let mut terminals = vec![Vec::<usize>::new()];
        for (entry_index, entry) in entries.iter().enumerate() {
            let mut node = 0usize;
            for character in entry.reading.chars() {
                let next = if let Some(next) = building[node].get(&character).copied() {
                    next
                } else {
                    let next = building.len();
                    building.push(BTreeMap::new());
                    terminals.push(Vec::new());
                    building[node].insert(character, next);
                    next
                };
                node = next;
            }
            terminals[node].push(entry_index);
        }
        let trie = building
            .into_iter()
            .zip(terminals)
            .map(|(children, entries)| TrieNode {
                children: children.into_iter().collect(),
                entries,
            })
            .collect();
        Ok(Self { entries, trie })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn entry(&self, index: usize) -> Option<&UserDictionaryEntry> {
        self.entries.get(index)
    }

    pub fn entries(&self) -> &[UserDictionaryEntry] {
        &self.entries
    }

    /// Serializes the validated dictionary into Sakura's canonical, stable
    /// TSV representation. `UserDictionary` construction validates that no
    /// field can split a row, so this operation is infallible and round-trips
    /// through [`UserDictionary::parse_tsv`].
    pub fn to_tsv(&self) -> String {
        let mut output = format!(
            "# format-version: {USER_DICTIONARY_FORMAT_VERSION}\nreading\tsurface\tpos\tcomment\n"
        );
        for entry in &self.entries {
            output.push_str(&entry.reading);
            output.push('\t');
            output.push_str(&entry.surface);
            output.push('\t');
            output.push_str(entry.part_of_speech.spec().name);
            output.push('\t');
            output.push_str(&entry.comment);
            output.push('\n');
        }
        output
    }

    /// Visits entries whose reading is a prefix of `query`, shortest prefix
    /// first and source order within one reading. Returning `false` stops.
    pub fn common_prefix_search(&self, query: &str, mut visit: impl FnMut(usize, usize) -> bool) {
        if self.trie.is_empty() {
            return;
        }
        let mut node = 0usize;
        for (offset, character) in query.char_indices() {
            let Ok(position) = self.trie[node]
                .children
                .binary_search_by_key(&character, |(label, _)| *label)
            else {
                break;
            };
            node = self.trie[node].children[position].1;
            let matched_bytes = offset + character.len_utf8();
            for entry in &self.trie[node].entries {
                if !visit(matched_bytes, *entry) {
                    return;
                }
            }
        }
    }

    /// Visits entries whose complete reading starts with `prefix`, in the
    /// dictionary's deterministic reading/cost/surface order. The entries are
    /// sorted by reading, so a lower-bound search finds the start of the one
    /// contiguous matching range in O(log N), then visits its K entries.
    pub fn predictive_search(&self, prefix: &str, mut visit: impl FnMut(usize) -> bool) {
        if prefix.is_empty() {
            return;
        }
        let prefix = prefix.as_bytes();
        let mut low = 0usize;
        let mut high = self.entries.len();
        while low < high {
            let middle = low + (high - low) / 2;
            #[cfg(test)]
            record_lower_bound_comparison();
            if self.entries[middle].reading.as_bytes() < prefix {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        for (offset, entry) in self.entries[low..].iter().enumerate() {
            #[cfg(test)]
            record_prefix_comparison();
            if !entry.reading.as_bytes().starts_with(prefix) {
                break;
            }
            if !visit(low + offset) {
                return;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDictionaryErrorKind {
    MissingHeader,
    WrongColumnCount,
    EmptyReading,
    InvalidReading,
    EmptySurface,
    FieldTooLong,
    InvalidFieldCharacter,
    UnknownPartOfSpeech,
    DuplicateEntry,
    TooManyEntries,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDictionaryError {
    pub line: usize,
    pub kind: UserDictionaryErrorKind,
}

impl UserDictionaryError {
    const fn new(line: usize, kind: UserDictionaryErrorKind) -> Self {
        Self { line, kind }
    }
}

impl fmt::Display for UserDictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "line {}: ", self.line)?;
        }
        match self.kind {
            UserDictionaryErrorKind::MissingHeader => {
                f.write_str("expected reading, surface, pos, comment TSV header")
            }
            UserDictionaryErrorKind::WrongColumnCount => {
                f.write_str("expected exactly four tab-separated columns")
            }
            UserDictionaryErrorKind::EmptyReading => f.write_str("reading is empty"),
            UserDictionaryErrorKind::InvalidReading => {
                f.write_str(
                    "reading must contain hiragana or the long-vowel mark; alphabet entries may use a lowercase ASCII letter followed by lowercase letters or digits",
                )
            }
            UserDictionaryErrorKind::EmptySurface => f.write_str("surface is empty"),
            UserDictionaryErrorKind::FieldTooLong => f.write_str("field exceeds the IME bound"),
            UserDictionaryErrorKind::InvalidFieldCharacter => {
                f.write_str("fields cannot contain tabs or line breaks")
            }
            UserDictionaryErrorKind::UnknownPartOfSpeech => {
                f.write_str("part of speech is not in the curated picklist")
            }
            UserDictionaryErrorKind::DuplicateEntry => {
                f.write_str("reading and surface are registered twice")
            }
            UserDictionaryErrorKind::TooManyEntries => f.write_str("user dictionary exceeds cap"),
        }
    }
}

impl std::error::Error for UserDictionaryError {}

fn parse_entries(source: &str) -> Result<Vec<UserDictionaryEntry>, UserDictionaryError> {
    let mut entries = Vec::new();
    let mut saw_header = false;
    for (line_index, raw) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !saw_header {
            if line != "reading\tsurface\tpos\tcomment" {
                return Err(UserDictionaryError::new(
                    line_number,
                    UserDictionaryErrorKind::MissingHeader,
                ));
            }
            saw_header = true;
            continue;
        }
        if entries.len() >= MAX_USER_DICTIONARY_ENTRIES {
            return Err(UserDictionaryError::new(
                line_number,
                UserDictionaryErrorKind::TooManyEntries,
            ));
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 4 {
            return Err(UserDictionaryError::new(
                line_number,
                UserDictionaryErrorKind::WrongColumnCount,
            ));
        }
        let Some(part_of_speech) = UserPartOfSpeech::from_name(columns[2]) else {
            return Err(UserDictionaryError::new(
                line_number,
                UserDictionaryErrorKind::UnknownPartOfSpeech,
            ));
        };
        let entry = UserDictionaryEntry {
            reading: columns[0].to_owned(),
            surface: columns[1].to_owned(),
            part_of_speech,
            comment: columns[3].to_owned(),
        };
        validate_entry(&entry).map_err(|kind| UserDictionaryError::new(line_number, kind))?;
        entries.push(entry);
    }
    if !saw_header {
        return Err(UserDictionaryError::new(
            0,
            UserDictionaryErrorKind::MissingHeader,
        ));
    }
    Ok(entries)
}

fn validate_entry(entry: &UserDictionaryEntry) -> Result<(), UserDictionaryErrorKind> {
    if entry.reading.is_empty() {
        return Err(UserDictionaryErrorKind::EmptyReading);
    }
    let kana_reading = entry
        .reading
        .chars()
        .all(|character| ('\u{3041}'..='\u{309f}').contains(&character) || character == 'ー');
    let mut shift_latin_bytes = entry.reading.bytes();
    let shift_latin_reading = entry.part_of_speech == UserPartOfSpeech::Alphabet
        && shift_latin_bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && shift_latin_bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !kana_reading && !shift_latin_reading {
        return Err(UserDictionaryErrorKind::InvalidReading);
    }
    if entry.surface.is_empty() {
        return Err(UserDictionaryErrorKind::EmptySurface);
    }
    if entry.reading.len() > MAX_PREEDIT_BYTES
        || entry.surface.len() > MAX_PREEDIT_BYTES
        || entry.comment.len() > MAX_PREEDIT_BYTES
    {
        return Err(UserDictionaryErrorKind::FieldTooLong);
    }
    if [&entry.reading, &entry.surface, &entry.comment]
        .into_iter()
        .any(|field| field.contains(['\t', '\r', '\n']))
    {
        return Err(UserDictionaryErrorKind::InvalidFieldCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_entry(reading: impl Into<String>, surface: impl Into<String>) -> UserDictionaryEntry {
        UserDictionaryEntry {
            reading: reading.into(),
            surface: surface.into(),
            part_of_speech: UserPartOfSpeech::Noun,
            comment: String::new(),
        }
    }

    fn capacity_entries(prefix: &str) -> Vec<UserDictionaryEntry> {
        const HIRAGANA_DIGITS: [char; 10] =
            ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];

        (0..MAX_USER_DICTIONARY_ENTRIES)
            .map(|number| {
                let mut reading = prefix.to_owned();
                let mut value = number;
                for _ in 0..4 {
                    reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                    value /= HIRAGANA_DIGITS.len();
                }
                user_entry(reading, format!("user-{number:04}"))
            })
            .collect()
    }

    fn measured_prediction_indices(
        dictionary: &UserDictionary,
        prefix: &str,
        stop_after: Option<usize>,
    ) -> (Vec<usize>, PredictiveSearchComparisons) {
        reset_predictive_search_comparisons();
        let mut indices = Vec::new();
        dictionary.predictive_search(prefix, |index| {
            indices.push(index);
            stop_after.is_none_or(|limit| indices.len() < limit)
        });
        (indices, take_predictive_search_comparisons())
    }

    fn ceil_log2(value: usize) -> usize {
        assert!(value > 0);
        usize::BITS as usize - (value - 1).leading_zeros() as usize
    }

    #[test]
    fn parses_picklist_entries_and_searches_reading_prefixes() {
        let dictionary = UserDictionary::parse_tsv(
            "# format-version: 1\nreading\tsurface\tpos\tcomment\nさくら\tSakura\tproper-noun\tproduct\nさくらにゅうりょく\tSakura Input\torganization\tIME\n",
        )
        .expect("user dictionary");
        let mut matches = Vec::new();
        dictionary.common_prefix_search("さくらにゅうりょくを", |bytes, entry| {
            matches.push((
                bytes,
                dictionary.entry(entry).expect("entry").surface.clone(),
            ));
            true
        });
        assert_eq!(
            matches,
            vec![(9, "Sakura".to_owned()), (27, "Sakura Input".to_owned())]
        );

        let mut predictions = Vec::new();
        dictionary.predictive_search("さく", |entry| {
            predictions.push(dictionary.entry(entry).expect("entry").surface.clone());
            true
        });
        assert_eq!(predictions, ["Sakura", "Sakura Input"]);
    }

    #[test]
    fn predictive_search_preserves_bounds_order_and_early_stop_semantics() {
        let dictionary = UserDictionary::from_entries(vec![
            user_entry("かき", "follow-up"),
            user_entry("か", "exact-b"),
            user_entry("かー", "long-vowel"),
            user_entry("か", "exact-a"),
            user_entry("さ", "later"),
        ])
        .expect("valid dictionary");

        let (indices, _) = measured_prediction_indices(&dictionary, "か", None);
        let surfaces = indices
            .iter()
            .map(|&index| dictionary.entry(index).expect("entry").surface.as_str())
            .collect::<Vec<_>>();
        assert_eq!(surfaces, ["exact-a", "exact-b", "follow-up", "long-vowel"]);

        let (exact, _) = measured_prediction_indices(&dictionary, "かき", None);
        assert_eq!(
            exact
                .iter()
                .map(|&index| dictionary.entry(index).expect("entry").surface.as_str())
                .collect::<Vec<_>>(),
            ["follow-up"]
        );
        let (long_vowel, _) = measured_prediction_indices(&dictionary, "かー", None);
        assert_eq!(
            long_vowel
                .iter()
                .map(|&index| dictionary.entry(index).expect("entry").surface.as_str())
                .collect::<Vec<_>>(),
            ["long-vowel"]
        );

        let (early, early_comparisons) = measured_prediction_indices(&dictionary, "か", Some(1));
        assert_eq!(early, indices[..1]);
        assert_eq!(early_comparisons.prefix, 1);

        for prefix in ["", "ぁ", "き", "そ"] {
            let (matches, comparisons) = measured_prediction_indices(&dictionary, prefix, None);
            assert!(matches.is_empty(), "unexpected match for {prefix:?}");
            if prefix.is_empty() {
                assert_eq!(comparisons, PredictiveSearchComparisons::EMPTY);
            }
        }
    }

    #[test]
    fn predictive_search_at_capacity_is_logarithmic_plus_matching_entries() {
        let dictionary =
            UserDictionary::from_entries(capacity_entries("")).expect("capacity dictionary");
        assert_eq!(dictionary.len(), MAX_USER_DICTIONARY_ENTRIES);
        let first = dictionary
            .entries()
            .first()
            .expect("first entry")
            .reading
            .clone();
        let last = dictionary
            .entries()
            .last()
            .expect("last entry")
            .reading
            .clone();
        let logarithmic_bound = ceil_log2(dictionary.len());

        for (prefix, expected_matches) in [
            (first.as_str(), 1usize),
            (last.as_str(), 1usize),
            ("おさ", 0usize),
            ("そ", 0usize),
        ] {
            let (matches, comparisons) = measured_prediction_indices(&dictionary, prefix, None);
            assert_eq!(matches.len(), expected_matches, "prefix {prefix:?}");
            assert!(
                comparisons.lower_bound > 0,
                "the capacity search must perform lower-bound comparisons for {prefix:?}"
            );
            assert!(
                comparisons.lower_bound <= logarithmic_bound,
                "lower-bound comparisons for {prefix:?}: {comparisons:?}"
            );
            assert!(
                comparisons.total() <= logarithmic_bound + expected_matches + 2,
                "search comparisons for {prefix:?}: {comparisons:?}"
            );
        }

        let matching = UserDictionary::from_entries(capacity_entries("さ"))
            .expect("shared-prefix capacity dictionary");
        let (matches, comparisons) = measured_prediction_indices(&matching, "さ", None);
        assert_eq!(matches.len(), MAX_USER_DICTIONARY_ENTRIES);
        assert!(comparisons.lower_bound <= ceil_log2(matching.len()));
        assert_eq!(comparisons.prefix, MAX_USER_DICTIONARY_ENTRIES);
        assert!(comparisons.total() <= ceil_log2(matching.len()) + MAX_USER_DICTIONARY_ENTRIES);
    }

    #[test]
    fn every_picklist_value_has_a_non_generic_pinned_class() {
        assert_eq!(UserPartOfSpeech::ALL.len(), 21);
        for part_of_speech in UserPartOfSpeech::ALL {
            let spec = part_of_speech.spec();
            assert_ne!(spec.left_id, 0, "{}", spec.name);
            assert_eq!(spec.left_id, spec.right_id, "{}", spec.name);
            assert!(spec.word_cost >= 0, "{}", spec.name);
            assert_eq!(UserPartOfSpeech::from_name(spec.name), Some(part_of_speech));
        }
    }

    #[test]
    fn alphabet_entries_accept_only_canonical_lowercase_alphanumeric_readings() {
        let dictionary = UserDictionary::parse_tsv(concat!(
            "reading\tsurface\tpos\tcomment\n",
            "custom\tCustom\talphabet\tShift-Latin exact entry\n",
            "ipv6\tIPv6\talphabet\topaque identifier exact entry\n",
        ))
        .expect("canonical Shift-Latin and alphanumeric readings");
        assert_eq!(dictionary.entries()[0].reading, "custom");
        assert_eq!(dictionary.entries()[1].reading, "ipv6");

        for source in [
            "reading\tsurface\tpos\tcomment\ncustom\tCustom\tnoun\twrong pos\n",
            "reading\tsurface\tpos\tcomment\nCustom\tCustom\talphabet\twrong case\n",
            "reading\tsurface\tpos\tcomment\n1custom\tCustom1\talphabet\tmust start with a letter\n",
            "reading\tsurface\tpos\tcomment\ncustom-1\tCustom1\talphabet\tpunctuation is not canonical\n",
            "reading\tsurface\tpos\tcomment\nかcustom\tCustom\talphabet\tmixed script\n",
        ] {
            let error = UserDictionary::parse_tsv(source).expect_err("must reject");
            assert_eq!(error.line, 2);
            assert_eq!(error.kind, UserDictionaryErrorKind::InvalidReading);
        }
    }

    #[test]
    fn hostile_rows_fail_with_the_exact_line_and_never_partially_load() {
        let malformed = [
            (
                "reading\tsurface\tpos\tcomment\n\t表層\tnoun\tx\n",
                UserDictionaryErrorKind::EmptyReading,
            ),
            (
                "reading\tsurface\tpos\tcomment\nカナ\t表層\tnoun\tx\n",
                UserDictionaryErrorKind::InvalidReading,
            ),
            (
                "reading\tsurface\tpos\tcomment\nかな\t\tnoun\tx\n",
                UserDictionaryErrorKind::EmptySurface,
            ),
            (
                "reading\tsurface\tpos\tcomment\nかな\t表層\tunknown\tx\n",
                UserDictionaryErrorKind::UnknownPartOfSpeech,
            ),
            (
                "reading\tsurface\tpos\tcomment\nかな\t表層\tnoun\n",
                UserDictionaryErrorKind::WrongColumnCount,
            ),
        ];
        for (source, expected) in malformed {
            let error = UserDictionary::parse_tsv(source).expect_err("must reject");
            assert_eq!(error.line, 2);
            assert_eq!(error.kind, expected);
        }
    }

    #[test]
    fn duplicate_reading_surface_pair_is_rejected() {
        let error = UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\nかな\t仮名\tnoun\ta\nかな\t仮名\tproper-noun\tb\n",
        )
        .expect_err("duplicate");
        assert_eq!(error.kind, UserDictionaryErrorKind::DuplicateEntry);
    }

    #[test]
    fn canonical_tsv_roundtrips_without_reordering_or_field_loss() {
        let original = UserDictionary::from_entries(vec![
            UserDictionaryEntry {
                reading: "さくら".to_owned(),
                surface: "Sakura".to_owned(),
                part_of_speech: UserPartOfSpeech::ProperNoun,
                comment: "product".to_owned(),
            },
            UserDictionaryEntry {
                reading: "かいはつ".to_owned(),
                surface: "開発".to_owned(),
                part_of_speech: UserPartOfSpeech::SaNoun,
                comment: String::new(),
            },
        ])
        .expect("valid dictionary");

        let serialized = original.to_tsv();
        let reparsed = UserDictionary::parse_tsv(&serialized).expect("canonical TSV");

        assert_eq!(reparsed.entries(), original.entries());
        assert!(serialized.starts_with("# format-version: 1\nreading\tsurface\tpos\tcomment\n"));
    }

    #[test]
    fn constructed_entries_cannot_inject_an_extra_tsv_row() {
        let error = UserDictionary::from_entries(vec![UserDictionaryEntry {
            reading: "さくら".to_owned(),
            surface: "Sakura\nreading\tsurface\tnoun\tcomment".to_owned(),
            part_of_speech: UserPartOfSpeech::ProperNoun,
            comment: String::new(),
        }])
        .expect_err("embedded newline must be rejected");
        assert_eq!(error.kind, UserDictionaryErrorKind::InvalidFieldCharacter);
    }
}
