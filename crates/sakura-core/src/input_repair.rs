//! Bounded ATOK-style reading repair for conversion and prediction.
//!
//! The typed reading stays on screen. Repair only invents alternate dictionary
//! lookup prefixes that map back onto the typed byte span, so Viterbi still
//! covers the original reading while surfaces come from the corrected word.
//!
//! Every generator is finite: a fixed scratch of variants, a character-edit
//! radius of one, and no whole-dictionary fuzzy scan.

use sakura_proto::{FixedStr, FixedVec, MAX_PREEDIT_BYTES};

use crate::dictionary::EntryFlags;
use crate::preferences::InputSupport;

/// Soft cost added when a lattice edge used a repaired reading prefix.
pub const REPAIR_PENALTY: i64 = 1_200;
/// Higher penalty for the broad advanced edit-1 pass.
pub const ADVANCED_REPAIR_PENALTY: i64 = 2_400;
/// Soft cost for English-spelling → katakana loanword recovery.
pub const ENGLISH_KATAKANA_PENALTY: i64 = 1_800;
/// Soft cost for a commit-history repair hint. Lower than rule repair so a
/// previously accepted correction outranks an unconfirmed rule variant.
pub const COMMIT_HISTORY_PENALTY: i64 = 600;

/// Maximum alternate prefixes retained for one typed reading.
pub const MAX_REPAIR_VARIANTS: usize = 24;
/// Prediction keeps a smaller variant budget so the 10 ms worker stays light.
pub const MAX_PREDICTION_REPAIR_VARIANTS: usize = 8;
/// Advanced edit-1 only runs on short prefixes.
const ADVANCED_MAX_CHARS: usize = 6;

/// Shared dictionary-entry admission for conversion lattices and prediction.
///
/// SPELLING_CORRECTION entries are rejected before they consume lattice or
/// ranking budget unless the unified gate allows them (Issue #63).
pub const fn allows_system_entry(
    support: InputSupport,
    skip_input_repair: bool,
    flags: EntryFlags,
) -> bool {
    !flags.contains(EntryFlags::SPELLING_CORRECTION)
        || support.allows_spelling_correction(skip_input_repair)
}

/// Bounded list of repair variants. Uses owned `FixedStr` values, so it cannot
/// live inside [`FixedVec`] (which requires `Copy`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairVariantList {
    items: Vec<RepairVariant>,
}

