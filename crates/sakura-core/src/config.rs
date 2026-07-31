//! The config file format, and the hand-written parser for it.
//!
//! Every data file that ships with Sakura Input — the romaji table, the key
//! map presets, the user's overrides — is written in a deliberately small
//! subset of TOML. The full-scratch rule (DESIGN 3.1) covers data formats
//! too, so there is no `toml` crate here and never will be; the subset exists
//! so that the parser can be a few hundred lines of obviously-correct code
//! instead of a spec implementation.
//!
//! What the subset has:
//!
//! ```toml
//! # comments
//! [section]
//! bare_key = "value"
//! "quoted key" = ["value", "with carry"]
//! ```
//!
//! What it deliberately does not have: nested tables, inline tables, numbers,
//! booleans, dates, multi-line strings, literal strings, and values spanning
//! more than one line. Anything a config file genuinely needs that is missing
//! should be added here on purpose, not arrived at by accident.
//!
//! Parsing happens once at load, so this code optimizes for being obviously
//! right rather than for speed, and owns its strings rather than borrowing
//! from the source text.

use core::fmt;

/// A parsed config file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Document {
    sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    name: String,
    entries: Vec<Entry>,
}

/// One `key = value` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
}

/// The two value shapes the subset allows.
///
/// A list rather than an inline table because the one thing a romaji entry
/// needs beyond its output — the carry-over consonant of `tt` → `っ` + `t` —
/// is a second string, and a two-element list expresses that without adding a
/// whole table syntax to the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    List(Vec<String>),
}

impl Value {
    /// The value if it is a single string, `None` if it is a list.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            Value::List(_) => None,
        }
    }

    /// The value if it is a list, `None` if it is a single string.
    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Value::Text(_) => None,
            Value::List(items) => Some(items),
        }
    }
}

impl Document {
    /// The entries of `name`, in file order, or `None` if there is no such
    /// section.
    ///
    /// File order is part of the contract: a caller that reports a bad entry
    /// wants to name the line the user wrote, and a caller building a table
    /// wants deterministic output.
    pub fn section(&self, name: &str) -> Option<&[Entry]> {
        self.sections
            .iter()
            .find(|section| section.name == name)
            .map(|section| section.entries.as_slice())
    }

    /// Every section name, in file order.
    pub fn section_names(&self) -> impl Iterator<Item = &str> {
        self.sections.iter().map(|section| section.name.as_str())
    }
}

/// What went wrong, and on which line of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based, so it matches what an editor shows.
    pub line: usize,
    pub kind: ErrorKind,
}

/// The specific fault. Every variant names something a human can go and fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// A `[` with no closing `]`.
    UnclosedSection,
    /// `[]`, which names nothing.
    EmptySectionName,
    /// The same section header twice. Almost always a copy-paste that would
    /// otherwise silently drop half the entries.
    DuplicateSection,
    /// A `key = value` line before any `[section]` header.
    KeyOutsideSection,
    /// A key with no `=` after it.
    MissingEquals,
    /// An empty key, or one containing a character bare keys do not allow.
    InvalidKey,
    /// The same key twice in one section. Silently shadowing a romaji mapping
    /// is exactly the kind of bug that survives to a release.
    DuplicateKey,
    /// A value that is neither a quoted string nor a list.
    InvalidValue,
    /// A string with no closing quote before the end of the line.
    UnterminatedString,
    /// A `\` followed by something the subset does not define.
    InvalidEscape,
    /// A `\u` escape that is not four hex digits, or that names a surrogate
    /// or other non-character.
    InvalidUnicodeEscape,
    /// A list with no closing `]`, or a missing comma between items.
    MalformedList,
    /// Text after the value that is not a comment.
    TrailingContent,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            ErrorKind::UnclosedSection => "section header is missing its ']'",
            ErrorKind::EmptySectionName => "section header names nothing",
            ErrorKind::DuplicateSection => "section is declared twice",
            ErrorKind::KeyOutsideSection => "entry appears before any section header",
            ErrorKind::MissingEquals => "entry is missing its '='",
            ErrorKind::InvalidKey => "key is empty or contains an unsupported character",
            ErrorKind::DuplicateKey => "key is declared twice in this section",
            ErrorKind::InvalidValue => "value must be a quoted string or a list of them",
            ErrorKind::UnterminatedString => "string is missing its closing quote",
            ErrorKind::InvalidEscape => "unsupported escape sequence",
            ErrorKind::InvalidUnicodeEscape => "\\u escape is not four hex digits of a character",
            ErrorKind::MalformedList => "list is malformed",
            ErrorKind::TrailingContent => "unexpected text after the value",
        };
        f.write_str(text)
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.kind)
    }
}

impl std::error::Error for ParseError {}

