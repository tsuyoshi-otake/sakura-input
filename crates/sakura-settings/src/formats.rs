//! Sakura, Microsoft IME, ATOK, and Mozc user-dictionary text formats.
//!
//! The three external formats all use `reading<TAB>surface<TAB>POS<TAB>comment`
//! rows; their headers, encodings, and POS vocabularies differ. Imports reject
//! unknown or non-representable data with an exact line instead of silently
//! dropping fields. Exports use Unicode (UTF-16LE for the Windows-oriented
//! tools and UTF-8 for Mozc/Sakura), so a round trip never depends on CP932
//! representability.

use std::fmt;
use std::io;

use sakura_core::{
    UserDictionary, UserDictionaryEntry, UserPartOfSpeech, MAX_USER_DICTIONARY_ENTRIES,
};
use windows::Win32::Globalization::{MultiByteToWideChar, MB_ERR_INVALID_CHARS};

const CP932: u32 = 932;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryFormat {
    Sakura,
    MicrosoftIme,
    Atok,
    Mozc,
}

impl DictionaryFormat {
    pub const ALL: [Self; 4] = [Self::Sakura, Self::MicrosoftIme, Self::Atok, Self::Mozc];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Sakura => "sakura",
            Self::MicrosoftIme => "ms-ime",
            Self::Atok => "atok",
            Self::Mozc => "mozc",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sakura" => Some(Self::Sakura),
            "ms-ime" | "msime" | "microsoft-ime" => Some(Self::MicrosoftIme),
            "atok" => Some(Self::Atok),
            "mozc" | "google-ime" => Some(Self::Mozc),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryFormatError {
    pub line: usize,
    pub message: String,
}

impl DictionaryFormatError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for DictionaryFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(formatter, "line {}: {}", self.line, self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for DictionaryFormatError {}

pub fn detect_format(source: &str) -> Result<DictionaryFormat, DictionaryFormatError> {
    let first = source
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .find(|line| !line.is_empty())
        .ok_or_else(|| DictionaryFormatError::new(0, "dictionary text is empty"))?;
    let lower = first.to_ascii_lowercase();
    if lower.starts_with("!microsoft ime") {
        return Ok(DictionaryFormat::MicrosoftIme);
    }
    if lower.starts_with("!!atok_tango_text_header")
        || lower
            .strip_prefix("!!dicut")
            .and_then(|version| version.parse::<u16>().ok())
            .is_some_and(|version| version >= 11)
    {
        return Ok(DictionaryFormat::Atok);
    }
    if first == "reading\tsurface\tpos\tcomment" || first.starts_with("# format-version:") {
        return Ok(DictionaryFormat::Sakura);
    }
    if first.starts_with('#') || first.contains('\t') {
        return Ok(DictionaryFormat::Mozc);
    }
    Err(DictionaryFormatError::new(
        1,
        "dictionary format could not be detected",
    ))
}

pub fn parse_dictionary(
    source: &str,
    format: DictionaryFormat,
) -> Result<UserDictionary, DictionaryFormatError> {
    if format == DictionaryFormat::Sakura {
        return UserDictionary::parse_tsv(source)
            .map_err(|error| DictionaryFormatError::new(error.line, error.to_string()));
    }

    validate_external_header(source, format)?;
    let mut entries = Vec::new();
    for (index, raw) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = raw.trim_end_matches('\r').trim_start_matches('\u{feff}');
        if line.is_empty() || is_comment(line, format) {
            continue;
        }
        if entries.len() >= MAX_USER_DICTIONARY_ENTRIES {
            return Err(DictionaryFormatError::new(
                line_number,
                "user dictionary exceeds Sakura's entry cap",
            ));
        }
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 3 {
            return Err(DictionaryFormatError::new(
                line_number,
                "expected reading, surface, and part-of-speech columns",
            ));
        }
        if columns.iter().skip(4).any(|column| !column.is_empty()) {
            return Err(DictionaryFormatError::new(
                line_number,
                "external replacement fields are not representable; no data was imported",
            ));
        }
        let part_of_speech = parse_pos(columns[2], format).ok_or_else(|| {
            DictionaryFormatError::new(
                line_number,
                format!("unsupported part of speech {:?}", columns[2]),
            )
        })?;
        entries.push(UserDictionaryEntry {
            reading: normalize_reading(columns[0]).map_err(|message| {
                DictionaryFormatError::new(line_number, format!("reading: {message}"))
            })?,
            surface: columns[1].to_owned(),
            part_of_speech,
            comment: columns.get(3).copied().unwrap_or_default().to_owned(),
        });
    }
    UserDictionary::from_entries(entries)
        .map_err(|error| DictionaryFormatError::new(error.line, error.to_string()))
}

pub fn serialize_dictionary(dictionary: &UserDictionary, format: DictionaryFormat) -> String {
    if format == DictionaryFormat::Sakura {
        return dictionary.to_tsv();
    }
    let newline = if matches!(
        format,
        DictionaryFormat::MicrosoftIme | DictionaryFormat::Atok
    ) {
        "\r\n"
    } else {
        "\n"
    };
    let mut output = String::new();
    match format {
        DictionaryFormat::MicrosoftIme => {
            output.push_str("!Microsoft IME Dictionary Tool");
            output.push_str(newline);
        }
        DictionaryFormat::Atok => {
            output.push_str("!!ATOK_TANGO_TEXT_HEADER_1");
            output.push_str(newline);
        }
        DictionaryFormat::Mozc => {
            output.push_str("# Sakura Input user dictionary");
            output.push_str(newline);
        }
        DictionaryFormat::Sakura => unreachable!("handled above"),
    }
    for entry in dictionary.entries() {
        output.push_str(&entry.reading);
        output.push('\t');
        output.push_str(&entry.surface);
        output.push('\t');
        output.push_str(export_pos(entry.part_of_speech, format));
        output.push('\t');
        output.push_str(&entry.comment);
        output.push_str(newline);
    }
    output
}

pub fn decode_file_text(bytes: &[u8]) -> io::Result<String> {
    if let Some(payload) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8(payload.to_vec())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    }
    if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(payload, false);
    }
    if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(payload, true);
    }
    if let Ok(source) = core::str::from_utf8(bytes) {
        return Ok(source.to_owned());
    }

    // MS-IME and ATOK exports from older releases commonly use CP932.
    // SAFETY: both calls receive slices whose lengths describe their backing
    // storage. The first asks only for the required UTF-16 length.
    let needed = unsafe { MultiByteToWideChar(CP932, MB_ERR_INVALID_CHARS, bytes, None) };
    if needed <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: `wide` has exactly the size returned by the sizing call.
    let written = unsafe {
        MultiByteToWideChar(
            CP932,
            MB_ERR_INVALID_CHARS,
            bytes,
            Some(wide.as_mut_slice()),
        )
    };
    if written != needed {
        return Err(io::Error::last_os_error());
    }
    String::from_utf16(&wide).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn encode_file_text(source: &str, format: DictionaryFormat) -> Vec<u8> {
    if matches!(
        format,
        DictionaryFormat::MicrosoftIme | DictionaryFormat::Atok
    ) {
        let mut bytes = Vec::with_capacity(2 + source.len() * 2);
        bytes.extend_from_slice(&[0xff, 0xfe]);
        for unit in source.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    } else {
        source.as_bytes().to_vec()
    }
}