impl RepairVariantList {
    pub fn new() -> Self {
        Self {
            items: Vec::with_capacity(MAX_REPAIR_VARIANTS),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &RepairVariant> {
        self.items.iter()
    }

    pub fn as_slice(&self) -> &[RepairVariant] {
        &self.items
    }
}

/// One alternate reading that covers `typed_end` bytes of the typed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairVariant {
    pub repaired: FixedStr<MAX_PREEDIT_BYTES>,
    pub typed_end: u16,
    pub penalty: i64,
    pub kind: RepairKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairKind {
    Rule,
    Advanced,
    EnglishSpelling,
    CommitHistory,
}

/// Collects rule-based and advanced reading variants for `typed`.
///
/// Variants never include the original reading itself. Callers look the
/// repaired string up in the dictionary and attach edges whose `end` equals
/// `typed_end` so the lattice still covers the typed bytes.
pub fn collect_repair_variants(
    typed: &str,
    support: InputSupport,
    max_variants: usize,
) -> RepairVariantList {
    let mut out = RepairVariantList::new();
    if !support.is_active() || typed.is_empty() || max_variants == 0 {
        return out;
    }
    // Pure Latin/digit tokens are handled by english-to-katakana, not kana edit
    // rules. Open edit-1 on ASCII used to turn `llvm` into the dictionary hit
    // `lvm` and steal top-1 from the typed token.
    if !typed
        .chars()
        .any(|character| is_hiragana(character) || is_katakana(character))
    {
        return out;
    }
    // A decimal digit is an explicit literal token, not an uncertain kana.
    // In particular, Advanced substitution must never reinterpret `5かい`
    // as `あかい` (or any other digit/counter input as an unrelated word).
    if typed
        .chars()
        .any(|character| to_half_ascii(character).is_ascii_digit())
    {
        return out;
    }
    let limit = max_variants.min(MAX_REPAIR_VARIANTS);

    push_n_count_variants(typed, support, &mut out, limit);
    push_consonant_extra_variants(typed, support, &mut out, limit);
    push_vowel_variants(typed, support, &mut out, limit);
    push_kana_rule_variants(typed, support, &mut out, limit);
    push_fuzzy_proper_variants(typed, support, &mut out, limit);
    if support.advanced {
        push_advanced_variants(typed, &mut out, limit);
    }
    out
}

/// Rebuilds an ASCII spelling from a mixed kana/Latin reading such as
/// `あっｐぇ` or `いんてｒねｔ`, then produces a katakana reading for dictionary
/// lookup. Returns `None` when the reading has no Latin letters or the
/// reconstruction fails.
pub fn english_spelling_katakana_reading(typed: &str) -> Option<FixedStr<MAX_PREEDIT_BYTES>> {
    if !typed.chars().any(is_latin_letter) {
        return None;
    }
    let mut ascii = FixedStr::<MAX_PREEDIT_BYTES>::new();
    let chars: Vec<char> = typed.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character == 'っ' || character == 'ッ' {
            // Sokuon doubles the next Latin consonant when present.
            if let Some(next) = chars.get(index + 1).copied() {
                let half = to_half_ascii(next).to_ascii_lowercase();
                if half.is_ascii_alphabetic() && !matches!(half, 'a' | 'i' | 'u' | 'e' | 'o' | 'n')
                {
                    ascii.push(half).ok()?;
                    index += 1;
                    continue;
                }
            }
            ascii.push_str("tsu").ok()?;
            index += 1;
            continue;
        }
        if is_latin_letter(character) {
            let half = to_half_ascii(character).to_ascii_lowercase();
            ascii.push(half).ok()?;
            index += 1;
            continue;
        }
        if let Some(romaji) = kana_to_romaji(character) {
            ascii.push_str(romaji).ok()?;
            index += 1;
            continue;
        }
        if !character.is_whitespace() {
            return None;
        }
        index += 1;
    }
    if ascii.is_empty() || !ascii.as_str().chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let romaji = english_ascii_to_kana_romaji(ascii.as_str())?;
    romaji_to_katakana(romaji.as_str())
}

/// Turns an English orthographic spelling into a romaji skeleton that
/// `romaji_to_katakana` can consume (apple → appuru, internet → intaanetto).
fn english_ascii_to_kana_romaji(ascii: &str) -> Option<String> {
    let lower = ascii.to_ascii_lowercase();
    if lower.is_empty() || !lower.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
        return None;
    }
    // Lightweight orthography normalizations used by Japanese loanword readings.
    // `er`/`ar` become a long vowel mark rather than a second `a` mora.
    let normalized = lower
        .replace("er", "a-")
        .replace("ar", "a-")
        .replace("or", "o-");
    let chars: Vec<char> = normalized.chars().collect();
    let mut end = chars.len();
    if end >= 2
        && chars[end - 1] == 'e'
        && is_english_consonant(chars[end - 2])
        && chars[end - 2] != 'e'
    {
        end -= 1;
    }
    let mut out = String::new();
    let mut index = 0usize;
    while index < end {
        let character = chars[index];
        if character == '-' {
            out.push('-');
            index += 1;
            continue;
        }
        if is_english_vowel(character) {
            out.push(character);
            index += 1;
            continue;
        }
        if character == 'n'
            && index + 1 < end
            && !is_english_vowel(chars[index + 1])
            && chars[index + 1] != 'n'
            && chars[index + 1] != 'y'
        {
            out.push('n');
            index += 1;
            continue;
        }
        if index + 1 < end && chars[index + 1] == character && is_english_consonant(character) {
            out.push(map_english_consonant(character));
            out.push(map_english_consonant(character));
            out.push('u');
            index += 2;
            continue;
        }
        if index + 1 < end && is_english_vowel(chars[index + 1]) {
            out.push(map_english_consonant(character));
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        // Final / bare consonant: stopped consonants close as ッCォ (net →
        // netto), while other consonants take a plain Cu (apple → ru).
        if index + 1 == end && matches!(character, 't' | 'k' | 'p' | 'c') {
            let body = map_english_consonant(character);
            out.push(body);
            out.push(body);
            out.push('o');
        } else {
            out.push(map_english_consonant(character));
            out.push('u');
        }
        index += 1;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn is_english_vowel(character: char) -> bool {
    matches!(character, 'a' | 'i' | 'u' | 'e' | 'o')
}

fn is_english_consonant(character: char) -> bool {
    character.is_ascii_alphabetic() && !is_english_vowel(character)
}

fn map_english_consonant(character: char) -> char {
    match character {
        'l' => 'r',
        'c' => 'k',
        other => other,
    }
}

/// Contextual replacement for one keystroke after `previous`, following ATOK's
/// digit/alnum punctuation and long-vowel rules. Returns `None` when the
/// character should keep its ordinary width/punctuation normalization.
pub fn contextual_punctuation_swap(
    previous: Option<char>,
    typed: char,
    support: InputSupport,
) -> Option<char> {
    if !support.is_active() {
        return None;
    }
    let previous = previous?;
    let half_prev = to_half_ascii(previous);
    if support.period_after_digit && matches!(typed, '。' | '．') && half_prev.is_ascii_digit() {
        return Some('．');
    }
    if support.comma_after_digit && matches!(typed, '、' | '，') && half_prev.is_ascii_digit() {
        return Some('，');
    }
    if support.middle_dot_after_digit && matches!(typed, '・' | '／') && half_prev.is_ascii_digit()
    {
        return Some('／');
    }
    if support.long_vowel_after_alnum
        && matches!(typed, 'ー' | '－' | '-')
        && (half_prev.is_ascii_alphanumeric()
            || matches!(half_prev, '!'..='/' | ':'..='@' | '['..='`' | '{'..='~'))
    {
        return Some('－');
    }
    None
}

fn push_unique(
    out: &mut RepairVariantList,
    typed: &str,
    repaired: &str,
    typed_end: usize,
    penalty: i64,
    kind: RepairKind,
    limit: usize,
) {
    if out.len() >= limit || repaired.is_empty() || repaired == typed {
        return;
    }
    if out.iter().any(|existing| {
        existing.repaired.as_str() == repaired && usize::from(existing.typed_end) == typed_end
    }) {
        return;
    }
    let Ok(typed_end) = u16::try_from(typed_end) else {
        return;
    };
    let mut text = FixedStr::new();
    if text.push_str(repaired).is_err() {
        return;
    }
    out.items.push(RepairVariant {
        repaired: text,
        typed_end,
        penalty,
        kind,
    });
}

fn push_n_count_variants(
    typed: &str,
    support: InputSupport,
    out: &mut RepairVariantList,
    limit: usize,
) {
    if !support.n_count {
        return;
    }
    // Delete one duplicated ん.
    let chars: Vec<char> = typed.chars().collect();
    for index in 0..chars.len().saturating_sub(1) {
        if chars[index] == 'ん' && chars[index + 1] == 'ん' {
            let mut repaired = String::new();
            for (i, character) in chars.iter().enumerate() {
                if i == index {
                    continue;
                }
                repaired.push(*character);
            }
            push_unique(
                out,
                typed,
                &repaired,
                typed.len(),
                REPAIR_PENALTY,
                RepairKind::Rule,
                limit,
            );
        }
    }
    // Insert one ん before a consonant-like mora when the typed reading looks
    // like a missing nasal (こにちは → こんにちは).
    for index in 1..chars.len() {
        if chars[index - 1] != 'ん'
            && is_hiragana(chars[index])
            && !is_vowel_kana(chars[index])
            && chars[index] != 'ん'
            && chars[index] != 'っ'
        {
            let mut repaired = String::new();
            for (i, character) in chars.iter().enumerate() {
                if i == index {
                    repaired.push('ん');
                }
                repaired.push(*character);
            }
            push_unique(
                out,
                typed,
                &repaired,
                typed.len(),
                REPAIR_PENALTY,
                RepairKind::Rule,
                limit,
            );
        }
    }
}

fn push_consonant_extra_variants(
    typed: &str,
    support: InputSupport,
    out: &mut RepairVariantList,
    limit: usize,
) {
    if !support.consonant_extra {
        return;
    }
    let chars: Vec<char> = typed.chars().collect();
    for (index, character) in chars.iter().enumerate() {
        if *character != 'っ' {
            continue;
        }
        let mut repaired = String::new();
        for (i, other) in chars.iter().enumerate() {
            if i == index {
                continue;
            }
            repaired.push(*other);
        }
        push_unique(
            out,
            typed,
            &repaired,
            typed.len(),
            REPAIR_PENALTY,
            RepairKind::Rule,
            limit,
        );
    }
}

fn push_vowel_variants(
    typed: &str,
    support: InputSupport,
    out: &mut RepairVariantList,
    limit: usize,
) {
    if !support.vowel_count {
        return;
    }
    let chars: Vec<char> = typed.chars().collect();
    // Delete one of two identical consecutive vowel kana (おお → お).
    // Distinct pairs such as えお are left alone: deleting the trailing mora
    // would otherwise let a prefix dictionary entry absorb the rest of the
    // reading as a false full-span repair.
    for index in 0..chars.len().saturating_sub(1) {
        if is_vowel_kana(chars[index]) && chars[index] == chars[index + 1] {
            for drop in [index, index + 1] {
                let mut repaired = String::new();
                for (i, character) in chars.iter().enumerate() {
                    if i == drop {
                        continue;
                    }
                    repaired.push(*character);
                }
                push_unique(
                    out,
                    typed,
                    &repaired,
                    typed.len(),
                    REPAIR_PENALTY,
                    RepairKind::Rule,
                    limit,
                );
            }
        }
    }
    // Insert a lengthening vowel after an open syllable that often needs one
    // (おはよ → おはよう).
    for index in 0..chars.len() {
        let Some(extra) = lengthening_vowel(chars[index]) else {
            continue;
        };
        if chars.get(index + 1) == Some(&extra) {
            continue;
        }
        let mut repaired = String::new();
        for (i, character) in chars.iter().enumerate() {
            repaired.push(*character);
            if i == index {
                repaired.push(extra);
            }
        }
        push_unique(
            out,
            typed,
            &repaired,
            typed.len(),
            REPAIR_PENALTY,
            RepairKind::Rule,
            limit,
        );
    }
}

fn push_kana_rule_variants(
    typed: &str,
    support: InputSupport,
    out: &mut RepairVariantList,
    limit: usize,
) {
    let chars: Vec<char> = typed.chars().collect();
    for (index, character) in chars.iter().enumerate() {
        let replacements = kana_replacements(*character, support);
        for replacement in replacements.as_slice().iter().copied() {
            let mut repaired = String::new();
            for (i, other) in chars.iter().enumerate() {
                if i == index {
                    repaired.push(replacement);
                } else {
                    repaired.push(*other);
                }
            }
            push_unique(
                out,
                typed,
                &repaired,
                typed.len(),
                REPAIR_PENALTY,
                RepairKind::Rule,
                limit,
            );
        }
    }
}

fn kana_replacements(character: char, support: InputSupport) -> FixedVec<char, 4> {
    let mut out = FixedVec::new();
    if support.dakuten_swap {
        if let Some(swapped) = swap_dakuten(character) {
            let _ = out.push(swapped);
        }
    }
    if support.tsu_sokuon && character == 'つ' {
        let _ = out.push('っ');
    }
    if support.wa_wo && character == 'わ' {
        let _ = out.push('を');
    }
    if support.small_u && character == 'ぅ' {
        let _ = out.push('う');
    }
    out
}

fn push_fuzzy_proper_variants(
    typed: &str,
    support: InputSupport,
    out: &mut RepairVariantList,
    limit: usize,
) {
    if !support.fuzzy_proper_nouns {
        return;
    }
    let pairs = [
        ('お', 'う'),
        ('う', 'お'),
        ('ず', 'づ'),
        ('づ', 'ず'),
        ('じ', 'ぢ'),
        ('ぢ', 'じ'),
    ];
    let chars: Vec<char> = typed.chars().collect();
    for (index, character) in chars.iter().enumerate() {
        for (from, to) in pairs {
            if *character != from {
                continue;
            }
            let mut repaired = String::new();
            for (i, other) in chars.iter().enumerate() {
                if i == index {
                    repaired.push(to);
                } else {
                    repaired.push(*other);
                }
            }
            push_unique(
                out,
                typed,
                &repaired,
                typed.len(),
                REPAIR_PENALTY,
                RepairKind::Rule,
                limit,
            );
        }
    }
}

fn push_advanced_variants(typed: &str, out: &mut RepairVariantList, limit: usize) {
    let chars: Vec<char> = typed.chars().collect();
    if chars.len() > ADVANCED_MAX_CHARS || chars.is_empty() {
        return;
    }
    // Single-character deletions are limited to marks that are common typing
    // slips. Open deletion of arbitrary kana would let a prefix dictionary
    // entry absorb a trailing mora and surface as a false full-span candidate.
    for index in 0..chars.len() {
        if !is_advanced_deletable(chars[index]) {
            continue;
        }
        let mut repaired = String::new();
        for (i, character) in chars.iter().enumerate() {
            if i == index {
                continue;
            }
            repaired.push(*character);
        }
        push_unique(
            out,
            typed,
            &repaired,
            typed.len(),
            ADVANCED_REPAIR_PENALTY,
            RepairKind::Advanced,
            limit,
        );
    }
    // Single-character substitutions among a small vowel/nasal alphabet.
    const ALPHABET: [char; 8] = ['あ', 'い', 'う', 'え', 'お', 'ん', 'っ', 'ー'];
    for index in 0..chars.len() {
        if !is_hiragana(chars[index]) && !is_katakana(chars[index]) {
            continue;
        }
        for candidate in ALPHABET {
            if candidate == chars[index] {
                continue;
            }
            let mut repaired = String::new();
            for (i, character) in chars.iter().enumerate() {
                if i == index {
                    repaired.push(candidate);
                } else {
                    repaired.push(*character);
                }
            }
            push_unique(
                out,
                typed,
                &repaired,
                typed.len(),
                ADVANCED_REPAIR_PENALTY,
                RepairKind::Advanced,
                limit,
            );
        }
    }
}

fn is_advanced_deletable(character: char) -> bool {
    matches!(
        character,
        'ん' | 'っ'
            | 'ー'
            | 'ぁ'
            | 'ぃ'
            | 'ぅ'
            | 'ぇ'
            | 'ぉ'
            | 'ゃ'
            | 'ゅ'
            | 'ょ'
            | 'ァ'
            | 'ィ'
            | 'ゥ'
            | 'ェ'
            | 'ォ'
            | 'ャ'
            | 'ュ'
            | 'ョ'
            | 'ッ'
    )
}

fn is_katakana(character: char) -> bool {
    matches!(character, '\u{30A1}'..='\u{30F6}')
}

fn swap_dakuten(character: char) -> Option<char> {
    Some(match character {
        'は' => 'ば',
        'ひ' => 'び',
        'ふ' => 'ぶ',
        'へ' => 'べ',
        'ほ' => 'ぼ',
        'ば' => 'ぱ',
        'び' => 'ぴ',
        'ぶ' => 'ぷ',
        'べ' => 'ぺ',
        'ぼ' => 'ぽ',
        'ぱ' => 'ば',
        'ぴ' => 'び',
        'ぷ' => 'ぶ',
        'ぺ' => 'べ',
        'ぽ' => 'ぼ',
        'が' => 'か',
        'ぎ' => 'き',
        'ぐ' => 'く',
        'げ' => 'け',
        'ご' => 'こ',
        'ざ' => 'さ',
        'じ' => 'し',
        'ず' => 'す',
        'ぜ' => 'せ',
        'ぞ' => 'そ',
        'だ' => 'た',
        'ぢ' => 'ち',
        'づ' => 'つ',
        'で' => 'て',
        'ど' => 'と',
        'か' => 'が',
        'き' => 'ぎ',
        'く' => 'ぐ',
        'け' => 'げ',
        'こ' => 'ご',
        'さ' => 'ざ',
        'し' => 'じ',
        'す' => 'ず',
        'せ' => 'ぜ',
        'そ' => 'ぞ',
        'た' => 'だ',
        'ち' => 'ぢ',
        'つ' => 'づ',
        'て' => 'で',
        'と' => 'ど',
        _ => return None,
    })
}

fn lengthening_vowel(character: char) -> Option<char> {
    Some(match character {
        'あ' | 'か' | 'さ' | 'た' | 'な' | 'は' | 'ま' | 'や' | 'ら' | 'わ' | 'が' | 'ざ'
        | 'だ' | 'ば' | 'ぱ' => 'あ',
        'い' | 'き' | 'し' | 'ち' | 'に' | 'ひ' | 'み' | 'り' | 'ぎ' | 'じ' | 'ぢ' | 'び'
        | 'ぴ' => 'い',
        'う' | 'く' | 'す' | 'つ' | 'ぬ' | 'ふ' | 'む' | 'ゆ' | 'る' | 'ぐ' | 'ず' | 'づ'
        | 'ぶ' | 'ぷ' => 'う',
        'え' | 'け' | 'せ' | 'て' | 'ね' | 'へ' | 'め' | 'れ' | 'げ' | 'ぜ' | 'で' | 'べ'
        | 'ぺ' => 'い',
        'お' | 'こ' | 'そ' | 'と' | 'の' | 'ほ' | 'も' | 'よ' | 'ろ' | 'を' | 'ご' | 'ぞ'
        | 'ど' | 'ぼ' | 'ぽ' => 'う',
        _ => return None,
    })
}

fn is_vowel_kana(character: char) -> bool {
    matches!(character, 'あ' | 'い' | 'う' | 'え' | 'お' | 'ー')
}

fn is_hiragana(character: char) -> bool {
    matches!(character, '\u{3041}'..='\u{3096}')
}

fn is_latin_letter(character: char) -> bool {
    to_half_ascii(character).is_ascii_alphabetic()
}

fn to_half_ascii(character: char) -> char {
    match character {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(character as u32 - 0xFEE0).unwrap_or(character),
        other => other,
    }
}

fn kana_to_romaji(character: char) -> Option<&'static str> {
    Some(match character {
        'あ' => "a",
        'い' => "i",
        'う' | 'ぅ' => "u",
        'え' => "e",
        'お' => "o",
        'か' => "ka",
        'き' => "ki",
        'く' => "ku",
        'け' => "ke",
        'こ' => "ko",
        'さ' => "sa",
        'し' => "shi",
        'す' => "su",
        'せ' => "se",
        'そ' => "so",
        'た' => "ta",
        'ち' => "chi",
        'つ' => "tsu",
        'っ' => "tsu",
        'て' => "te",
        'と' => "to",
        'な' => "na",
        'に' => "ni",
        'ぬ' => "nu",
        'ね' => "ne",
        'の' => "no",
        'は' => "ha",
        'ひ' => "hi",
        'ふ' => "fu",
        'へ' => "he",
        'ほ' => "ho",
        'ま' => "ma",
        'み' => "mi",
        'む' => "mu",
        'め' => "me",
        'も' => "mo",
        'や' => "ya",
        'ゆ' => "yu",
        'よ' => "yo",
        'ら' => "ra",
        'り' => "ri",
        'る' => "ru",
        'れ' => "re",
        'ろ' => "ro",
        'わ' => "wa",
        'を' => "wo",
        'ん' => "n",
        'が' => "ga",
        'ぎ' => "gi",
        'ぐ' => "gu",
        'げ' => "ge",
        'ご' => "go",
        'ざ' => "za",
        'じ' => "ji",
        'ず' => "zu",
        'ぜ' => "ze",
        'ぞ' => "zo",
        'だ' => "da",
        'ぢ' => "di",
        'づ' => "du",
        'で' => "de",
        'ど' => "do",
        'ば' => "ba",
        'び' => "bi",
        'ぶ' => "bu",
        'べ' => "be",
        'ぼ' => "bo",
        'ぱ' => "pa",
        'ぴ' => "pi",
        'ぷ' => "pu",
        'ぺ' => "pe",
        'ぽ' => "po",
        'ぁ' => "a",
        'ぃ' => "i",
        'ぇ' => "e",
        'ぉ' => "o",
        'ゃ' => "ya",
        'ゅ' => "yu",
        'ょ' => "yo",
        'ー' => "",
        _ => return None,
    })
}

fn romaji_to_katakana(romaji: &str) -> Option<FixedStr<MAX_PREEDIT_BYTES>> {
    // Minimal longest-match table covering ordinary English loanword spellings.
    const TABLE: &[(&str, &str)] = &[
        ("kya", "キャ"),
        ("kyu", "キュ"),
        ("kyo", "キョ"),
        ("sha", "シャ"),
        ("shu", "シュ"),
        ("sho", "ショ"),
        ("cha", "チャ"),
        ("chu", "チュ"),
        ("cho", "チョ"),
        ("nya", "ニャ"),
        ("nyu", "ニュ"),
        ("nyo", "ニョ"),
        ("hya", "ヒャ"),
        ("hyu", "ヒュ"),
        ("hyo", "ヒョ"),
        ("mya", "ミャ"),
        ("myu", "ミュ"),
        ("myo", "ミョ"),
        ("rya", "リャ"),
        ("ryu", "リュ"),
        ("ryo", "リョ"),
        ("gya", "ギャ"),
        ("gyu", "ギュ"),
        ("gyo", "ギョ"),
        ("ja", "ジャ"),
        ("ju", "ジュ"),
        ("jo", "ジョ"),
        ("bya", "ビャ"),
        ("byu", "ビュ"),
        ("byo", "ビョ"),
        ("pya", "ピャ"),
        ("pyu", "ピュ"),
        ("pyo", "ピョ"),
        ("shi", "シ"),
        ("chi", "チ"),
        ("tsu", "ツ"),
        ("ka", "カ"),
        ("ki", "キ"),
        ("ku", "ク"),
        ("ke", "ケ"),
        ("ko", "コ"),
        ("sa", "サ"),
        ("su", "ス"),
        ("se", "セ"),
        ("so", "ソ"),
        ("ta", "タ"),
        ("te", "テ"),
        ("to", "ト"),
        ("na", "ナ"),
        ("ni", "ニ"),
        ("nu", "ヌ"),
        ("ne", "ネ"),
        ("no", "ノ"),
        ("ha", "ハ"),
        ("hi", "ヒ"),
        ("fu", "フ"),
        ("he", "ヘ"),
        ("ho", "ホ"),
        ("ma", "マ"),
        ("mi", "ミ"),
        ("mu", "ム"),
        ("me", "メ"),
        ("mo", "モ"),
        ("ya", "ヤ"),
        ("yu", "ユ"),
        ("yo", "ヨ"),
        ("ra", "ラ"),
        ("ri", "リ"),
        ("ru", "ル"),
        ("re", "レ"),
        ("ro", "ロ"),
        ("wa", "ワ"),
        ("wo", "ヲ"),
        ("ga", "ガ"),
        ("gi", "ギ"),
        ("gu", "グ"),
        ("ge", "ゲ"),
        ("go", "ゴ"),
        ("za", "ザ"),
        ("ji", "ジ"),
        ("zu", "ズ"),
        ("ze", "ゼ"),
        ("zo", "ゾ"),
        ("da", "ダ"),
        ("de", "デ"),
        ("do", "ド"),
        ("ba", "バ"),
        ("bi", "ビ"),
        ("bu", "ブ"),
        ("be", "ベ"),
        ("bo", "ボ"),
        ("pa", "パ"),
        ("pi", "ピ"),
        ("pu", "プ"),
        ("pe", "ペ"),
        ("po", "ポ"),
        ("nn", "ン"),
        ("a", "ア"),
        ("i", "イ"),
        ("u", "ウ"),
        ("e", "エ"),
        ("o", "オ"),
        ("n", "ン"),
        ("-", "ー"),
    ];
    let mut remaining = romaji;
    let mut out = FixedStr::new();
    while !remaining.is_empty() {
        // Sokuon via consonant doubling (pp, tt, kk, ss, ...).
        let bytes = remaining.as_bytes();
        if bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[0] == bytes[1]
            && !matches!(bytes[0], b'a' | b'i' | b'u' | b'e' | b'o' | b'n')
        {
            out.push_str("ッ").ok()?;
            remaining = &remaining[1..];
            continue;
        }
        let mut matched = false;
        for (sequence, kana) in TABLE {
            if remaining.starts_with(sequence) {
                out.push_str(kana).ok()?;
                remaining = &remaining[sequence.len()..];
                matched = true;
                break;
            }
        }
        if !matched {
            return None;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::width::{BracketStyle, Normalizer, PunctuationStyle, WidthPolicy};
    use sakura_proto::Mode;

    #[test]
    fn duplicated_n_and_missing_n_are_repaired() {
        let support = InputSupport::default();
        let variants = collect_repair_variants("こんんにちは", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "こんにちは"));
        let variants = collect_repair_variants("こにちは", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "こんにちは"));
    }

    #[test]
    fn extra_sokuon_and_wa_wo_are_repaired() {
        let support = InputSupport::default();
        let variants = collect_repair_variants("がっっこう", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "がっこう"));
        let variants = collect_repair_variants("をかし", support, MAX_REPAIR_VARIANTS);
        assert!(
            variants
                .iter()
                .any(|variant| variant.repaired.as_str() == "をかし")
                || variants
                    .iter()
                    .any(|variant| variant.repaired.as_str().contains('を'))
        );
        let variants = collect_repair_variants("わたし", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "をたし"));
    }

