//! Candidate order the shipped system dictionary produces.
//!
//! Number readings: upstream Mozc prices every Arabic digit in one numeral
//! class far below the words that share its reading, so before
//! `data/conversion-priorities.tsv` carried digit rows a bare digit led seven
//! of the eleven single-digit readings -- typing `いち` offered `1` ahead of
//! `市`, `位置` and `一`. Sakura has no number rewriter, so the lattice cost is
//! the whole story and the calibration overlay is what keeps a digit behind
//! the word it spells.
//!
//! Issue #62: `きのうしょうかい` must convert as the IT compound `機能紹介`,
//! not the homophone split `昨日紹介`. The shipped image keeps the cheap
//! `昨日` edges for bare `きのう` and date phrases; the compound itself is the
//! evidence that outranks that split.
//!
//! These tests read the built dictionary, which is a local build artifact
//! (`/artifacts/` is not tracked), so they are ignored by default. Run them
//! after `scripts/build-dictionary.ps1`:
//!
//! ```text
//! cargo test -p sakura-engine --test shipped_dictionary_ranking -- --ignored
//! SAKURA_SYSTEM_DIC=<path>\system.dic cargo test ... -- --ignored
//! ```
use std::path::{Path, PathBuf};

use sakura_core::ConversionOptions;
use sakura_engine::dispatch::{Dispatcher, Reply};
use sakura_proto::{
    InputScope, KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response, SessionId,
};