fn decode_utf16(payload: &[u8], big_endian: bool) -> io::Result<String> {
    if !payload.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UTF-16 dictionary has a trailing byte",
        ));
    }
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn validate_external_header(
    source: &str,
    format: DictionaryFormat,
) -> Result<(), DictionaryFormatError> {
    if format == DictionaryFormat::Mozc {
        return Ok(());
    }
    let detected = detect_format(source)?;
    if detected == format {
        Ok(())
    } else {
        Err(DictionaryFormatError::new(
            1,
            format!(
                "file header identifies {}, not {}",
                detected.name(),
                format.name()
            ),
        ))
    }
}

fn is_comment(line: &str, format: DictionaryFormat) -> bool {
    match format {
        DictionaryFormat::MicrosoftIme | DictionaryFormat::Atok => line.starts_with('!'),
        DictionaryFormat::Mozc => line.starts_with('#'),
        DictionaryFormat::Sakura => line.starts_with('#'),
    }
}

fn normalize_reading(reading: &str) -> Result<String, &'static str> {
    if reading.is_empty() {
        return Err("is empty");
    }
    let mut normalized = String::with_capacity(reading.len());
    for character in reading.chars() {
        let mapped = if ('\u{30a1}'..='\u{30f6}').contains(&character) {
            char::from_u32(character as u32 - 0x60).ok_or("contains invalid katakana")?
        } else {
            character
        };
        if !(('\u{3041}'..='\u{309f}').contains(&mapped) || mapped == 'ー') {
            return Err("must contain hiragana/full-width katakana and the long-vowel mark only");
        }
        normalized.push(mapped);
    }
    Ok(normalized)
}