    #[test]
    fn master_off_emits_no_variants() {
        let support = InputSupport {
            enabled: false,
            ..InputSupport::default()
        };
        let variants = collect_repair_variants("こんんにちは", support, MAX_REPAIR_VARIANTS);
        assert!(variants.is_empty());
    }

    #[test]
    fn english_spelling_reconstructs_apple_and_internet() {
        let apple = english_spelling_katakana_reading("あｐｐｌｅ").expect("apple");
        assert_eq!(apple.as_str(), "アップル");
        let apple_sokuon = english_spelling_katakana_reading("あっｐｌｅ").expect("apple sokuon");
        assert_eq!(apple_sokuon.as_str(), "アップル");
        let internet = english_spelling_katakana_reading("いんてｒねｔ").expect("internet");
        assert_eq!(internet.as_str(), "インターネット");
    }

    #[test]
    fn contextual_punctuation_swaps_after_digits_and_alnum() {
        let support = InputSupport::default();
        assert_eq!(
            contextual_punctuation_swap(Some('1'), '。', support),
            Some('．')
        );
        assert_eq!(
            contextual_punctuation_swap(Some('a'), 'ー', support),
            Some('－')
        );
        assert_eq!(contextual_punctuation_swap(Some('あ'), '。', support), None);
    }

