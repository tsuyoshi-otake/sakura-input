//! Pinned `BertJapaneseTokenizer` basic + character tokenization.
//!
//! The exported model config fixes `do_lower_case=false`, `word_tokenizer_type`
//! to `basic`, and `subword_tokenizer_type` to `character`.  In particular,
//! this deliberately does not apply NFKC or accent stripping: neither is part
//! of Transformers' `BasicTokenizer` path for that configuration.

use std::{collections::HashMap, fmt};

use unicode_general_category::{get_general_category, GeneralCategory};

pub const MAX_TOKEN_COUNT: usize = 128;
const SPECIAL_TOKENS: [&str; 5] = ["[PAD]", "[CLS]", "[SEP]", "[UNK]", "[MASK]"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    EmptyVocabulary,
    MissingSpecialToken,
    EmptyInput,
    InvalidTokenLimit,
    InputTooLong,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyVocabulary => "vocabulary is empty",
            Self::MissingSpecialToken => "vocabulary is missing a required special token",
            Self::EmptyInput => "tokenization input is empty after basic cleanup",
            Self::InvalidTokenLimit => "tokenization token limit is invalid",
            Self::InputTooLong => "tokenization input exceeds the token limit",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub struct Tokenizer {
    ids: HashMap<String, i64>,
    cls: i64,
    sep: i64,
    unk: i64,
    mask: i64,
    pad: i64,
}

impl Tokenizer {
    /// Loads a UTF-8 (optionally UTF-8 BOM-prefixed) `vocab.txt`.
    pub fn from_vocab(vocabulary: &str) -> Result<Self, Error> {
        let vocabulary = vocabulary.strip_prefix('\u{feff}').unwrap_or(vocabulary);
        if vocabulary.is_empty() {
            return Err(Error::EmptyVocabulary);
        }

        let mut ids = HashMap::new();
        for (index, token) in vocabulary.lines().enumerate() {
            // Match Transformers' load_vocab exactly: duplicate token text is
            // legal and the final occurrence supplies its model embedding ID.
            ids.insert(token.to_owned(), index as i64);
        }
        if ids.is_empty() {
            return Err(Error::EmptyVocabulary);
        }

        let id = |token| ids.get(token).copied().ok_or(Error::MissingSpecialToken);
        Ok(Self {
            pad: id("[PAD]")?,
            cls: id("[CLS]")?,
            sep: id("[SEP]")?,
            unk: id("[UNK]")?,
            mask: id("[MASK]")?,
            ids,
        })
    }

    /// Adds `[CLS]` and `[SEP]` around character-subword token IDs.
    ///
    /// The caller supplies the model sequence limit.  Both the worker-wide
    /// bound and that supplied limit are checked before allocating unbounded
    /// token vectors.
    pub fn encode(&self, text: &str, max_tokens: usize) -> Result<Vec<i64>, Error> {
        if !(2..=MAX_TOKEN_COUNT).contains(&max_tokens) {
            return Err(Error::InvalidTokenLimit);
        }

        let basic_tokens = basic_tokens(text);
        if basic_tokens.is_empty() {
            return Err(Error::EmptyInput);
        }

        let mut encoded = Vec::with_capacity(max_tokens);
        encoded.push(self.cls);
        for token in basic_tokens {
            if is_special(&token) {
                push_bounded(&mut encoded, self.id_for(&token), max_tokens)?;
            } else {
                for scalar in token.chars() {
                    let id = self.id_for_scalar(scalar);
                    push_bounded(&mut encoded, id, max_tokens)?;
                }
            }
        }
        encoded.push(self.sep);
        Ok(encoded)
    }

    pub fn mask(&self) -> i64 {
        self.mask
    }

    pub fn pad(&self) -> i64 {
        self.pad
    }

    pub fn unk(&self) -> i64 {
        self.unk
    }

    fn id_for(&self, token: &str) -> i64 {
        // Special-token presence was verified when the vocabulary was loaded.
        *self.ids.get(token).unwrap_or(&self.unk)
    }

    fn id_for_scalar(&self, scalar: char) -> i64 {
        self.id_for(&scalar.to_string())
    }
}

/// Runs a no-model fixture through the BasicTokenizer and character lookup.
/// This is intentionally available to the worker's `--self-test` mode, so
/// startup validation exercises the same non-ORT path used before inference.
pub fn self_test() -> Result<(), &'static str> {
    let tokenizer = Tokenizer::from_vocab("[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\n日\n本\nA\n!\n")
        .map_err(|_| "tokenizer self-test vocabulary rejected")?;
    if tokenizer.encode("日[MASK]本 A!", 16).ok() != Some(vec![1, 5, 4, 6, 7, 8, 2])
        || tokenizer.pad() != 0
        || tokenizer.mask() != 4
    {
        return Err("tokenizer self-test parity failure");
    }
    Ok(())
}

