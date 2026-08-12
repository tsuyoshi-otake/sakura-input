//! Bunsetsu-boundary table generation from the pinned Mozc segmenter rules.
//!
//! Mozc decides segment (bunsetsu) boundaries with an ordered rule list over
//! POS feature strings: `src/data/rules/segmenter.def` interpreted by
//! `gen_segmenter_code.py`.  The left pattern is matched against the feature
//! of the previous word's right connection class, the right pattern against
//! the feature of the following word's left class; the first matching rule
//! wins, BOS/EOS (class 0) is always a boundary, and an unmatched pair
//! defaults to a boundary.  Patterns are Python `re.match` prefix regexes
//! after every `*` is replaced with `[^,]+`.
//!
//! This module reproduces those semantics with a hand-written matcher for the
//! pattern subset the pinned rules actually use (literals, `*` fields,
//! `(a|b)` literal alternations, a leading `^`, and a trailing `$`), then
//! freezes the verdicts into a `class × class` bit table so the runtime
//! never parses rules.  Any construct outside that subset is a build error,
//! never a silent skip.

use crate::Error;

/// A frozen `class_count × class_count` boundary matrix.  Bit `(rid, lid)`
/// set means a bunsetsu boundary separates a word ending with connection
/// class `rid` from a following word starting with class `lid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BunsetsuBoundaries {
    class_count: u16,
    /// Row-major with byte-aligned rows of `ceil(class_count / 8)` bytes;
    /// padding bits at each row tail stay zero.
    rows: Vec<u8>,
}

impl BunsetsuBoundaries {
    pub const fn class_count(&self) -> u16 {
        self.class_count
    }

    pub(crate) fn rows(&self) -> &[u8] {
        &self.rows
    }

    pub fn is_boundary(&self, right_id: u16, left_id: u16) -> bool {
        let classes = usize::from(self.class_count);
        let (rid, lid) = (usize::from(right_id), usize::from(left_id));
        if rid >= classes || lid >= classes {
            return true;
        }
        let row_bytes = classes.div_ceil(8);
        self.rows[rid * row_bytes + lid / 8] & (1u8 << (lid % 8)) != 0
    }
}

/// One parsed `left right boundary` rule line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmenterRule {
    left: Pattern,
    right: Pattern,
    boundary: bool,
    line: usize,
}

/// Parses the Mozc `id.def` taxonomy into feature strings indexed by class.
///
/// The file must enumerate ids densely from zero in order; anything else is
/// a wrong or truncated pin, not something to repair silently.
pub fn parse_mozc_pos_features(source: &str, text: &str) -> Result<Vec<String>, Error> {
    let mut features = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(id), Some(feature), None) = (fields.next(), fields.next(), fields.next()) else {
            return Err(Error::at(
                source,
                line_number,
                "expected exactly 'id feature' fields",
            ));
        };
        let id: usize = id
            .parse()
            .map_err(|_| Error::at(source, line_number, "connection class id is not a number"))?;
        if id != features.len() {
            return Err(Error::at(
                source,
                line_number,
                format!("expected dense class id {}, found {id}", features.len()),
            ));
        }
        features.push(feature.to_string());
    }
    if features.is_empty() {
        return Err(Error::at(source, 1, "no connection classes"));
    }
    if u16::try_from(features.len()).is_err() {
        return Err(Error::at(source, 1, "more than 65,535 connection classes"));
    }
    Ok(features)
}

/// Parses `segmenter.def` rule lines, rejecting unsupported pattern syntax.
pub fn parse_mozc_segmenter_rules(source: &str, text: &str) -> Result<Vec<SegmenterRule>, Error> {
    let mut rules = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(left), Some(right), Some(result), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::at(
                source,
                line_number,
                "expected exactly 'left-pattern right-pattern boundary' fields",
            ));
        };
        let boundary = match result.to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(Error::at(
                    source,
                    line_number,
                    "boundary verdict must be true or false",
                ))
            }
        };
        rules.push(SegmenterRule {
            left: parse_pattern(source, line_number, left)?,
            right: parse_pattern(source, line_number, right)?,
            boundary,
            line: line_number,
        });
    }
    if rules.is_empty() {
        return Err(Error::at(source, 1, "no segmenter rules"));
    }
    Ok(rules)
}