    #[test]
    fn contextual_punctuation_swap_defers_the_final_glyph_to_the_punctuation_style() {
        // The digit rule decides the *role* — after `1`, a 。 is a decimal
        // point, not a full stop — and deliberately stops there. It returns
        // one of the four code points the punctuation choke point owns, so
        // the configured style still picks the glyph. A reader who set the
        // period to half-width for a manuscript typeset from plain text gets
        // `1.5`, not a full-width `1．5` the setting could not reach.
        let support = InputSupport::default();
        let ascii = Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::ASCII,
            brackets: BracketStyle::default(),
        };
        let period = contextual_punctuation_swap(Some('1'), '。', support).expect("digit period");
        assert_eq!(ascii.normalize_char(period, Mode::Hiragana), '.');
        let comma = contextual_punctuation_swap(Some('1'), '、', support).expect("digit comma");
        assert_eq!(ascii.normalize_char(comma, Mode::Hiragana), ',');

        // Same rule, the full-width styles: nothing about the swap is
        // hard-wired to one width.
        let full = Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::COMMA_PERIOD,
            brackets: BracketStyle::default(),
        };
        assert_eq!(full.normalize_char(period, Mode::Hiragana), '\u{FF0E}');
        assert_eq!(full.normalize_char(comma, Mode::Hiragana), '\u{FF0C}');
    }

    #[test]
    fn original_reading_is_never_emitted_as_a_variant() {
        let support = InputSupport::default();
        let variants = collect_repair_variants("こんにちは", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .all(|variant| variant.repaired.as_str() != "こんにちは"));
    }

    #[test]
    fn allows_system_entry_rejects_spelling_correction_when_gated() {
        let support = InputSupport::default();
        assert!(allows_system_entry(
            support,
            false,
            EntryFlags::SPELLING_CORRECTION
        ));
        assert!(!allows_system_entry(
            support,
            true,
            EntryFlags::SPELLING_CORRECTION
        ));
        assert!(allows_system_entry(support, true, EntryFlags::IT));
        assert!(allows_system_entry(
            support,
            false,
            EntryFlags::IT | EntryFlags::SPELLING_CORRECTION
        ));
        let off = InputSupport {
            enabled: false,
            ..InputSupport::default()
        };
        assert!(!allows_system_entry(
            off,
            false,
            EntryFlags::IT | EntryFlags::SPELLING_CORRECTION
        ));
    }

    #[test]
    fn vowel_and_kana_rules_emit_bounded_repairs() {
        let support = InputSupport::default();
        let variants = collect_repair_variants("おはよ", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "おはよう"));
        let variants = collect_repair_variants("つづく", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .any(|variant| variant.repaired.as_str() == "っづく"
                || variant.repaired.as_str() == "つずく"));
    }

    #[test]
    fn decimal_counter_inputs_never_emit_kana_repairs() {
        const SUFFIXES: [&str; 24] = [
            "えん",
            "かい",
            "がつ",
            "けん",
            "こ",
            "さい",
            "じ",
            "せき",
            "そく",
            "だい",
            "つ",
            "にち",
            "にん",
            "ねん",
            "はい",
            "ばん",
            "ふん",
            "ぷん",
            "ほん",
            "まい",
            "わり",
            "ちゃく",
            "ひき",
            "かしょ",
        ];
        let support = InputSupport::default();
        for digit in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'] {
            for suffix in SUFFIXES {
                let typed = format!("{digit}{suffix}");
                assert!(
                    collect_repair_variants(&typed, support, MAX_REPAIR_VARIANTS).is_empty(),
                    "ASCII digit/counter input emitted a repair: {typed}"
                );
                let fullwidth = char::from_u32(digit as u32 + 0xFEE0).expect("full-width digit");
                let typed = format!("{fullwidth}{suffix}");
                assert!(
                    collect_repair_variants(&typed, support, MAX_REPAIR_VARIANTS).is_empty(),
                    "full-width digit/counter input emitted a repair: {typed}"
                );
            }
        }
    }

    #[test]
    fn advanced_substitution_preserves_non_kana_literals() {
        let mut variants = RepairVariantList::new();
        push_advanced_variants("Aかい", &mut variants, MAX_REPAIR_VARIANTS);
        assert!(
            !variants.is_empty(),
            "kana positions should remain repairable"
        );
        assert!(
            variants
                .iter()
                .all(|variant| variant.repaired.as_str().starts_with('A')),
            "Advanced substitution replaced a non-kana literal: {variants:?}"
        );
    }

    #[test]
    fn individual_flags_can_disable_specific_rules() {
        let support = InputSupport {
            n_count: false,
            ..InputSupport::default()
        };
        let variants = collect_repair_variants("こにちは", support, MAX_REPAIR_VARIANTS);
        assert!(variants
            .iter()
            .all(|variant| variant.repaired.as_str() != "こんにちは"));
    }
}
