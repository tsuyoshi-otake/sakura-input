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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sakura_core::{
    ConversionInput, ConversionOptions, CrossCommitBridge, InputMethod, InputSupport, Preferences,
    RightContextId, MAX_CROSS_COMMIT_CURRENT_BYTES, MAX_CROSS_COMMIT_TAIL_BYTES,
};
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

fn enter_key() -> KeyInput {
    KeyInput {
        code: KeyCode::Enter,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn create_normal_session(dispatcher: &mut Dispatcher, process_name: &str) -> SessionId {
    let mut out = OutputBuf::new();
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: process_name.to_owned(),
        },
        &mut out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("unexpected session response: {other:?}"),
    };
    assert!(matches!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut OutputBuf::new(),
        ),
        Reply::Message(Response::Ok)
    ));
    session
}

fn send_key(dispatcher: &mut Dispatcher, session: SessionId, key: KeyInput) -> OutputBuf {
    let mut out = OutputBuf::new();
    let reply = dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
    assert!(
        matches!(reply, Reply::Output),
        "unexpected key reply: {reply:?}"
    );
    out
}

fn type_romaji(dispatcher: &mut Dispatcher, session: SessionId, romaji: &str) -> OutputBuf {
    let mut out = OutputBuf::new();
    for ch in romaji.chars() {
        out = send_key(dispatcher, session, char_key(ch));
    }
    out
}

fn commit_converted_romaji(
    dispatcher: &mut Dispatcher,
    session: SessionId,
    romaji: &str,
) -> String {
    type_romaji(dispatcher, session, romaji);
    send_key(dispatcher, session, space_key());
    send_key(dispatcher, session, enter_key())
        .to_output()
        .commit
        .unwrap_or_default()
}

fn commit_named_converted_romaji(
    dispatcher: &mut Dispatcher,
    session: SessionId,
    romaji: &str,
    selected_segment: &str,
) -> String {
    type_romaji(dispatcher, session, romaji);
    let converted = send_key(dispatcher, session, space_key());
    let candidates = candidate_surfaces(&converted);
    let index = candidates
        .iter()
        .position(|candidate| candidate == selected_segment)
        .unwrap_or_else(|| panic!("missing {selected_segment} for {romaji}: {candidates:?}"));
    let mut out = OutputBuf::new();
    let reply = dispatcher.dispatch(
        &Request::CommitCandidate {
            session,
            revision: 0,
            candidate_index: u16::try_from(index).expect("bounded candidate index"),
        },
        &mut out,
    );
    assert!(
        matches!(reply, Reply::Output),
        "unexpected commit reply: {reply:?}"
    );
    out.to_output().commit.unwrap_or_default()
}

fn convert_romaji_in_session(
    dispatcher: &mut Dispatcher,
    session: SessionId,
    romaji: &str,
) -> Vec<String> {
    type_romaji(dispatcher, session, romaji);
    candidate_surfaces(&send_key(dispatcher, session, space_key()))
}

