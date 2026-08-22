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
use std::sync::Arc;
use std::time::Duration;

use sakura_core::{ConversionOptions, InputMethod, InputSupport, Preferences};
use sakura_engine::dispatch::{Dispatcher, Reply};
use sakura_engine::learning::LearningService;
use sakura_engine::prediction::PredictionRuntime;
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

fn open_dispatcher_with_input_method(input_method: InputMethod) -> Dispatcher {
    let conversion = open_conversion();
    let learning = Arc::new(LearningService::memory());
    let preferences = Preferences {
        input_method,
        ..Preferences::default()
    };
    Dispatcher::new_with_configuration(conversion, learning, preferences)
        .expect("configured dispatcher")
}

fn predictions_for(prefix: &str) -> Vec<(String, String)> {
    let conversion = open_conversion();
    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("prediction runtime");
    let result = runtime
        .service()
        .request(
            1,
            1,
            prefix,
            0,
            InputSupport::default(),
            false,
            Duration::from_secs(1),
        )
        .unwrap_or_else(|| panic!("{prefix}: prediction request failed"));
    let candidates = result
        .candidates()
        .iter()
        .map(|candidate| {
            (
                candidate.reading().to_owned(),
                candidate.surface().to_owned(),
            )
        })
        .collect();
    runtime.stop().expect("joined prediction runtime");
    candidates
}

