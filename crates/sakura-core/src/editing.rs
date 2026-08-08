//! Allocation-free editing transforms used by F6-F10 and IT identifiers.
//!
//! Transformations write through [`TextSink`] so the dispatcher can reuse its
//! fixed scratch buffer. Explicit F9/F10 intent is represented by the `full`
//! argument and therefore bypasses the ordinary width policy exactly once.

use sakura_proto::Overflow;

use crate::TextSink;

/// F6-F10 transform applied to one focused segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentTransform {
    #[default]
    None,
    Hiragana,
    Katakana,
    HalfKatakana,
    FullAlnum,
    HalfAlnum,
}

/// Generated identifier casing offered beside an English conversion surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierStyle {
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
}

impl IdentifierStyle {
    pub const ALL: [Self; 4] = [Self::Camel, Self::Snake, Self::ScreamingSnake, Self::Kebab];

    pub const fn annotation(self) -> &'static str {
        match self {
            Self::Camel => "camelCase",
            Self::Snake => "snake_case",
            Self::ScreamingSnake => "SCREAMING_SNAKE",
            Self::Kebab => "kebab-case",
        }
    }
}

/// Applies one focused-segment transform.
///
/// `raw_romaji` is used by F9/F10, matching Microsoft IME: those transforms
/// operate on what was physically typed, not on a semantic English candidate.
pub fn transform_into(
    source: &str,
    raw_romaji: &str,
    transform: SegmentTransform,
    case_cycle: u8,
    sink: &mut impl TextSink,
) -> Result<(), Overflow> {
    match transform {
        SegmentTransform::None => sink.push_str(source),
        SegmentTransform::Hiragana => write_hiragana(source, sink),
        SegmentTransform::Katakana => write_katakana(source, sink),
        SegmentTransform::HalfKatakana => write_half_katakana(source, sink),
        SegmentTransform::FullAlnum => write_alnum(raw_romaji, case_cycle, true, sink),
        SegmentTransform::HalfAlnum => write_alnum(raw_romaji, case_cycle, false, sink),
    }
}

pub(crate) fn hiragana_char(character: char) -> char {
    match character {
        '\u{30a1}'..='\u{30f6}' => char::from_u32(u32::from(character) - 0x60).unwrap_or(character),
        _ => character,
    }
}

pub(crate) fn katakana_char(character: char) -> char {
    match character {
        '\u{3041}'..='\u{3096}' => char::from_u32(u32::from(character) + 0x60).unwrap_or(character),
        _ => character,
    }
}

pub fn write_hiragana(source: &str, sink: &mut impl TextSink) -> Result<(), Overflow> {
    for character in source.chars() {
        sink.push(hiragana_char(character))?;
    }
    Ok(())
}

pub fn write_katakana(source: &str, sink: &mut impl TextSink) -> Result<(), Overflow> {
    for character in source.chars() {
        sink.push(katakana_char(character))?;
    }
    Ok(())
}

pub fn write_half_katakana(source: &str, sink: &mut impl TextSink) -> Result<(), Overflow> {
    for character in source.chars() {
        let katakana = katakana_char(character);
        if let Some(mapped) = half_katakana(katakana) {
            sink.push_str(mapped)?;
        } else {
            sink.push(katakana)?;
        }
    }
    Ok(())
}