fn parse_pos(label: &str, format: DictionaryFormat) -> Option<UserPartOfSpeech> {
    let normalized = label
        .trim()
        .trim_end_matches(['$', '*'])
        .split(':')
        .next()
        .unwrap_or_default();
    let pos = match normalized {
        "名詞" | "一般名詞" | "短縮よみ" | "品詞なし" => UserPartOfSpeech::Noun,
        "固有名詞" | "固有一般" | "固有商品" => UserPartOfSpeech::ProperNoun,
        "人名" | "固有人名" => UserPartOfSpeech::PersonalName,
        "姓" | "固有人姓" => UserPartOfSpeech::FamilyName,
        "名" | "固有人名（名）" => UserPartOfSpeech::FirstName,
        "組織" | "固有組織" => UserPartOfSpeech::Organization,
        "地名" | "固有地名" => UserPartOfSpeech::Place,
        "名詞サ変" | "サ変名詞" => UserPartOfSpeech::SaNoun,
        "名詞形動" | "形容動詞" => UserPartOfSpeech::AdjectivalNoun,
        "数" | "数詞" => UserPartOfSpeech::Number,
        "アルファベット" | "英字" => UserPartOfSpeech::Alphabet,
        "記号" => UserPartOfSpeech::Symbol,
        "副詞" => UserPartOfSpeech::Adverb,
        "連体詞" => UserPartOfSpeech::PrenounAdjectival,
        "接続詞" => UserPartOfSpeech::Conjunction,
        "感動詞" => UserPartOfSpeech::Interjection,
        "接頭語" | "接頭詞" => UserPartOfSpeech::Prefix,
        "助数詞" | "接尾助数詞" => UserPartOfSpeech::CounterSuffix,
        "接尾語" | "接尾一般" => UserPartOfSpeech::GenericSuffix,
        "接尾人名" => UserPartOfSpeech::PersonNameSuffix,
        "接尾地名" => UserPartOfSpeech::PlaceNameSuffix,
        _ => return None,
    };
    let _ = format;
    Some(pos)
}