fn convert_direct_kana(dispatcher: &mut Dispatcher, kana: &str) -> (String, Vec<String>) {
    let mut out = OutputBuf::new();
    let session: SessionId = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "shipped_dictionary_ranking-kana.exe".to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("session for {kana}: unexpected {other:?}"),
    };
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );

    for ch in kana.chars() {
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

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn reported_prediction_prefixes_keep_direct_candidates_before_repairs() {
    let conversion = open_conversion();
    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("prediction runtime");
    let service = runtime.service();

    for (generation, (prefix, expected_first)) in [
        ("かが", "輝け"),
        ("みみ", "みみりん"),
        ("まだ", "まだ"),
        ("ただ", "正しかろ"),
    ]
    .into_iter()
    .enumerate()
    {
        let result = service
            .request(
                1,
                u64::try_from(generation).expect("generation"),
                prefix,
                1_000,
                InputSupport::default(),
                false,
                Duration::from_secs(1),
            )
            .unwrap_or_else(|| panic!("{prefix}: prediction request failed"));
        let candidates = result.candidates();
        let direct_count = candidates
            .iter()
            .filter(|candidate| candidate.reading().starts_with(prefix))
            .count();
        let first_repair = candidates
            .iter()
            .position(|candidate| !candidate.reading().starts_with(prefix));
        let direct_after_repair = first_repair
            .map(|repair| {
                candidates[repair..]
                    .iter()
                    .any(|candidate| candidate.reading().starts_with(prefix))
            })
            .unwrap_or(false);
        let surfaces: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.surface())
            .collect();

        assert!(
            direct_count > 0,
            "{prefix}: no direct candidate in {:?}",
            surfaces
        );
        assert_eq!(
            candidates.first().map(|candidate| candidate.surface()),
            Some(expected_first),
            "{prefix}: unexpected first candidate in {surfaces:?}"
        );
        assert!(
            !direct_after_repair,
            "{prefix}: direct candidate followed a repair in {surfaces:?}"
        );
        for (index, surface) in surfaces.iter().enumerate() {
            assert_eq!(
                surfaces[..index]
                    .iter()
                    .filter(|previous| *previous == surface)
                    .count(),
                0,
                "{prefix}: duplicate surface {surface} in {surfaces:?}"
            );
        }
    }

    runtime.stop().expect("joined prediction runtime");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn advanced_reading_only_repair_is_absent_for_romaji_and_direct_kana() {
    let mut romaji_dispatcher = open_dispatcher();
    let (romaji_reading, romaji_candidates) = convert(&mut romaji_dispatcher, "nazeka");
    assert_eq!(romaji_reading, "なぜか");
    assert!(
        romaji_candidates.iter().all(|surface| surface != "内科"),
        "Romaji なぜか must not expose the Advanced-derived 内科 candidate: {romaji_candidates:?}"
    );

    let mut kana_dispatcher = open_dispatcher_with_input_method(InputMethod::Kana);
    let (kana_reading, kana_candidates) = convert_direct_kana(&mut kana_dispatcher, "なぜか");
    assert_eq!(kana_reading, "なぜか");
    assert!(
        kana_candidates.iter().all(|surface| surface != "内科"),
        "direct Kana なぜか must not expose the Advanced-derived 内科 candidate: {kana_candidates:?}"
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn raw_key_phase1_repairs_stay_after_direct_candidates_in_shipped_dictionary() {
    let mut dispatcher = open_dispatcher();

    let (nazka_reading, nazka_candidates) = convert(&mut dispatcher, "nazka");
    println!("Romaji nazka => {nazka_reading:?}: {nazka_candidates:?}");
    assert_eq!(nazka_reading, "なzか");
    assert_eq!(
        nazka_candidates.first().map(String::as_str),
        Some("なzか"),
        "the exact direct reading must remain candidate #0"
    );
    assert!(
        nazka_candidates.iter().any(|surface| surface == "なぜか"),
        "Romaji nazka must expose the local completion なぜか: {nazka_candidates:?}"
    );
    for (index, surface) in nazka_candidates.iter().enumerate() {
        assert_eq!(
            nazka_candidates[..index]
                .iter()
                .filter(|previous| *previous == surface)
                .count(),
            0,
            "nazka: duplicate surface {surface} in {nazka_candidates:?}"
        );
    }

    // Keep each positive in its own isolated dispatcher.  The live dispatcher
    // intentionally fences simultaneous composing sessions from the same host
    // process, while this artifact regression is about independent readings.
    let mut naik_dispatcher = open_dispatcher();
    let (naikniiku_reading, naikniiku_candidates) = convert(&mut naik_dispatcher, "naikniiku");
    println!("Romaji naikniiku => {naikniiku_reading:?}: {naikniiku_candidates:?}");
    assert_eq!(naikniiku_reading, "ないkにいく");
    assert_eq!(
        naikniiku_candidates.first().map(String::as_str),
        Some("ないkにいく"),
        "the exact direct reading must remain candidate #0"
    );
    let semantic_candidate = naikniiku_candidates
        .iter()
        .find(|surface| surface.contains("内科"))
        .unwrap_or_else(|| {
            panic!(
                "Romaji naikniiku must expose a candidate representing 内科に行く: {naikniiku_candidates:?}"
            )
        });
    assert!(
        semantic_candidate.contains("行") || semantic_candidate.contains("いく"),
        "unexpected 内科 candidate for naikniiku: {semantic_candidate}"
    );
    for (index, surface) in naikniiku_candidates.iter().enumerate() {
        assert_eq!(
            naikniiku_candidates[..index]
                .iter()
                .filter(|previous| *previous == surface)
                .count(),
            0,
            "naikniiku: duplicate surface {surface} in {naikniiku_candidates:?}"
        );
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn a_bare_digit_never_leads_a_single_digit_reading() {
    for (romaji, reading, digit, kanji_numeral) in DIGIT_READINGS {
        let mut dispatcher = open_dispatcher();
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
fn bare_spoken_numbers_defer_to_common_words_but_keep_numeric_forms() {
    for (reading, lexical, numeric_forms) in [
        ("せん", "線", ["1000", "１０００", "千"]),
        ("にじゅう", "二重", ["20", "２０", "二十"]),
        ("さんぜん", "産前", ["3000", "３０００", "三千"]),
    ] {
        let candidates = candidates_for(reading);
        assert_eq!(
            candidates.first().map(String::as_str),
            Some(lexical),
            "{reading}: unexpected first candidate ({:?})",
            &candidates[..candidates.len().min(9)]
        );
        for numeric in numeric_forms {
            assert!(
                candidates.iter().any(|candidate| candidate == numeric),
                "{reading}: {numeric} disappeared ({:?})",
                &candidates[..candidates.len().min(12)]
            );
        }
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn counter_and_calendar_number_readings_keep_their_strong_forms() {
    for (reading, expected) in [("せんにち", "千日"), ("にじゅうよんにち", "24日")] {
        let candidates = candidates_for(reading);
        assert_eq!(
            candidates.first().map(String::as_str),
            Some(expected),
            "{reading}: unexpected first candidate ({:?})",
            &candidates[..candidates.len().min(9)]
        );
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn compound_number_words_keep_both_spellings() {
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
        let mut dispatcher = open_dispatcher();
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

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn ranks_more_function_compounds_above_yesterday_splits() {
    let wanted = [
        ("きのうついか", "機能追加"),
        ("きのうせいげん", "機能制限"),
        ("きのうかいぜん", "機能改善"),
        ("きのうかくちょう", "機能拡張"),
        ("きのうさくじょ", "機能削除"),
        ("きのうじっそう", "機能実装"),
        ("きのうきょうか", "機能強化"),
        ("きのうていし", "機能停止"),
        ("きのうようぼう", "機能要望"),
        ("きのうへんこう", "機能変更"),
        ("きのうせつめい", "機能説明"),
        ("きのうひょうか", "機能評価"),
        ("きのうかいはつ", "機能開発"),
        ("きのうてすと", "機能テスト"),
        ("きのうしよう", "機能仕様"),
        ("きのうてき", "機能的"),
        ("きのうじょう", "機能上"),
        ("きのうめい", "機能名"),
        ("きのうめん", "機能面"),
    ];
    for (reading, want) in wanted {
        assert_eq!(top_text(reading), want, "{reading}");
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn ranks_business_compounds_above_homophone_splits() {
    let wanted = [
        ("けんしゅうかんりょう", "検収完了"),
        ("けんしゅうしょ", "検収書"),
        ("けんしゅうしけん", "検収試験"),
        ("けっさいしょ", "決裁書"),
        ("けっさいしゃ", "決裁者"),
        ("いどうとどけ", "異動届"),
        ("しゅうぎょうきてい", "就業規程"),
        ("しゃないきてい", "社内規程"),
        ("ふくむきてい", "服務規程"),
        ("はいふきんじゅん", "配賦基準"),
        ("こうつうひせいさん", "交通費精算"),
        ("しょうひょうしょるい", "証憑書類"),
    ];
    for (reading, want) in wanted {
        assert_eq!(top_text(reading), want, "{reading}");
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn generic_candidate_quality_covers_reported_unregistered_readings() {
    for (reading, expected) in [
        ("きのうかくにん", "機能確認"),
        ("きのうぜんぱん", "機能全般"),
        ("きのうとうごう", "機能統合"),
    ] {
        let candidates = candidates_for(reading);
        assert_eq!(
            candidates.first().map(String::as_str),
            Some(expected),
            "{reading}: generic compound coherence failed: {:?}",
            &candidates[..candidates.len().min(9)]
        );
    }

    let nin = candidates_for("にん");
    let whole = nin
        .iter()
        .position(|surface| surface == "人")
        .expect("にん keeps the whole-reading 人 candidate");
    let shortened = nin.iter().position(|surface| surface == "に");
    assert!(
        shortened.is_none_or(|shortened| whole < shortened),
        "にん: a shortened repair outranked the whole reading: {nin:?}"
    );

    let mut dispatcher = open_dispatcher();
    let (reading, ui_nin) = convert(&mut dispatcher, "ninn");
    assert_eq!(reading, "にん");
    assert_eq!(
        ui_nin.first().map(String::as_str),
        Some("人"),
        "にん: the user-facing path did not prefer the whole reading: {ui_nin:?}"
    );
    assert!(
        ui_nin.iter().take(9).all(|surface| surface != "に"),
        "にん: a shortened candidate reached the first page: {ui_nin:?}"
    );

    let azure = candidates_for("あじゅーる");
    assert_eq!(
        azure.first().map(String::as_str),
        Some("アジュール"),
        "あじゅーる: unexpected top candidate: {azure:?}"
    );
    assert!(
        azure.iter().take(9).any(|surface| surface == "Azure"),
        "あじゅーる: exact Azure spelling left the first page: {azure:?}"
    );
    assert!(
        azure.iter().take(9).all(|surface| {
            !surface.starts_with("あジュール")
                && !surface.starts_with("亜ジュール")
                && !surface.starts_with("亜joule")
                && !surface.starts_with("亜Joule")
        }),
        "あじゅーる: low-evidence mosaic reached the first page: {azure:?}"
    );

    let weekly = candidates_for("しゅうじ");
    assert!(
        weekly.iter().take(9).any(|surface| surface == "週次"),
        "しゅうじ: 週次 is missing from the first page: {weekly:?}"
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn prediction_prefers_exact_readings_and_does_not_repair_known_words() {
    let oracle = predictions_for("おらくる");
    assert_eq!(
        oracle.first().map(|(_, surface)| surface.as_str()),
        Some("オラクル"),
        "おらくる: an extension outranked the exact reading: {oracle:?}"
    );

    let should = predictions_for("すべき");
    assert!(
        should
            .iter()
            .all(|(reading, _)| reading.starts_with("すべき")),
        "すべき: a known exact reading triggered unrelated repair predictions: {should:?}"
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn conversion_candidates_do_not_show_bracket_tags() {
    let readings = [
        "きのう",
        "きのうしょうかい",
        "きのうしよう",
        "きのうてすと",
        "あまぞん",
        "amazon",
    ];
    for reading in readings {
        open_conversion()
            .with_candidates(reading, ConversionOptions::default(), |candidates| {
                for candidate in candidates {
                    let annotation = candidate.annotation();
                    assert!(
                        !annotation.starts_with('['),
                        "{reading} / {} shows bracket tag {annotation:?}",
                        candidate.text()
                    );
                }
            })
            .unwrap_or_else(|error| panic!("{reading}: {error}"));
    }
}