fn candidate_surfaces(out: &OutputBuf) -> Vec<String> {
    out.to_output()
        .candidates
        .map(|list| {
            list.items
                .iter()
                .map(|item| item.text.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
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

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn issue_83_shipped_path_uses_a_costed_typed_frontier() {
    let conversion = open_conversion();
    let prefix_right = conversion
        .with_candidates("こうりょ", ConversionOptions::default(), |candidates| {
            candidates
                .iter()
                .find(|candidate| candidate.text() == "考慮")
                .and_then(|candidate| candidate.segments().last())
                .map(|segment| segment.right_id)
                .expect("考慮 terminal right ID")
        })
        .expect("prefix conversion");
    let (tail_right, tail_prefix_cost) = conversion
        .with_candidates(
            "もれ",
            ConversionOptions {
                initial_right_id: prefix_right,
                ..ConversionOptions::default()
            },
            |candidates| {
                let tail = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "漏れ")
                    .expect("tail");
                (
                    tail.segments().last().expect("tail segment").right_id,
                    conversion
                        .cross_commit_prefix_cost(tail)
                        .expect("one-edge system tail"),
                )
            },
        )
        .expect("tail conversion");
    assert!(tail_prefix_cost > 0);
    conversion
        .with_conversion_input_bridge_hints(
            ConversionInput::ordinary("ないか"),
            ConversionOptions {
                initial_right_id: tail_right,
                ..ConversionOptions::default()
            },
            &[],
            Some(CrossCommitBridge {
                tail_reading: "もれ",
                tail_surface: "漏れ",
                prefix_right_id: RightContextId::new(prefix_right),
                prefix_cost: tail_prefix_cost,
            }),
            |candidates, diagnostics| {
                let surfaces = candidates
                    .iter()
                    .map(|candidate| candidate.text())
                    .collect::<Vec<_>>();
                let plain = surfaces
                    .iter()
                    .position(|surface| *surface == "ないか")
                    .unwrap();
                let negative = surfaces
                    .iter()
                    .position(|surface| *surface == "無いか")
                    .unwrap();
                let clinic = surfaces
                    .iter()
                    .position(|surface| *surface == "内科")
                    .unwrap();
                assert!(plain < negative && negative < clinic, "{surfaces:?}");
                assert_eq!(diagnostics.cross_commit_bridge_spanning_paths, 0);
                assert!(diagnostics.cross_commit_bridge_frontier_paths > 0);
                assert!(diagnostics.cross_commit_bridge_candidates_rescored >= 2);
            },
        )
        .expect("bridged conversion");
}

#[test]
#[ignore = "release-only Issue #83 full-conversion percentile benchmark"]
fn issue_83_cross_commit_bridge_release_percentiles() {
    if cfg!(debug_assertions) {
        panic!("timing evidence must be collected with --release");
    }
    const WARMUP: usize = 500;
    const SAMPLES: usize = 5_000;
    const BUDGET: Duration = Duration::from_millis(20);

    let conversion = open_conversion();
    let prefix_right = conversion
        .with_candidates("こうりょ", ConversionOptions::default(), |candidates| {
            candidates
                .iter()
                .find(|candidate| candidate.text() == "考慮")
                .and_then(|candidate| candidate.segments().last())
                .map(|segment| segment.right_id)
                .expect("考慮 terminal right ID")
        })
        .expect("prefix conversion");
    let (tail_right, tail_prefix_cost) = conversion
        .with_candidates(
            "もれ",
            ConversionOptions {
                initial_right_id: prefix_right,
                ..ConversionOptions::default()
            },
            |candidates| {
                let tail = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "漏れ")
                    .expect("tail");
                (
                    tail.segments().last().expect("tail segment").right_id,
                    conversion
                        .cross_commit_prefix_cost(tail)
                        .expect("one-edge system tail"),
                )
            },
        )
        .expect("tail conversion");
    let target_options = ConversionOptions {
        initial_right_id: tail_right,
        ..ConversionOptions::default()
    };
    let target_bridge = CrossCommitBridge {
        tail_reading: "もれ",
        tail_surface: "漏れ",
        prefix_right_id: RightContextId::new(prefix_right),
        prefix_cost: tail_prefix_cost,
    };

    let max_tail = "あ".repeat(MAX_CROSS_COMMIT_TAIL_BYTES / "あ".len());
    let max_current = "あ".repeat(MAX_CROSS_COMMIT_CURRENT_BYTES / "あ".len());
    assert_eq!(max_tail.len(), MAX_CROSS_COMMIT_TAIL_BYTES);
    assert_eq!(max_current.len(), MAX_CROSS_COMMIT_CURRENT_BYTES);
    let max_bridge = CrossCommitBridge {
        tail_reading: &max_tail,
        tail_surface: &max_tail,
        prefix_right_id: RightContextId::new(0),
        prefix_cost: 0,
    };

    for _ in 0..WARMUP {
        run_timed_conversion(&conversion, "ないか", target_options, None);
        run_timed_conversion(&conversion, "ないか", target_options, Some(target_bridge));
        run_timed_conversion(
            &conversion,
            &max_current,
            ConversionOptions::default(),
            Some(max_bridge),
        );
    }

    let mut absent = Vec::with_capacity(SAMPLES);
    let mut present = Vec::with_capacity(SAMPLES);
    let mut incremental = Vec::with_capacity(SAMPLES);
    let mut maximum = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let (without, with) = if index % 2 == 0 {
            (
                run_timed_conversion(&conversion, "ないか", target_options, None),
                run_timed_conversion(&conversion, "ないか", target_options, Some(target_bridge)),
            )
        } else {
            let with =
                run_timed_conversion(&conversion, "ないか", target_options, Some(target_bridge));
            let without = run_timed_conversion(&conversion, "ないか", target_options, None);
            (without, with)
        };
        absent.push(without);
        present.push(with);
        incremental.push(with.saturating_sub(without));
        maximum.push(run_timed_conversion(
            &conversion,
            &max_current,
            ConversionOptions::default(),
            Some(max_bridge),
        ));
    }

    let absent_report = percentiles(&mut absent);
    let present_report = percentiles(&mut present);
    let incremental_report = percentiles(&mut incremental);
    let maximum_report = percentiles(&mut maximum);
    println!(
        "Issue #83 release benchmark ({SAMPLES} samples after {WARMUP} warmups):\n\
         current-only full conversion: {absent_report}\n\
         target bridge full conversion: {present_report}\n\
         paired bridge increment proxy: {incremental_report}\n\
         max 48-byte tail + 96-byte current full conversion: {maximum_report}"
    );
    assert!(
        present_report.p99 < BUDGET,
        "target bridge p99 {:?} exceeds {BUDGET:?}",
        present_report.p99
    );
    assert!(
        maximum_report.p99 < BUDGET,
        "maximum-bound bridge p99 {:?} exceeds {BUDGET:?}",
        maximum_report.p99
    );
}

fn run_timed_conversion(
    conversion: &sakura_engine::dictionary::ConversionService,
    reading: &str,
    options: ConversionOptions,
    bridge: Option<CrossCommitBridge<'_>>,
) -> Duration {
    let start = Instant::now();
    conversion
        .with_conversion_input_bridge_hints(
            ConversionInput::ordinary(reading),
            options,
            &[],
            bridge,
            |candidates, diagnostics| {
                std::hint::black_box((
                    candidates.first().map(|candidate| candidate.cost),
                    diagnostics.cross_commit_bridge_candidates_examined,
                ));
            },
        )
        .expect("benchmark conversion");
    start.elapsed()
}

#[derive(Clone, Copy)]
struct PercentileReport {
    p50: Duration,
    p99: Duration,
    max: Duration,
}

impl std::fmt::Display for PercentileReport {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "p50={:.3}ms p99={:.3}ms max={:.3}ms",
            self.p50.as_secs_f64() * 1_000.0,
            self.p99.as_secs_f64() * 1_000.0,
            self.max.as_secs_f64() * 1_000.0
        )
    }
}