/// Parses `source` into a [`Document`].
pub fn parse(source: &str) -> Result<Document, ParseError> {
    let mut document = Document::default();
    let mut current: Option<usize> = None;

    for (index, raw) in source.lines().enumerate() {
        let line = index + 1;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }

        let fail = |kind| ParseError { line, kind };

        if let Some(rest) = text.strip_prefix('[') {
            let name = parse_section_header(rest).map_err(fail)?;
            if document.sections.iter().any(|s| s.name == name) {
                return Err(fail(ErrorKind::DuplicateSection));
            }
            document.sections.push(Section {
                name: name.to_string(),
                entries: Vec::new(),
            });
            current = Some(document.sections.len() - 1);
            continue;
        }

        let entry = parse_entry(text).map_err(fail)?;
        let Some(index) = current else {
            return Err(fail(ErrorKind::KeyOutsideSection));
        };
        let Some(section) = document.sections.get_mut(index) else {
            return Err(fail(ErrorKind::KeyOutsideSection));
        };
        if section.entries.iter().any(|e| e.key == entry.key) {
            return Err(fail(ErrorKind::DuplicateKey));
        }
        section.entries.push(entry);
    }

    Ok(document)
}

/// Reads a section name from the text after the opening `[`.
fn parse_section_header(rest: &str) -> Result<&str, ErrorKind> {
    let Some(end) = rest.find(']') else {
        return Err(ErrorKind::UnclosedSection);
    };
    let (name, tail) = rest.split_at(end);
    let name = name.trim();
    if name.is_empty() {
        return Err(ErrorKind::EmptySectionName);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(ErrorKind::InvalidKey);
    }
    // `tail` still starts with the `]` itself.
    check_only_comment_follows(&tail[1..])?;
    Ok(name)
}

/// Reads one `key = value` line.
fn parse_entry(text: &str) -> Result<Entry, ErrorKind> {
    let (key, rest) = parse_key(text)?;
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return Err(ErrorKind::MissingEquals);
    };

    let (value, rest) = parse_value(rest.trim_start())?;
    check_only_comment_follows(rest)?;
    Ok(Entry { key, value })
}

/// Reads a bare or quoted key, returning it and the text after it.
fn parse_key(text: &str) -> Result<(String, &str), ErrorKind> {
    if text.starts_with('"') {
        let (key, rest) = parse_string(text)?;
        if key.is_empty() {
            return Err(ErrorKind::InvalidKey);
        }
        return Ok((key, rest));
    }

    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        .unwrap_or(text.len());
    if end == 0 {
        return Err(ErrorKind::InvalidKey);
    }
    let (key, rest) = text.split_at(end);
    Ok((key.to_string(), rest))
}

/// Reads a value, returning it and the text after it.
fn parse_value(text: &str) -> Result<(Value, &str), ErrorKind> {
    if text.starts_with('"') {
        let (item, rest) = parse_string(text)?;
        return Ok((Value::Text(item), rest));
    }
    if let Some(rest) = text.strip_prefix('[') {
        let (items, rest) = parse_list(rest)?;
        return Ok((Value::List(items), rest));
    }
    Err(ErrorKind::InvalidValue)
}

/// Reads the items of a list, starting after the opening `[`.
fn parse_list(mut text: &str) -> Result<(Vec<String>, &str), ErrorKind> {
    let mut items = Vec::new();
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix(']') {
            return Ok((items, rest));
        }
        if !text.starts_with('"') {
            return Err(ErrorKind::MalformedList);
        }
        let (item, rest) = parse_string(text)?;
        items.push(item);

        text = rest.trim_start();
        if let Some(rest) = text.strip_prefix(',') {
            text = rest;
        } else if !text.starts_with(']') {
            // No comma and no close: two strings jammed together.
            return Err(ErrorKind::MalformedList);
        }
    }
}

/// Reads a quoted string, starting at its opening `"`, returning its decoded
/// contents and the text after the closing quote.
fn parse_string(text: &str) -> Result<(String, &str), ErrorKind> {
    let mut chars = text.char_indices();
    // The caller checked this, but consuming it here keeps the offsets honest.
    match chars.next() {
        Some((_, '"')) => {}
        _ => return Err(ErrorKind::InvalidValue),
    }

    let mut out = String::new();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' => {
                let rest = text.get(offset + 1..).unwrap_or("");
                return Ok((out, rest));
            }
            '\\' => {
                let Some((_, escape)) = chars.next() else {
                    return Err(ErrorKind::UnterminatedString);
                };
                match escape {
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'u' => out.push(parse_unicode_escape(&mut chars)?),
                    _ => return Err(ErrorKind::InvalidEscape),
                }
            }
            _ => out.push(c),
        }
    }

    Err(ErrorKind::UnterminatedString)
}