/// Evaluates every rule over every class pair and freezes the verdicts.
///
/// Mirrors `gen_segmenter_code.py`: a non-`*` pattern matching no feature at
/// all is a fatal inconsistency between the pinned rule file and taxonomy.
pub fn build_boundaries(
    rules_source: &str,
    features: &[String],
    rules: &[SegmenterRule],
) -> Result<BunsetsuBoundaries, Error> {
    let class_count = u16::try_from(features.len())
        .map_err(|_| Error::build("more than 65,535 connection classes"))?;
    let classes = features.len();
    let words = classes.div_ceil(64);

    struct CompiledRule {
        left: Vec<u64>,
        right: Vec<u64>,
        boundary: bool,
    }
    let match_set = |pattern: &Pattern, side: &str, line: usize| -> Result<Vec<u64>, Error> {
        let mut set = vec![0u64; words];
        let mut any = false;
        for (id, feature) in features.iter().enumerate() {
            if pattern.matches(feature) {
                set[id / 64] |= 1u64 << (id % 64);
                any = true;
            }
        }
        if !any {
            return Err(Error::at(
                rules_source,
                line,
                format!("{side} pattern matches no connection class"),
            ));
        }
        Ok(set)
    };
    let compiled = rules
        .iter()
        .map(|rule| {
            Ok(CompiledRule {
                left: match_set(&rule.left, "left", rule.line)?,
                right: match_set(&rule.right, "right", rule.line)?,
                boundary: rule.boundary,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let row_bytes = classes.div_ceil(8);
    let mut rows = vec![0u8; classes * row_bytes];
    let mut fused = vec![0u64; words];
    for rid in 0..classes {
        // Default verdict and both BOS/EOS sides are boundaries; only cells
        // an explicit `false` rule wins may fuse.
        fused.iter_mut().for_each(|word| *word = 0);
        if rid != 0 {
            let mut undecided = vec![!0u64; words];
            undecided[0] &= !1u64; // lid 0 is always a boundary
            if classes % 64 != 0 {
                undecided[words - 1] &= (1u64 << (classes % 64)) - 1;
            }
            for rule in &compiled {
                if rule.left[rid / 64] & (1u64 << (rid % 64)) == 0 {
                    continue;
                }
                let mut open = false;
                for word in 0..words {
                    let taken = rule.right[word] & undecided[word];
                    undecided[word] &= !taken;
                    if !rule.boundary {
                        fused[word] |= taken;
                    }
                    open |= undecided[word] != 0;
                }
                if !open {
                    break;
                }
            }
        }
        let row = &mut rows[rid * row_bytes..(rid + 1) * row_bytes];
        for lid in 0..classes {
            if fused[lid / 64] & (1u64 << (lid % 64)) == 0 {
                row[lid / 8] |= 1u8 << (lid % 8);
            }
        }
    }
    Ok(BunsetsuBoundaries { class_count, rows })
}

/// One anchored-prefix pattern from a rule line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    tokens: Vec<Token>,
    /// A trailing `$`: the whole feature must be consumed, not just a prefix.
    anchored_end: bool,
    /// The pattern was the single character `*`, which matches every class.
    match_all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(String),
    /// One `*` field: `[^,]+` after Mozc's pattern-to-regex rewrite.
    AnyField,
    /// `(a|b|c)` with literal alternatives only.
    Alternation(Vec<String>),
}

fn parse_pattern(source: &str, line: usize, pattern: &str) -> Result<Pattern, Error> {
    if pattern == "*" {
        return Ok(Pattern {
            tokens: Vec::new(),
            anchored_end: false,
            match_all: true,
        });
    }
    let unsupported = |what: &str| {
        Error::at(
            source,
            line,
            format!("unsupported segmenter pattern construct {what} in '{pattern}'"),
        )
    };
    // `re.match` is already anchored at the start, so a leading `^` is inert.
    let mut rest = pattern.strip_prefix('^').unwrap_or(pattern);
    let anchored_end = if let Some(stripped) = rest.strip_suffix('$') {
        rest = stripped;
        true
    } else {
        false
    };

    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut characters = rest.chars();
    while let Some(character) = characters.next() {
        match character {
            '*' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(core::mem::take(&mut literal)));
                }
                tokens.push(Token::AnyField);
            }
            '(' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(core::mem::take(&mut literal)));
                }
                let mut alternatives = Vec::new();
                let mut alternative = String::new();
                loop {
                    match characters.next() {
                        Some(')') => break,
                        Some('|') => alternatives.push(core::mem::take(&mut alternative)),
                        Some(
                            inner @ ('(' | '*' | '[' | ']' | '.' | '+' | '?' | '{' | '}' | '\\'
                            | '^' | '$'),
                        ) => return Err(unsupported(&format!("'{inner}' inside a group"))),
                        Some(inner) => alternative.push(inner),
                        None => return Err(unsupported("an unterminated group")),
                    }
                }
                alternatives.push(alternative);
                if alternatives.iter().any(String::is_empty) {
                    return Err(unsupported("an empty group alternative"));
                }
                tokens.push(Token::Alternation(alternatives));
            }
            ')' | '|' | '[' | ']' | '.' | '+' | '?' | '{' | '}' | '\\' | '^' | '$' => {
                return Err(unsupported(&format!("'{character}'")))
            }
            _ => literal.push(character),
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    if tokens.is_empty() {
        return Err(unsupported("an empty pattern"));
    }
    Ok(Pattern {
        tokens,
        anchored_end,
        match_all: false,
    })
}

impl Pattern {
    fn matches(&self, feature: &str) -> bool {
        self.match_all || match_tokens(&self.tokens, feature, self.anchored_end)
    }
}