fn percentiles(samples: &mut [Duration]) -> PercentileReport {
    samples.sort_unstable();
    PercentileReport {
        p50: samples[samples.len() / 2],
        p99: samples[samples.len() * 99 / 100],
        max: samples[samples.len() - 1],
    }
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn committed_kouryo_more_makes_naika_continue_as_a_grammar_phrase() {
    let mut dispatcher = open_dispatcher();
    let session = create_normal_session(&mut dispatcher, "issue-83-cross-commit.exe");

    let composed = type_romaji(&mut dispatcher, session, "kouryomore");
    let reading = composed
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
    assert_eq!(reading, "こうりょもれ");

    let first_conversion = send_key(&mut dispatcher, session, space_key());
    let rendered = first_conversion
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
    assert_eq!(
        rendered, "考慮漏れ",
        "the setup must commit the exact reported left context"
    );
    let committed = send_key(&mut dispatcher, session, enter_key());
    assert_eq!(committed.to_output().commit.as_deref(), Some("考慮漏れ"));

    let second_composed = type_romaji(&mut dispatcher, session, "naika");
    let second_reading = second_composed
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
    assert_eq!(second_reading, "ないか");

    let second_conversion = send_key(&mut dispatcher, session, space_key());
    let candidates = candidate_surfaces(&second_conversion);
    let plain = candidates
        .iter()
        .position(|surface| surface == "ないか")
        .unwrap_or_else(|| panic!("ないか disappeared: {candidates:?}"));
    let negative = candidates
        .iter()
        .position(|surface| surface == "無いか")
        .unwrap_or_else(|| panic!("無いか disappeared: {candidates:?}"));
    let clinic = candidates
        .iter()
        .position(|surface| surface == "内科")
        .unwrap_or_else(|| panic!("内科 disappeared: {candidates:?}"));
    assert!(
        plain < clinic && negative < clinic,
        "考慮漏れ + ないか must prefer grammatical continuations over 内科: {candidates:?}"
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn issue_84_recent_cached_naika_selection_cannot_override_kouryo_more_context() {
    let mut dispatcher = open_dispatcher();
    let session = create_normal_session(&mut dispatcher, "issue-84-reproduction.exe");

    assert_eq!(
        commit_converted_romaji(&mut dispatcher, session, "kyouha"),
        "今日は"
    );
    assert_eq!(
        commit_named_converted_romaji(&mut dispatcher, session, "naika", "内科"),
        "内科",
        "setup must reproduce the user's explicit homophone selection"
    );
    assert_eq!(
        commit_converted_romaji(&mut dispatcher, session, "kouryomore"),
        "考慮漏れ"
    );

    type_romaji(&mut dispatcher, session, "naika");
    let converted = send_key(&mut dispatcher, session, space_key());
    let selected = converted
        .to_output()
        .preedit
        .map(|preedit| {
            preedit
                .segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
    let candidates = candidate_surfaces(&converted);
    let clinic = candidates
        .iter()
        .position(|surface| surface == "内科")
        .expect("内科 control candidate");
    let grammatical = candidates
        .iter()
        .position(|surface| surface == "ないか" || surface == "無いか")
        .expect("grammatical continuation");

    assert_ne!(selected, "内科", "cached selection leaked across context");
    assert!(
        grammatical < clinic,
        "bridge-supported continuation must outrank cached 内科: {candidates:?}"
    );
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn issue_83_shipped_japanese_bridge_controls() {
    let conversion = open_conversion();
    let (tail_right, tail) = conversion
        .with_candidates(
            "きさいもれ",
            ConversionOptions::default(),
            |candidates| {
                let candidate = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "記載漏れ")
                    .expect("記載漏れ candidate");
                assert!(candidate.system_entry_index().is_some());
                (
                    candidate.segments().last().expect("tail segment").right_id,
                    conversion
                        .cross_commit_tail(candidate)
                        .expect("tail evidence"),
                )
            },
        )
        .expect("記載漏れ evidence");
    assert_eq!(tail.reading_start, 0, "記載漏れ is one selected raw edge");
    let options = ConversionOptions {
        initial_right_id: tail_right,
        ..ConversionOptions::default()
    };
    let direct = conversion
        .with_candidates("ないか", options, relevant_naika_costs)
        .expect("記載漏れ current-only costs");
    let (bridged, diagnostics) = conversion
        .with_conversion_input_bridge_hints(
            ConversionInput::ordinary("ないか"),
            options,
            &[],
            Some(CrossCommitBridge {
                tail_reading: "きさいもれ",
                tail_surface: "記載漏れ",
                prefix_right_id: tail.prefix_right_id,
                prefix_cost: tail.prefix_cost,
            }),
            |candidates, diagnostics| (relevant_naika_costs(candidates), diagnostics),
        )
        .expect("記載漏れ bridge");
    assert!(bridged[0] < direct[0], "ないか receives lexical evidence");
    assert!(bridged[1] < direct[1], "無いか receives lexical evidence");
    assert_eq!(bridged[2], direct[2], "内科 is not context-boosted");
    assert!(
        bridged[2] < bridged[0],
        "the selected atomic 記載漏れ path's retained reanalysis cost must not be erased"
    );
    assert!(diagnostics.cross_commit_bridge_frontier_paths > 0);

    for (romaji, expected_commit) in [("jouhoumore", "情報漏れ"), ("jouhougamore", "情報が漏れ")]
    {
        let mut dispatcher = open_dispatcher();
        let session = create_normal_session(&mut dispatcher, "issue-83-japanese-controls.exe");
        let committed = commit_converted_romaji(&mut dispatcher, session, romaji);
        assert_eq!(committed, expected_commit, "control setup for {romaji}");
        let candidates = convert_romaji_in_session(&mut dispatcher, session, "naika");
        let plain = candidates.iter().position(|item| item == "ないか").unwrap();
        let negative = candidates.iter().position(|item| item == "無いか").unwrap();
        let clinic = candidates.iter().position(|item| item == "内科").unwrap();
        assert!(
            plain < negative && negative < clinic,
            "{expected_commit} must use the generic grammatical bridge: {candidates:?}"
        );
    }
}

fn relevant_naika_costs(candidates: &[sakura_core::ConversionCandidate]) -> [i64; 3] {
    ["ないか", "無いか", "内科"].map(|surface| {
        candidates
            .iter()
            .find(|candidate| candidate.text() == surface)
            .unwrap_or_else(|| panic!("missing {surface}"))
            .cost
    })
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn issue_84_bridge_bounds_conflicting_learning_but_preserves_other_authorities() {
    let mut baseline_dispatcher = open_dispatcher();
    let (_, baseline) = convert(&mut baseline_dispatcher, "naika");
    assert!(!baseline.is_empty(), "fresh current-only conversion");
    assert_eq!(baseline.first().map(String::as_str), Some("ないか"));

    let conversion = open_conversion();
    let association_off_it_bias = conversion
        .with_candidates(
            "こうりょもれ",
            ConversionOptions::default(),
            |candidates| {
                let committed = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "考慮漏れ")
                    .expect("考慮漏れ candidate");
                let (it_words, total_words) =
                    committed
                        .segments()
                        .iter()
                        .fold((0u16, 0u16), |(it, total), segment| {
                            (
                                it.saturating_add(u16::from(segment.it_word_count)),
                                total.saturating_add(u16::from(segment.word_count)),
                            )
                        });
                let ratio = it_words
                    .saturating_mul(1_000)
                    .checked_div(total_words)
                    .unwrap_or(0);
                100u16.saturating_add((ratio / 5).min(150))
            },
        )
        .expect("association-off domain ratio");
    let expected_disabled = conversion
        .with_candidates(
            "ないか",
            ConversionOptions {
                initial_right_id: 0,
                it_bias_per_mille: association_off_it_bias,
                ..ConversionOptions::default()
            },
            |candidates| {
                candidates
                    .iter()
                    .map(|candidate| candidate.text().to_owned())
                    .collect::<Vec<_>>()
            },
        )
        .expect("association-off current-only conversion");
    let association_off = Preferences {
        association_enabled: false,
        ..Preferences::default()
    };
    let mut disabled = Dispatcher::new_with_configuration(
        Arc::clone(&conversion),
        Arc::new(LearningService::memory()),
        association_off,
    )
    .expect("association-off dispatcher");
    let disabled_session = create_normal_session(&mut disabled, "issue-83-disabled.exe");
    assert_eq!(
        commit_converted_romaji(&mut disabled, disabled_session, "kouryomore"),
        "考慮漏れ"
    );
    let disabled_candidates = convert_romaji_in_session(&mut disabled, disabled_session, "naika");
    assert_eq!(
        disabled_candidates, expected_disabled,
        "association off must retain the complete current-only order at the existing domain bias"
    );

    let left_context = conversion
        .with_candidates(
            "こうりょもれ",
            ConversionOptions::default(),
            |candidates| {
                let committed = candidates
                    .iter()
                    .find(|candidate| candidate.text() == "考慮漏れ")
                    .expect("考慮漏れ candidate");
                committed
                    .segments()
                    .last()
                    .expect("committed tail")
                    .right_id
            },
        )
        .expect("context conversion");
    let clinic_right = conversion
        .with_candidates(
            "ないか",
            ConversionOptions {
                initial_right_id: left_context,
                ..ConversionOptions::default()
            },
            |current| {
                current
                    .iter()
                    .find(|candidate| candidate.text() == "内科")
                    .and_then(|candidate| candidate.segments().last())
                    .map(|segment| segment.right_id)
                    .expect("内科 right ID")
            },
        )
        .expect("current conversion");
    let learning_root = std::env::temp_dir().join(format!(
        "sakura-issue-84-learning-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&learning_root).expect("create durable learning root");
    let learning_path = learning_root.join("learning.log");
    {
        let durable = LearningService::open(&learning_path).expect("open durable learning");
        for _ in 0..2 {
            durable.learn("ないか", "内科", left_context, clinic_right);
        }
        durable.maintain().expect("flush durable learning");
    }
    let learning = Arc::new(
        LearningService::open(&learning_path).expect("reopen persisted learned homophone"),
    );
    let mut learned =
        Dispatcher::new_with_configuration(conversion, learning, Preferences::default())
            .expect("learned dispatcher");
    let learned_session = create_normal_session(&mut learned, "issue-83-learned.exe");
    assert_eq!(
        commit_converted_romaji(&mut learned, learned_session, "kouryomore"),
        "考慮漏れ"
    );
    type_romaji(&mut learned, learned_session, "naika");
    let learned_output = send_key(&mut learned, learned_session, space_key());
    let learned_surface = learned_output
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
    assert_eq!(
        learned_surface, "無いか",
        "even strong learned homophones must stay inside the bridge-supported continuation set"
    );
    drop(learned);
    std::fs::remove_dir_all(learning_root).expect("remove durable learning root");
}

#[test]
#[ignore = "needs the built system dictionary in artifacts/release"]
fn issue_83_medical_particle_contexts_keep_the_current_only_naika_order() {
    for (romaji, reading, selected_segment, expected) in [
        ("shinnryoukaha", "しんりょうかは", "診療科は", "診療科は"),
        (
            "jushinnsakihasougoubyouinnno",
            "じゅしんさきはそうごうびょういんの",
            "受診",
            "受診先は総合病院の",
        ),
    ] {
        let conversion = open_conversion();
        let prior_right = conversion
            .with_candidates(reading, ConversionOptions::default(), |candidates| {
                candidates
                    .iter()
                    .find(|candidate| candidate.text() == expected)
                    .and_then(|candidate| candidate.segments().last())
                    .map(|segment| segment.right_id)
                    .expect("medical control right ID")
            })
            .expect("medical control conversion");
        let current_only = conversion
            .with_candidates(
                "ないか",
                ConversionOptions {
                    initial_right_id: prior_right,
                    ..ConversionOptions::default()
                },
                |candidates| {
                    candidates
                        .iter()
                        .map(|candidate| candidate.text().to_owned())
                        .collect::<Vec<_>>()
                },
            )
            .expect("current-only medical conversion");

        let mut dispatcher =
            Dispatcher::new_with_conversion(conversion).expect("medical dispatcher");
        let session = create_normal_session(&mut dispatcher, "issue-83-medical.exe");
        let committed =
            commit_named_converted_romaji(&mut dispatcher, session, romaji, selected_segment);
        assert_eq!(committed, expected, "medical control setup: {romaji}");
        let candidates = convert_romaji_in_session(&mut dispatcher, session, "naika");
        assert_eq!(
            candidates, current_only,
            "a particle-ending medical context must retain its current-only order: {expected}"
        );
    }
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