/// Reads the four hex digits of a `\uXXXX` escape.
fn parse_unicode_escape(chars: &mut core::str::CharIndices<'_>) -> Result<char, ErrorKind> {
    let mut code = 0u32;
    for _ in 0..4 {
        let Some((_, digit)) = chars.next() else {
            return Err(ErrorKind::InvalidUnicodeEscape);
        };
        let Some(value) = digit.to_digit(16) else {
            return Err(ErrorKind::InvalidUnicodeEscape);
        };
        code = code * 16 + value;
    }
    // Rejects the surrogate range as well as anything else that is not a
    // scalar value, which is why this is a `char::from_u32` and not a cast.
    char::from_u32(code).ok_or(ErrorKind::InvalidUnicodeEscape)
}

/// Accepts only whitespace, or whitespace then a comment, to the end of line.
fn check_only_comment_follows(text: &str) -> Result<(), ErrorKind> {
    let rest = text.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(ErrorKind::TrailingContent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(source: &str) -> ErrorKind {
        parse(source).expect_err("expected a parse error").kind
    }

    fn text_of(document: &Document, section: &str, key: &str) -> String {
        document
            .section(section)
            .expect("section")
            .iter()
            .find(|entry| entry.key == key)
            .expect("key")
            .value
            .as_text()
            .expect("text value")
            .to_string()
    }

    #[test]
    fn parses_a_realistic_file() {
        let document = parse(
            r#"
# Romaji table.

[kana]
a = "あ"
ka = "か"
# Sokuon: doubled consonant, with the consonant carried over.
kk = ["っ", "k"]

[options]
"quoted key" = "value"  # trailing comment
"#,
        )
        .expect("parse");

        assert_eq!(
            document.section_names().collect::<Vec<_>>(),
            ["kana", "options"]
        );
        assert_eq!(text_of(&document, "kana", "a"), "あ");
        assert_eq!(text_of(&document, "options", "quoted key"), "value");

        let entries = document.section("kana").expect("kana");
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[2].value.as_list().expect("list"),
            ["っ".to_string(), "k".to_string()]
        );
    }

    #[test]
    fn preserves_file_order() {
        let document = parse("[t]\nc = \"3\"\na = \"1\"\nb = \"2\"\n").expect("parse");
        let keys: Vec<&str> = document
            .section("t")
            .expect("t")
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        assert_eq!(keys, ["c", "a", "b"]);
    }

    #[test]
    fn unknown_section_is_none() {
        let document = parse("[a]\nk = \"v\"\n").expect("parse");
        assert!(document.section("b").is_none());
    }

    #[test]
    fn empty_and_comment_only_sources_parse_to_nothing() {
        for source in ["", "\n\n", "# just a comment\n", "   \n\t\n"] {
            let document = parse(source).expect("parse");
            assert_eq!(document.section_names().count(), 0, "source: {source:?}");
        }
    }

    #[test]
    fn empty_section_is_allowed() {
        let document = parse("[empty]\n").expect("parse");
        assert_eq!(document.section("empty").expect("empty").len(), 0);
    }

    #[test]
    fn empty_string_and_empty_list_are_values() {
        let document = parse("[t]\na = \"\"\nb = []\n").expect("parse");
        assert_eq!(text_of(&document, "t", "a"), "");
        let entries = document.section("t").expect("t");
        assert_eq!(entries[1].value.as_list().expect("list").len(), 0);
    }

    #[test]
    fn accepts_whitespace_anywhere_it_is_insignificant() {
        let document =
            parse("  [ t ]  # header comment\n\ta\t=\t[ \"x\" , \"y\" , ]\t# c\n").expect("parse");
        assert_eq!(
            document.section("t").expect("t")[0]
                .value
                .as_list()
                .expect("list"),
            ["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn decodes_every_supported_escape() {
        let document =
            parse("[t]\na = \"q\\\"b\\\\s\\nn\\tt\\rr\\u3042\\u0041\"\n").expect("parse");
        assert_eq!(text_of(&document, "t", "a"), "q\"b\\s\nn\tt\rrあA");
    }

    /// A `#` inside a string is text, not the start of a comment. Getting this
    /// wrong would quietly truncate any mapping that produces one.
    #[test]
    fn hash_inside_a_string_is_not_a_comment() {
        let document = parse("[t]\nsharp = \"C#\"\nlist = [\"a#b\"]\n").expect("parse");
        assert_eq!(text_of(&document, "t", "sharp"), "C#");
        assert_eq!(
            document.section("t").expect("t")[1]
                .value
                .as_list()
                .expect("list"),
            ["a#b".to_string()]
        );
    }

    /// The key is a romaji sequence, so it will contain characters bare keys
    /// do not allow — `n'` and `[` among them — and must be quotable.
    #[test]
    fn quoted_keys_carry_punctuation() {
        let document =
            parse("[kana]\n\"n'\" = \"ん\"\n\"[\" = \"「\"\n\"=\" = \"＝\"\n").expect("parse");
        assert_eq!(text_of(&document, "kana", "n'"), "ん");
        assert_eq!(text_of(&document, "kana", "["), "「");
        assert_eq!(text_of(&document, "kana", "="), "＝");
    }

    #[test]
    fn duplicate_key_is_rejected() {
        assert_eq!(
            kind_of("[t]\na = \"1\"\na = \"2\"\n"),
            ErrorKind::DuplicateKey
        );
    }

    /// Duplicates only collide inside one section.
    #[test]
    fn the_same_key_in_two_sections_is_fine() {
        let document = parse("[a]\nk = \"1\"\n[b]\nk = \"2\"\n").expect("parse");
        assert_eq!(text_of(&document, "a", "k"), "1");
        assert_eq!(text_of(&document, "b", "k"), "2");
    }

    #[test]
    fn duplicate_section_is_rejected() {
        assert_eq!(
            kind_of("[t]\na = \"1\"\n[t]\nb = \"2\"\n"),
            ErrorKind::DuplicateSection
        );
    }

    #[test]
    fn every_malformed_line_names_its_fault() {
        let cases = [
            ("[t\n", ErrorKind::UnclosedSection),
            ("[]\n", ErrorKind::EmptySectionName),
            ("[a b]\n", ErrorKind::InvalidKey),
            ("a = \"1\"\n", ErrorKind::KeyOutsideSection),
            ("[t]\na \"1\"\n", ErrorKind::MissingEquals),
            ("[t]\na\n", ErrorKind::MissingEquals),
            ("[t]\n\"\" = \"1\"\n", ErrorKind::InvalidKey),
            ("[t]\n= \"1\"\n", ErrorKind::InvalidKey),
            ("[t]\na = 1\n", ErrorKind::InvalidValue),
            ("[t]\na = true\n", ErrorKind::InvalidValue),
            ("[t]\na = \"1\n", ErrorKind::UnterminatedString),
            ("[t]\na = \"\\q\"\n", ErrorKind::InvalidEscape),
            ("[t]\na = \"\\u12\"\n", ErrorKind::InvalidUnicodeEscape),
            ("[t]\na = \"\\uZZZZ\"\n", ErrorKind::InvalidUnicodeEscape),
            ("[t]\na = [\"x\"\n", ErrorKind::MalformedList),
            ("[t]\na = [\"x\" \"y\"]\n", ErrorKind::MalformedList),
            ("[t]\na = [1]\n", ErrorKind::MalformedList),
            ("[t]\na = \"1\" junk\n", ErrorKind::TrailingContent),
            ("[t] junk\n", ErrorKind::TrailingContent),
        ];
        for (source, expected) in cases {
            assert_eq!(kind_of(source), expected, "source: {source:?}");
        }
    }

    /// A surrogate half is not a `char`, and accepting one would mean building
    /// a `String` that is not valid UTF-8.
    #[test]
    fn lone_surrogate_escape_is_rejected() {
        assert_eq!(
            kind_of("[t]\na = \"\\ud800\"\n"),
            ErrorKind::InvalidUnicodeEscape
        );
    }

    #[test]
    fn error_reports_the_line_the_editor_shows() {
        let error = parse("[t]\n\n# comment\na = 1\n").expect_err("error");
        assert_eq!(error.line, 4);
        assert_eq!(error.kind, ErrorKind::InvalidValue);
        assert!(error.to_string().starts_with("line 4: "));
    }

    /// The parser is fed user-editable files, so it has to survive anything —
    /// and it runs in the engine, where `panic = "abort"` makes a panic fatal.
    #[test]
    fn arbitrary_input_never_panics() {
        let alphabet = [
            "[", "]", "=", "\"", "\\", "#", "u", "a", ",", " ", "\n", "\t", "あ", "'", "1",
        ];
        // A xorshift64* PRNG rather than a dependency: the workspace ships no
        // third-party crates, dev-dependencies included (DESIGN 3.1).
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };

        for _ in 0..20_000 {
            let length = (next() % 24) as usize;
            let mut source = String::new();
            for _ in 0..length {
                let index = (next() as usize) % alphabet.len();
                source.push_str(alphabet[index]);
            }
            // Only that it returns: either verdict is fine, a panic is not.
            let _ = parse(&source);
        }
    }

    #[test]
    fn value_accessors_do_not_confuse_the_two_shapes() {
        let text = Value::Text("x".to_string());
        let list = Value::List(vec!["x".to_string()]);
        assert_eq!(text.as_text(), Some("x"));
        assert!(text.as_list().is_none());
        assert!(list.as_text().is_none());
        assert_eq!(list.as_list(), Some(["x".to_string()].as_slice()));
    }
}
