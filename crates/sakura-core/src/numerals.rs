//! Japanese number readings rewritten into Arabic, full-width, and kanji forms.
//!
//! Mozc ships a number rewriter so `にじゅうよんにち` becomes `24日` instead of a
//! lattice of homophones (`二重` + `呼ん`). Sakura otherwise has only dictionary
//! costs, which is why superscript and circled numerals can win.

use sakura_proto::Overflow;

use crate::TextSink;

const MAX_VALUE: u32 = 9_999;

/// A number optionally followed by a calendar counter, covering a reading prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumericSpan {
    pub value: u32,
    pub counter: Option<NumericCounter>,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericCounter {
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericStyle {
    Arabic,
    FullWidth,
    Kanji,
}

pub const NUMERIC_STYLES: [NumericStyle; 3] = [
    NumericStyle::Arabic,
    NumericStyle::FullWidth,
    NumericStyle::Kanji,
];

const TRADITIONAL_DAYS: [(&str, u32); 13] = [
    ("にじゅうよっか", 24),
    ("じゅうよっか", 14),
    ("ここのか", 9),
    ("ついたち", 1),
    ("とおか", 10),
    ("はつか", 20),
    ("ふつか", 2),
    ("みっか", 3),
    ("よっか", 4),
    ("いつか", 5),
    ("むいか", 6),
    ("なのか", 7),
    ("ようか", 8),
];

const COUNTERS: [(&str, NumericCounter); 4] = [
    ("にち", NumericCounter::Day),
    ("じつ", NumericCounter::Day),
    ("がつ", NumericCounter::Month),
    ("ねん", NumericCounter::Year),
];

const KANJI_DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

impl NumericCounter {
    pub const fn surface(self) -> &'static str {
        match self {
            Self::Day => "日",
            Self::Month => "月",
            Self::Year => "年",
        }
    }
}

impl NumericStyle {
    pub const fn annotation(self) -> &'static str {
        match self {
            Self::Arabic => "算用数字",
            Self::FullWidth => "全角数字",
            Self::Kanji => "漢数字",
        }
    }

    pub fn write(self, span: NumericSpan, sink: &mut impl TextSink) -> Result<(), Overflow> {
        match self {
            Self::Arabic => write_arabic(span.value, sink)?,
            Self::FullWidth => write_fullwidth(span.value, sink)?,
            Self::Kanji => write_kanji(span.value, sink)?,
        }
        if let Some(counter) = span.counter {
            sink.push_str(counter.surface())?;
        }
        Ok(())
    }
}

/// Longest numeric prefix of `reading`, including an optional calendar counter.
pub fn parse_numeric_prefix(reading: &str) -> Option<NumericSpan> {
    if let Some(span) = parse_traditional_day(reading) {
        return Some(span);
    }
    let (value, number_bytes) = parse_ascii_number(reading)
        .or_else(|| parse_fullwidth_number(reading))
        .or_else(|| parse_japanese_number(reading))?;
    let rest = reading.get(number_bytes..)?;
    if let Some((counter, counter_bytes)) = parse_counter(rest) {
        return Some(NumericSpan {
            value,
            counter: Some(counter),
            bytes: number_bytes + counter_bytes,
        });
    }
    Some(NumericSpan {
        value,
        counter: None,
        bytes: number_bytes,
    })
}

pub fn should_emit_numeric_span(span: NumericSpan) -> bool {
    span.counter.is_some() || span.value >= 10
}

pub fn is_decorative_numeral_char(character: char) -> bool {
    matches!(
        character,
        '\u{00B2}'
            | '\u{00B3}'
            | '\u{00B9}'
            | '\u{2070}'
            | '\u{2074}'..='\u{2079}'
            | '\u{2460}'..='\u{2473}'
            | '\u{3251}'..='\u{325F}'
            | '\u{32B1}'..='\u{32BF}'
    )
}

fn parse_traditional_day(reading: &str) -> Option<NumericSpan> {
    for (token, value) in TRADITIONAL_DAYS {
        if reading.starts_with(token) {
            return Some(NumericSpan {
                value,
                counter: Some(NumericCounter::Day),
                bytes: token.len(),
            });
        }
    }
    None
}