pub(crate) fn half_katakana(character: char) -> Option<&'static str> {
    Some(match character {
        '。' => "｡",
        '「' => "｢",
        '」' => "｣",
        '、' => "､",
        '・' => "･",
        'ヲ' => "ｦ",
        'ァ' => "ｧ",
        'ィ' => "ｨ",
        'ゥ' => "ｩ",
        'ェ' => "ｪ",
        'ォ' => "ｫ",
        'ャ' => "ｬ",
        'ュ' => "ｭ",
        'ョ' => "ｮ",
        'ッ' => "ｯ",
        'ー' => "ｰ",
        'ア' => "ｱ",
        'イ' => "ｲ",
        'ウ' => "ｳ",
        'エ' => "ｴ",
        'オ' => "ｵ",
        'カ' => "ｶ",
        'キ' => "ｷ",
        'ク' => "ｸ",
        'ケ' => "ｹ",
        'コ' => "ｺ",
        'サ' => "ｻ",
        'シ' => "ｼ",
        'ス' => "ｽ",
        'セ' => "ｾ",
        'ソ' => "ｿ",
        'タ' => "ﾀ",
        'チ' => "ﾁ",
        'ツ' => "ﾂ",
        'テ' => "ﾃ",
        'ト' => "ﾄ",
        'ナ' => "ﾅ",
        'ニ' => "ﾆ",
        'ヌ' => "ﾇ",
        'ネ' => "ﾈ",
        'ノ' => "ﾉ",
        'ハ' => "ﾊ",
        'ヒ' => "ﾋ",
        'フ' => "ﾌ",
        'ヘ' => "ﾍ",
        'ホ' => "ﾎ",
        'マ' => "ﾏ",
        'ミ' => "ﾐ",
        'ム' => "ﾑ",
        'メ' => "ﾒ",
        'モ' => "ﾓ",
        'ヤ' => "ﾔ",
        'ユ' => "ﾕ",
        'ヨ' => "ﾖ",
        'ラ' => "ﾗ",
        'リ' => "ﾘ",
        'ル' => "ﾙ",
        'レ' => "ﾚ",
        'ロ' => "ﾛ",
        'ワ' => "ﾜ",
        'ン' => "ﾝ",
        'ガ' => "ｶﾞ",
        'ギ' => "ｷﾞ",
        'グ' => "ｸﾞ",
        'ゲ' => "ｹﾞ",
        'ゴ' => "ｺﾞ",
        'ザ' => "ｻﾞ",
        'ジ' => "ｼﾞ",
        'ズ' => "ｽﾞ",
        'ゼ' => "ｾﾞ",
        'ゾ' => "ｿﾞ",
        'ダ' => "ﾀﾞ",
        'ヂ' => "ﾁﾞ",
        'ヅ' => "ﾂﾞ",
        'デ' => "ﾃﾞ",
        'ド' => "ﾄﾞ",
        'バ' => "ﾊﾞ",
        'ビ' => "ﾋﾞ",
        'ブ' => "ﾌﾞ",
        'ベ' => "ﾍﾞ",
        'ボ' => "ﾎﾞ",
        'パ' => "ﾊﾟ",
        'ピ' => "ﾋﾟ",
        'プ' => "ﾌﾟ",
        'ペ' => "ﾍﾟ",
        'ポ' => "ﾎﾟ",
        'ヴ' => "ｳﾞ",
        _ => return None,
    })
}