/// Backtracking prefix match with Python `re.match` semantics over the
/// supported token subset.  Rule patterns are a handful of tokens long, so
/// recursion depth stays trivially small.
fn match_tokens(tokens: &[Token], rest: &str, anchored_end: bool) -> bool {
    let Some((first, tail)) = tokens.split_first() else {
        return !anchored_end || rest.is_empty();
    };
    match first {
        Token::Literal(text) => rest
            .strip_prefix(text.as_str())
            .is_some_and(|rest| match_tokens(tail, rest, anchored_end)),
        Token::AnyField => {
            let field = &rest[..rest.find(',').unwrap_or(rest.len())];
            field.char_indices().any(|(at, character)| {
                match_tokens(tail, &rest[at + character.len_utf8()..], anchored_end)
            })
        }
        Token::Alternation(alternatives) => alternatives.iter().any(|alternative| {
            rest.strip_prefix(alternative.as_str())
                .is_some_and(|rest| match_tokens(tail, rest, anchored_end))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(text: &str) -> Pattern {
        parse_pattern("segmenter.def", 1, text).expect("pattern")
    }

    #[test]
    fn patterns_follow_python_re_match_semantics() {
        // Anchored prefix, not substring and not full-field.
        assert!(pattern("名詞,数").matches("名詞,数,アラビア数字,*,*,*,*"));
        assert!(pattern("名詞,数").matches("名詞,数字っぽい何か"));
        assert!(!pattern("名詞,数").matches("接頭詞,名詞,数"));
        // `*` fields must consume at least one non-comma character each.
        assert!(pattern("動詞,非自立,*,*,五段・カ行促音便,連用タ接続,く")
            .matches("動詞,非自立,*,*,五段・カ行促音便,連用タ接続,くる"));
        assert!(!pattern("動詞,非自立,*,*,五段・カ行促音便,連用タ接続,く")
            .matches("動詞,非自立,*,五段・カ行促音便,連用タ接続,く"));
        // Alternations try every literal alternative.
        assert!(pattern("記号,(句点|読点|括弧開|括弧閉)").matches("記号,読点,*,*,*,*,*"));
        assert!(!pattern("記号,(句点|読点)").matches("記号,空白,*,*,*,*,*"));
        // A trailing `$` demands the whole feature.
        assert!(pattern("^助詞,*,*,*,*,*,(ヲ|ニ)$").matches("助詞,格助詞,一般,*,*,*,ニ"));
        assert!(!pattern("^助詞,*,*,*,*,*,(ヲ|ニ)$").matches("助詞,格助詞,一般,*,*,*,ニャ"));
        // A lone `*` matches everything, including BOS/EOS.
        assert!(pattern("*").matches("BOS/EOS,*,*,*,*,*,*"));
    }

    #[test]
    fn unsupported_pattern_syntax_is_a_build_error() {
        for bad in [
            "名詞.",
            "名詞+",
            "((あ|い))",
            "名詞\\d",
            "(あ|)",
            "名詞[ぁ-ん]",
        ] {
            assert!(
                parse_pattern("segmenter.def", 1, bad).is_err(),
                "pattern '{bad}' must be rejected"
            );
        }
    }

    #[test]
    fn boundary_table_applies_first_matching_rule_with_default_true() {
        let features: Vec<String> = [
            "BOS/EOS,*,*,*,*,*,*",
            "動詞,自立,*,*,サ変・スル,連用形,する",
            "助動詞,特殊・タ,*,*,基本形,*,た",
            "名詞,一般,*,*,*,*,*",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let rules = parse_mozc_segmenter_rules(
            "segmenter.def",
            "# comment\n\
             * ^名詞,一般 true\n\
             * ^(助詞|助動詞) false\n\
             * * true\n",
        )
        .expect("rules");
        let table = build_boundaries("segmenter.def", &features, &rules).expect("table");

        // 動詞|助動詞 fuses; 動詞|名詞 hits the explicit true rule first.
        assert!(!table.is_boundary(1, 2));
        assert!(table.is_boundary(1, 3));
        assert!(table.is_boundary(3, 3));
        // BOS/EOS is always a boundary even though `* ^(助詞|助動詞)` matches.
        for id in 0..4 {
            assert!(table.is_boundary(0, id));
            assert!(table.is_boundary(id, 0));
        }
        // Out-of-range classes fail closed to a boundary.
        assert!(table.is_boundary(4, 1));
    }

    #[test]
    fn rule_matching_nothing_is_rejected() {
        let features = vec!["BOS/EOS,*,*,*,*,*,*".to_string(), "名詞,一般".to_string()];
        let rules = parse_mozc_segmenter_rules("segmenter.def", "感動詞 * false\n* * true\n")
            .expect("rules");
        assert!(build_boundaries("segmenter.def", &features, &rules).is_err());
    }

    #[test]
    fn dense_id_parse_rejects_gaps() {
        assert!(parse_mozc_pos_features("id.def", "0 BOS/EOS\n1 名詞,一般\n").is_ok());
        assert!(parse_mozc_pos_features("id.def", "0 BOS/EOS\n2 名詞,一般\n").is_err());
        assert!(parse_mozc_pos_features("id.def", "0 BOS/EOS extra\n").is_err());
        assert!(parse_mozc_pos_features("id.def", "").is_err());
    }
}