fn parse_counter(reading: &str) -> Option<(NumericCounter, usize)> {
    for (token, counter) in COUNTERS {
        if reading.starts_with(token) {
            return Some((counter, token.len()));
        }
    }
    None
}

fn parse_ascii_number(reading: &str) -> Option<(u32, usize)> {
    let mut value = 0u32;
    let mut bytes = 0usize;
    let mut digits = 0u8;
    for character in reading.chars() {
        let Some(digit) = character.to_digit(10) else {
            break;
        };
        if digits == 0 && digit == 0 {
            if reading[character.len_utf8()..]
                .chars()
                .next()
                .is_some_and(|next| next.is_ascii_digit())
            {
                return None;
            }
            return Some((0, character.len_utf8()));
        }
        value = value.checked_mul(10)?.checked_add(digit)?;
        if value > MAX_VALUE {
            return None;
        }
        bytes += character.len_utf8();
        digits += 1;
        if digits == 4 {
            break;
        }
    }
    (digits > 0).then_some((value, bytes))
}

fn parse_fullwidth_number(reading: &str) -> Option<(u32, usize)> {
    let mut value = 0u32;
    let mut bytes = 0usize;
    for character in reading.chars() {
        let Some(digit) = (character as u32).checked_sub('０' as u32) else {
            break;
        };
        if digit > 9 {
            break;
        }
        value = value.checked_mul(10)?.checked_add(digit)?;
        if value > MAX_VALUE {
            return None;
        }
        bytes += character.len_utf8();
    }
    (bytes > 0).then_some((value, bytes))
}

fn parse_japanese_number(reading: &str) -> Option<(u32, usize)> {
    let mut rest = reading;
    let mut total = 0u32;
    let mut consumed = 0usize;
    let mut progressed = false;

    if let Some((value, bytes)) = take_place(rest, &["まん"], 10_000) {
        if value > MAX_VALUE {
            return None;
        }
        total = total.saturating_add(value);
        rest = rest.get(bytes..)?;
        consumed += bytes;
        progressed = true;
    }
    if total > MAX_VALUE {
        return None;
    }
    if let Some((value, bytes)) = take_place(rest, &["せん", "ぜん"], 1_000) {
        total = total.saturating_add(value);
        rest = rest.get(bytes..)?;
        consumed += bytes;
        progressed = true;
    }
    if let Some((value, bytes)) = take_place(rest, &["ひゃく", "びゃく", "ぴゃく"], 100) {
        total = total.saturating_add(value);
        rest = rest.get(bytes..)?;
        consumed += bytes;
        progressed = true;
    }
    if let Some((value, bytes)) = take_place(rest, &["じゅう"], 10) {
        total = total.saturating_add(value);
        rest = rest.get(bytes..)?;
        consumed += bytes;
        progressed = true;
    }
    if let Some((digit, bytes)) = take_ones(rest, false) {
        total = total.saturating_add(digit);
        consumed += bytes;
        progressed = true;
    }
    if !progressed || total > MAX_VALUE {
        return None;
    }
    Some((total, consumed))
}

fn take_place(reading: &str, markers: &[&str], place: u32) -> Option<(u32, usize)> {
    let (digit, digit_bytes) = match take_ones(reading, true) {
        Some(parsed) => parsed,
        None => (1, 0),
    };
    let rest = reading.get(digit_bytes..)?;
    for marker in markers {
        if rest.starts_with(marker) {
            let value = digit.checked_mul(place)?;
            return Some((value, digit_bytes + marker.len()));
        }
    }
    None
}

fn take_ones(reading: &str, in_place: bool) -> Option<(u32, usize)> {
    const DIGITS: [(&str, u32); 17] = [
        ("きゅう", 9),
        ("いち", 1),
        ("いっ", 1),
        ("さん", 3),
        ("よん", 4),
        ("なな", 7),
        ("しち", 7),
        ("はち", 8),
        ("はっ", 8),
        ("ろく", 6),
        ("ろっ", 6),
        ("ぜろ", 0),
        ("れい", 0),
        ("し", 4),
        ("く", 9),
        ("ご", 5),
        ("に", 2),
    ];
    for (token, value) in DIGITS {
        if !reading.starts_with(token) {
            continue;
        }
        if token == "し" || token == "く" {
            let rest = reading.get(token.len()..).unwrap_or("");
            if !in_place && !followed_by_counter_or_place(rest) {
                continue;
            }
        }
        if (token == "いっ" || token == "はっ" || token == "ろっ") && !in_place {
            continue;
        }
        return Some((value, token.len()));
    }
    None
}