fn export_pos(pos: UserPartOfSpeech, format: DictionaryFormat) -> &'static str {
    if format == DictionaryFormat::Atok {
        return match pos {
            UserPartOfSpeech::Noun => "名詞",
            UserPartOfSpeech::ProperNoun => "固有一般",
            UserPartOfSpeech::PersonalName => "人名",
            UserPartOfSpeech::FamilyName => "固有人姓",
            UserPartOfSpeech::FirstName => "固有人名（名）",
            UserPartOfSpeech::Organization => "固有組織",
            UserPartOfSpeech::Place => "固有地名",
            UserPartOfSpeech::SaNoun => "名詞サ変",
            UserPartOfSpeech::AdjectivalNoun => "名詞形動",
            UserPartOfSpeech::Number => "数詞",
            UserPartOfSpeech::Alphabet => "英字",
            UserPartOfSpeech::Symbol => "記号",
            UserPartOfSpeech::Adverb => "副詞",
            UserPartOfSpeech::PrenounAdjectival => "連体詞",
            UserPartOfSpeech::Conjunction => "接続詞",
            UserPartOfSpeech::Interjection => "感動詞",
            UserPartOfSpeech::Prefix => "接頭語",
            UserPartOfSpeech::CounterSuffix => "助数詞",
            UserPartOfSpeech::GenericSuffix => "接尾一般",
            UserPartOfSpeech::PersonNameSuffix => "接尾人名",
            UserPartOfSpeech::PlaceNameSuffix => "接尾地名",
        };
    }
    match pos {
        UserPartOfSpeech::Noun => "名詞",
        UserPartOfSpeech::ProperNoun => "固有名詞",
        UserPartOfSpeech::PersonalName => "人名",
        UserPartOfSpeech::FamilyName => "姓",
        UserPartOfSpeech::FirstName => "名",
        UserPartOfSpeech::Organization => "組織",
        UserPartOfSpeech::Place => "地名",
        UserPartOfSpeech::SaNoun => "名詞サ変",
        UserPartOfSpeech::AdjectivalNoun => "名詞形動",
        UserPartOfSpeech::Number => "数",
        UserPartOfSpeech::Alphabet => "アルファベット",
        UserPartOfSpeech::Symbol => "記号",
        UserPartOfSpeech::Adverb => "副詞",
        UserPartOfSpeech::PrenounAdjectival => "連体詞",
        UserPartOfSpeech::Conjunction => "接続詞",
        UserPartOfSpeech::Interjection => "感動詞",
        UserPartOfSpeech::Prefix => "接頭語",
        UserPartOfSpeech::CounterSuffix => "助数詞",
        UserPartOfSpeech::GenericSuffix => "接尾一般",
        UserPartOfSpeech::PersonNameSuffix => "接尾人名",
        UserPartOfSpeech::PlaceNameSuffix => "接尾地名",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_pos_dictionary() -> UserDictionary {
        UserDictionary::from_entries(
            UserPartOfSpeech::ALL
                .into_iter()
                .enumerate()
                .map(|(index, part_of_speech)| UserDictionaryEntry {
                    reading: format!("さくら{}", "あ".repeat(index + 1)),
                    surface: format!("Surface-{index}"),
                    part_of_speech,
                    comment: format!("comment-{index}"),
                })
                .collect(),
        )
        .expect("all POS dictionary")
    }

    #[test]
    fn every_supported_pos_roundtrips_through_all_three_external_formats() {
        let dictionary = every_pos_dictionary();
        for format in [
            DictionaryFormat::MicrosoftIme,
            DictionaryFormat::Atok,
            DictionaryFormat::Mozc,
        ] {
            let export = serialize_dictionary(&dictionary, format);
            assert_eq!(detect_format(&export), Ok(format));
            let imported = parse_dictionary(&export, format).expect("re-import");
            assert_eq!(
                imported.entries(),
                dictionary.entries(),
                "{}",
                format.name()
            );
        }
    }

    #[test]
    fn windows_exports_are_utf16_and_decode_without_field_loss() {
        let source = "!!ATOK_TANGO_TEXT_HEADER_1\r\nさくら\t櫻\t名詞\t旧字体\r\n";
        let bytes = encode_file_text(source, DictionaryFormat::Atok);
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        assert_eq!(decode_file_text(&bytes).expect("UTF-16"), source);
        let imported = parse_dictionary(
            &decode_file_text(&bytes).expect("decode"),
            DictionaryFormat::Atok,
        )
        .expect("import");
        assert_eq!(imported.entries()[0].comment, "旧字体");
    }

    #[test]
    fn katakana_readings_normalize_and_unknown_pos_or_extra_fields_fail_atomically() {
        let normalized = parse_dictionary(
            "# mozc\nサクラ\tSakura\t固有名詞\tproduct\n",
            DictionaryFormat::Mozc,
        )
        .expect("katakana reading");
        assert_eq!(normalized.entries()[0].reading, "さくら");

        for bad in [
            "# mozc\nさくら\tSakura\t未知\tcomment\n",
            "# mozc\nさくら\tSakura\t名詞\tcomment\textra\n",
        ] {
            assert!(parse_dictionary(bad, DictionaryFormat::Mozc).is_err());
        }
    }
}