/// `(romaji, reading, the digit that spells it, the kanji numeral the digit
/// must not overtake)`. The trimmed lexicon carries no kanji numeral for `よん`
/// or `ぜろ`, so those only require the digit to trail an ordinary word.
type DigitReading = (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

const DIGIT_READINGS: &[DigitReading] = &[
    ("ichi", "いち", "1", Some("一")),
    ("ni", "に", "2", Some("二")),
    ("sann", "さん", "3", Some("三")),
    ("yonn", "よん", "4", None),
    ("go", "ご", "5", Some("五")),
    ("roku", "ろく", "6", Some("六")),
    ("nana", "なな", "7", Some("七")),
    ("hachi", "はち", "8", Some("八")),
    ("kyuu", "きゅう", "9", Some("九")),
    ("zero", "ぜろ", "0", None),
    ("rei", "れい", "0", Some("零")),
];

fn dictionary_path() -> PathBuf {
    match std::env::var("SAKURA_SYSTEM_DIC") {
        Ok(value) => PathBuf::from(value),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/release/system.dic"),
    }
}

fn char_key(ch: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(ch),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn space_key() -> KeyInput {
    KeyInput {
        code: KeyCode::Space,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

/// Types `romaji`, converts once, and returns the reading it composed together
/// with the candidate surfaces in the order the engine offers them.
fn convert(dispatcher: &mut Dispatcher, romaji: &str) -> (String, Vec<String>) {
    let mut out = OutputBuf::new();
    let session: SessionId = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "shipped_dictionary_ranking.exe".to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("session for {romaji}: unexpected {other:?}"),
    };
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );

    for ch in romaji.chars() {
        out = OutputBuf::new();
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(ch),
            },
            &mut out,
        );
    }
    let reading = out
        .to_output()
        .preedit
        .map(|preedit| {
            preedit
                .segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<String>()
        })
        .unwrap_or_default();

    out = OutputBuf::new();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: space_key(),
        },
        &mut out,
    );
    let candidates = out
        .to_output()
        .candidates
        .map(|list| {
            list.items
                .iter()
                .map(|item| item.text.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    (reading, candidates)
}

fn open_conversion() -> std::sync::Arc<sakura_engine::dictionary::ConversionService> {
    let path = dictionary_path();
    let path = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("build the dictionary first ({}): {error}", path.display()));
    sakura_engine::dictionary::open(&path).expect("open dictionary")
}

fn candidates_for(reading: &str) -> Vec<String> {
    open_conversion()
        .with_candidates(reading, ConversionOptions::default(), |candidates| {
            candidates
                .iter()
                .map(|candidate| candidate.text().to_owned())
                .collect()
        })
        .unwrap_or_else(|error| panic!("{reading}: {error}"))
}

fn top_text(reading: &str) -> String {
    let candidates = candidates_for(reading);
    assert!(
        !candidates.is_empty(),
        "{reading}: conversion returned no candidates"
    );
    candidates[0].clone()
}

fn open_dispatcher() -> Dispatcher {
    let conversion = open_conversion();
    Dispatcher::new_with_conversion(conversion).expect("dispatcher")
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn a_bare_digit_never_leads_a_single_digit_reading() {
    let mut dispatcher = open_dispatcher();

    for (romaji, reading, digit, kanji_numeral) in DIGIT_READINGS {
        let (composed, candidates) = convert(&mut dispatcher, romaji);
        assert_eq!(&composed, reading, "{romaji} composed {composed}");
        assert!(!candidates.is_empty(), "{reading}: no candidates at all");

        assert_ne!(
            &candidates[0],
            digit,
            "{reading}: the bare digit leads the list ({:?})",
            &candidates[..candidates.len().min(6)]
        );

        let digit_at = candidates
            .iter()
            .position(|item| item == digit)
            .unwrap_or_else(|| {
                panic!(
                    "{reading}: the digit spelling disappeared ({:?})",
                    &candidates[..candidates.len().min(6)]
                )
            });

        if let Some(numeral) = kanji_numeral {
            let numeral_at = candidates
                .iter()
                .position(|item| item == numeral)
                .unwrap_or_else(|| {
                    panic!(
                        "{reading}: {numeral} missing from the candidates ({:?})",
                        &candidates[..candidates.len().min(8)]
                    )
                });
            assert!(
                numeral_at < digit_at,
                "{reading}: the digit still precedes {numeral} ({:?})",
                &candidates[..candidates.len().min(8)]
            );
        }
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn compound_number_words_keep_both_spellings() {
    let mut dispatcher = open_dispatcher();

    // Compounds are their own lexicon entries, so the single-digit calibration
    // must not move them: both spellings stay reachable and the whole-reading
    // word still wins where upstream priced it first.
    let expectations: &[(&str, &str, &[&str], &str)] = &[
        ("ichigatsu", "いちがつ", &["1月", "一月"], ""),
        ("hachigatsu", "はちがつ", &["8月", "八月"], ""),
        ("ichinichi", "いちにち", &["一日", "1日"], "一日"),
        ("ichido", "いちど", &["一度", "1度"], "一度"),
        ("ichibann", "いちばん", &["一番", "1番"], "一番"),
    ];

    for (romaji, reading, required, top) in expectations {
        let (composed, candidates) = convert(&mut dispatcher, romaji);
        assert_eq!(&composed, reading, "{romaji} composed {composed}");
        for surface in *required {
            assert!(
                candidates.iter().any(|item| item == surface),
                "{reading}: {surface} is unreachable ({:?})",
                &candidates[..candidates.len().min(8)]
            );
        }
        if !top.is_empty() {
            assert_eq!(
                &candidates[0],
                top,
                "{reading}: unexpected first candidate ({:?})",
                &candidates[..candidates.len().min(8)]
            );
        }
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn ranks_function_introduction_above_yesterday_introduction() {
    let candidates = candidates_for("きのうしょうかい");
    assert_eq!(
        candidates.first().map(String::as_str),
        Some("機能紹介"),
        "きのうしょうかい: unexpected first candidate ({:?})",
        &candidates[..candidates.len().min(8)]
    );
    assert!(
        candidates.iter().any(|item| item == "昨日紹介"),
        "きのうしょうかい: 昨日紹介 disappeared ({:?})",
        &candidates[..candidates.len().min(8)]
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn composes_prefix_with_function_introduction() {
    assert_eq!(top_text("ぜんきのうしょうかい"), "全機能紹介");
    assert_eq!(top_text("しんきのうしょうかい"), "新機能紹介");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn ranks_function_requirement_above_yesterday_requirement() {
    assert_eq!(top_text("きのうようけん"), "機能要件");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn ranks_function_component_above_yesterday_component() {
    assert_eq!(top_text("きのうこんぽーねんと"), "機能コンポーネント");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn preserves_standalone_yesterday() {
    assert_eq!(top_text("きのう"), "昨日");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn preserves_yesterday_date_compounds() {
    assert_eq!(top_text("きのういがい"), "昨日以外");
    assert_eq!(top_text("きのうげんざい"), "昨日現在");
    assert_eq!(top_text("きのうごご"), "昨日午後");
    assert_eq!(top_text("きのうあさ"), "昨日朝");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn preserves_existing_function_compounds_and_sibling_dates() {
    assert_eq!(top_text("きのういちらん"), "機能一覧");
    assert_eq!(top_text("きのうがいよう"), "機能概要");
    assert_eq!(top_text("きのうせっけい"), "機能設計");
    assert_eq!(top_text("きょう"), "今日");
    assert_eq!(top_text("あした"), "明日");
}