fn followed_by_counter_or_place(rest: &str) -> bool {
    COUNTERS.iter().any(|(token, _)| rest.starts_with(token))
        || rest.starts_with("じゅう")
        || rest.starts_with("ひゃく")
        || rest.starts_with("びゃく")
        || rest.starts_with("ぴゃく")
        || rest.starts_with("せん")
        || rest.starts_with("ぜん")
        || rest.starts_with("まん")
}

fn write_arabic(mut value: u32, sink: &mut impl TextSink) -> Result<(), Overflow> {
    let mut digits = [b'0'; 10];
    let mut written = 0usize;
    loop {
        written += 1;
        digits[digits.len() - written] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let text = core::str::from_utf8(&digits[digits.len() - written..]).unwrap_or("");
    sink.push_str(text)
}

fn write_fullwidth(value: u32, sink: &mut impl TextSink) -> Result<(), Overflow> {
    let mut digits = [0u32; 10];
    let mut written = 0usize;
    let mut remaining = value;
    loop {
        digits[written] = remaining % 10;
        written += 1;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for index in (0..written).rev() {
        let character = char::from_u32('０' as u32 + digits[index]).unwrap_or('０');
        sink.push(character)?;
    }
    Ok(())
}

fn write_kanji(value: u32, sink: &mut impl TextSink) -> Result<(), Overflow> {
    if value == 0 {
        return sink.push_str(KANJI_DIGITS[0]);
    }
    let thousands = value / 1_000;
    let hundreds = (value / 100) % 10;
    let tens = (value / 10) % 10;
    let ones = value % 10;
    write_kanji_place(sink, thousands, "千")?;
    write_kanji_place(sink, hundreds, "百")?;
    write_kanji_place(sink, tens, "十")?;
    if ones > 0 {
        sink.push_str(KANJI_DIGITS[ones as usize])?;
    }
    Ok(())
}

fn write_kanji_place(sink: &mut impl TextSink, digit: u32, place: &str) -> Result<(), Overflow> {
    if digit == 0 {
        return Ok(());
    }
    if digit > 1 {
        sink.push_str(KANJI_DIGITS[digit as usize])?;
    }
    sink.push_str(place)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(span: NumericSpan, style: NumericStyle) -> String {
        let mut text = String::new();
        style.write(span, &mut text).expect("fits");
        text
    }

    fn span(reading: &str) -> NumericSpan {
        parse_numeric_prefix(reading).expect(reading)
    }

    #[test]
    fn twenty_four_day_readings_become_calendar_dates() {
        for reading in ["にじゅうよんにち", "にじゅうよっか", "24にち", "２４にち"]
        {
            let parsed = span(reading);
            assert_eq!(parsed.value, 24, "{reading}");
            assert_eq!(parsed.counter, Some(NumericCounter::Day), "{reading}");
            assert_eq!(parsed.bytes, reading.len(), "{reading}");
            assert_eq!(render(parsed, NumericStyle::Arabic), "24日");
            assert_eq!(render(parsed, NumericStyle::FullWidth), "２４日");
            assert_eq!(render(parsed, NumericStyle::Kanji), "二十四日");
        }
    }

    #[test]
    fn traditional_and_arabic_days() {
        assert_eq!(span("ついたち").value, 1);
        assert_eq!(span("ふつか").value, 2);
        assert_eq!(span("よっか").value, 4);
        assert_eq!(span("とおか").value, 10);
        assert_eq!(span("はつか").value, 20);
        assert_eq!(span("4にち").value, 4);
        assert_eq!(span("よんにち").value, 4);
        assert_eq!(span("しがつ").value, 4);
        assert_eq!(span("しがつ").counter, Some(NumericCounter::Month));
    }

    #[test]
    fn ambiguous_shi_is_not_four_by_itself() {
        assert!(parse_numeric_prefix("し").is_none());
        assert_eq!(span("しにち").value, 4);
        assert_eq!(span("しにち").counter, Some(NumericCounter::Day));
    }
}