fn push_bounded(tokens: &mut Vec<i64>, id: i64, max_tokens: usize) -> Result<(), Error> {
    // Reserve room for the final `[SEP]`.
    if tokens.len() >= max_tokens - 1 {
        return Err(Error::InputTooLong);
    }
    tokens.push(id);
    Ok(())
}

fn basic_tokens(text: &str) -> Vec<String> {
    let cleaned = clean_text(text);
    let mut tokens = Vec::new();
    let mut remainder = cleaned.as_str();

    // PreTrainedTokenizer separates registered special tokens before invoking
    // BasicTokenizer.  Do the same explicitly so `[MASK]` remains one token
    // even when adjacent to CJK text or punctuation.
    while let Some((offset, special)) = first_special(remainder) {
        basic_segment(&remainder[..offset], &mut tokens);
        tokens.push(special.to_owned());
        remainder = &remainder[offset + special.len()..];
    }
    basic_segment(remainder, &mut tokens);
    tokens
}

fn first_special(text: &str) -> Option<(usize, &'static str)> {
    SPECIAL_TOKENS
        .iter()
        .filter_map(|special| text.find(special).map(|offset| (offset, *special)))
        .min_by_key(|(offset, _)| *offset)
}

fn basic_segment(segment: &str, output: &mut Vec<String>) {
    let mut cjk_spaced = String::with_capacity(segment.len());
    for scalar in segment.chars() {
        if is_cjk(scalar) {
            cjk_spaced.push(' ');
            cjk_spaced.push(scalar);
            cjk_spaced.push(' ');
        } else {
            cjk_spaced.push(scalar);
        }
    }

    for word in cjk_spaced.split_whitespace() {
        let mut current = String::new();
        for scalar in word.chars() {
            if is_punctuation(scalar) {
                if !current.is_empty() {
                    output.push(std::mem::take(&mut current));
                }
                output.push(scalar.to_string());
            } else {
                current.push(scalar);
            }
        }
        if !current.is_empty() {
            output.push(current);
        }
    }
}

fn clean_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    for scalar in text.chars() {
        // This order matches Transformers: tab, LF, and CR are whitespace,
        // not removable controls.
        if is_whitespace(scalar) {
            cleaned.push(' ');
        } else if scalar == '\0' || scalar == '\u{fffd}' || is_control(scalar) {
            continue;
        } else {
            cleaned.push(scalar);
        }
    }
    cleaned
}

fn is_whitespace(scalar: char) -> bool {
    matches!(scalar, ' ' | '\t' | '\n' | '\r')
        || get_general_category(scalar) == GeneralCategory::SpaceSeparator
}

fn is_control(scalar: char) -> bool {
    matches!(
        get_general_category(scalar),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::PrivateUse
            | GeneralCategory::Surrogate
            | GeneralCategory::Unassigned
    )
}

fn is_punctuation(scalar: char) -> bool {
    let codepoint = scalar as u32;
    if (33..=47).contains(&codepoint)
        || (58..=64).contains(&codepoint)
        || (91..=96).contains(&codepoint)
        || (123..=126).contains(&codepoint)
    {
        return true;
    }
    matches!(
        get_general_category(scalar),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::OpenPunctuation
    )
}

fn is_cjk(scalar: char) -> bool {
    matches!(
        scalar as u32,
        0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xf900..=0xfaff
            | 0x20000..=0x2a6df
            | 0x2a700..=0x2b73f
            | 0x2b740..=0x2b81f
            | 0x2b820..=0x2ceaf
            | 0x2f800..=0x2fa1f
    )
}