fn write_alnum(
    source: &str,
    case_cycle: u8,
    full: bool,
    sink: &mut impl TextSink,
) -> Result<(), Overflow> {
    let mut first_letter = true;
    for character in source.chars() {
        let cased = if character.is_ascii_alphabetic() {
            let result = match case_cycle % 3 {
                0 => character.to_ascii_lowercase(),
                1 => character.to_ascii_uppercase(),
                _ if first_letter => character.to_ascii_uppercase(),
                _ => character.to_ascii_lowercase(),
            };
            first_letter = false;
            result
        } else {
            character
        };
        let mapped = if full {
            match cased {
                ' ' => '\u{3000}',
                '!'..='~' => char::from_u32(u32::from(cased) + 0xfee0).unwrap_or(cased),
                _ => cased,
            }
        } else {
            match cased {
                '\u{3000}' => ' ',
                '\u{ff01}'..='\u{ff5e}' => {
                    char::from_u32(u32::from(cased) - 0xfee0).unwrap_or(cased)
                }
                _ => cased,
            }
        };
        sink.push(mapped)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct WordSpan {
    start: usize,
    end: usize,
}

/// Writes an identifier-case rendering of an ASCII word surface.
///
/// Returns `false` without writing when the source contains no ASCII word.
pub fn identifier_into(
    source: &str,
    style: IdentifierStyle,
    sink: &mut impl TextSink,
) -> Result<bool, Overflow> {
    // Identifier variants are for English/ASCII technical surfaces. Treating
    // the digits inside a mixed Japanese candidate (for example `候補03`) as
    // an identifier would discard the Japanese prefix and pollute paging with
    // unrelated numeric candidates.
    if !source.is_ascii() {
        return Ok(false);
    }
    let mut words = [WordSpan::default(); 32];
    let mut count = 0usize;
    let bytes = source.as_bytes();
    let mut start = None;
    for index in 0..=bytes.len() {
        let current = bytes.get(index).copied();
        let is_word = current.is_some_and(|byte| byte.is_ascii_alphanumeric());
        let camel_boundary = index > 0
            && index < bytes.len()
            && bytes[index - 1].is_ascii_lowercase()
            && bytes[index].is_ascii_uppercase();
        if camel_boundary {
            if let Some(word_start) = start {
                if count < words.len() {
                    words[count] = WordSpan {
                        start: word_start,
                        end: index,
                    };
                    count += 1;
                }
            }
            start = Some(index);
        } else if is_word && start.is_none() {
            start = Some(index);
        } else if !is_word {
            if let Some(word_start) = start.take() {
                if count < words.len() {
                    words[count] = WordSpan {
                        start: word_start,
                        end: index,
                    };
                    count += 1;
                }
            }
        }
    }
    if count == 0 {
        return Ok(false);
    }

    for (word_index, span) in words[..count].iter().enumerate() {
        if word_index > 0 {
            match style {
                IdentifierStyle::Snake | IdentifierStyle::ScreamingSnake => sink.push('_')?,
                IdentifierStyle::Kebab => sink.push('-')?,
                IdentifierStyle::Camel => {}
            }
        }
        let mut first = true;
        for character in source[span.start..span.end].chars() {
            let mapped = match style {
                IdentifierStyle::ScreamingSnake => character.to_ascii_uppercase(),
                IdentifierStyle::Camel if word_index > 0 && first => character.to_ascii_uppercase(),
                _ => character.to_ascii_lowercase(),
            };
            first = false;
            sink.push(mapped)?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::FixedStr;

    fn transformed(source: &str, raw: &str, transform: SegmentTransform, cycle: u8) -> String {
        let mut output = FixedStr::<128>::new();
        transform_into(source, raw, transform, cycle, &mut output).expect("fixture fits");
        output.as_str().to_owned()
    }

    #[test]
    fn f6_f8_cover_hiragana_katakana_and_half_katakana() {
        assert_eq!(
            transformed("ガッツ", "gattsu", SegmentTransform::Hiragana, 0),
            "がっつ"
        );
        assert_eq!(
            transformed("がっつ", "gattsu", SegmentTransform::Katakana, 0),
            "ガッツ"
        );
        assert_eq!(
            transformed("がっつ", "gattsu", SegmentTransform::HalfKatakana, 0),
            "ｶﾞｯﾂ"
        );
    }

    #[test]
    fn f9_f10_use_raw_romaji_and_cycle_case() {
        assert_eq!(
            transformed("どっかー", "docker", SegmentTransform::HalfAlnum, 0),
            "docker"
        );
        assert_eq!(
            transformed("どっかー", "docker", SegmentTransform::HalfAlnum, 1),
            "DOCKER"
        );
        assert_eq!(
            transformed("どっかー", "docker", SegmentTransform::FullAlnum, 2),
            "Ｄｏｃｋｅｒ"
        );
    }

    #[test]
    fn identifier_styles_are_generated_without_stored_dictionary_rows() {
        let expected = [
            "pullRequest",
            "pull_request",
            "PULL_REQUEST",
            "pull-request",
        ];
        for (style, expected) in IdentifierStyle::ALL.into_iter().zip(expected) {
            let mut output = FixedStr::<64>::new();
            assert!(identifier_into("Pull request", style, &mut output).expect("fits"));
            assert_eq!(output.as_str(), expected);
        }
    }

    #[test]
    fn non_ascii_surface_is_not_an_identifier() {
        let mut output = FixedStr::<32>::new();
        assert!(!identifier_into("候補03", IdentifierStyle::Camel, &mut output).expect("fits"));
        assert!(output.is_empty());
    }
}