fn is_special(token: &str) -> bool {
    SPECIAL_TOKENS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenizer() -> Tokenizer {
        // IDs are intentionally explicit so each fixture checks the exact
        // character-mode IDs the exported `vocab.txt` contract supplies.
        Tokenizer::from_vocab(
            "\u{feff}[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\n日\n本\n語\nA\na\n!\n、\né\n😀\n",
        )
        .unwrap()
    }

    #[test]
    fn japanese_ascii_and_mixed_exact_ids() {
        assert_eq!(
            tokenizer().encode("日本 A語", 16).unwrap(),
            vec![1, 5, 6, 8, 7, 2]
        );
    }

    #[test]
    fn whitespace_controls_and_cjk_spacing_are_basic_tokenizer_compatible() {
        assert_eq!(
            tokenizer().encode("日\t\0本\u{200e}\n語", 16).unwrap(),
            vec![1, 5, 6, 7, 2]
        );
    }

    #[test]
    fn punctuation_splits_without_changing_character_ids() {
        assert_eq!(
            tokenizer().encode("A!、", 16).unwrap(),
            vec![1, 8, 10, 11, 2]
        );
    }

    #[test]
    fn case_and_accents_are_not_normalized() {
        assert_eq!(tokenizer().encode("Aé", 16).unwrap(), vec![1, 8, 12, 2]);
        assert_eq!(tokenizer().encode("aÉ", 16).unwrap(), vec![1, 9, 3, 2]);
    }

    #[test]
    fn unknown_unicode_scalar_uses_unk_id() {
        assert_eq!(tokenizer().encode("🌸", 16).unwrap(), vec![1, 3, 2]);
    }

    #[test]
    fn special_tokens_are_never_split() {
        assert_eq!(
            tokenizer().encode("日[MASK]本", 16).unwrap(),
            vec![1, 5, 4, 6, 2]
        );
        assert_eq!(tokenizer().mask(), 4);
        assert_eq!(tokenizer().pad(), 0);
    }

    #[test]
    fn empty_and_oversize_input_are_explicit_errors() {
        assert_eq!(
            tokenizer().encode(" \0\u{fffd}", 16),
            Err(Error::EmptyInput)
        );
        assert_eq!(tokenizer().encode("日本語", 4), Err(Error::InputTooLong));
        assert_eq!(
            tokenizer().encode("日", MAX_TOKEN_COUNT + 1),
            Err(Error::InvalidTokenLimit)
        );
    }

    #[test]
    fn vocabulary_contract_uses_last_duplicate_id_and_rejects_missing_specials() {
        let duplicated =
            Tokenizer::from_vocab("[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\nA\n[PAD]\n").unwrap();
        assert_eq!(duplicated.pad(), 6);
        assert_eq!(duplicated.encode("A", 8).unwrap(), vec![1, 5, 2]);
        assert_eq!(
            Tokenizer::from_vocab("[CLS]\n").unwrap_err(),
            Error::MissingSpecialToken
        );
    }

    #[test]
    fn model_free_self_test_passes() {
        self_test().unwrap();
    }

    #[test]
    fn generated_text_respects_sequence_and_special_token_invariants() {
        let tokenizer =
            Tokenizer::from_vocab("[PAD]\n[CLS]\n[SEP]\n[UNK]\n[MASK]\nA\nB\nC\n!\n日\n").unwrap();
        let alphabet = ['A', 'B', 'C', '!', '日', ' ', '\t', '\0', '\u{200e}'];
        let mut state = 0xa076_1d64_78bd_642fu64;
        for case in 0..1024usize {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let scalar_count = case % 150;
            let mut text = String::new();
            for _ in 0..scalar_count {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                text.push(alphabet[state as usize % alphabet.len()]);
            }
            match tokenizer.encode(&text, MAX_TOKEN_COUNT) {
                Ok(tokens) => {
                    assert!((3..=MAX_TOKEN_COUNT).contains(&tokens.len()));
                    assert_eq!(tokens.first(), Some(&1));
                    assert_eq!(tokens.last(), Some(&2));
                    assert!(tokens.iter().all(|id| (0..=9).contains(id)));
                }
                Err(error) => assert!(matches!(error, Error::EmptyInput | Error::InputTooLong)),
            }
        }
    }
}
