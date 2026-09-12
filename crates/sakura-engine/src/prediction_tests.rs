use super::*;
use sakura_core::{
    ConversionOptions, UserDictionaryEntry, UserPartOfSpeech, MAX_USER_DICTIONARY_ENTRIES,
};

#[test]
fn mapped_entry_index_stays_compact() {
    assert_eq!(core::mem::size_of::<IndexedEntry>(), 12);
}

fn conversion() -> Arc<ConversionService> {
    prediction_fixture_conversion(
            "かな\t仮名\t0\t0\t100\t100\tpredict\tcommon\nかんすう\t関数\t0\t0\t200\t50\tit,predict\ttechnical\nかんじ\t感じ\t0\t0\t50\t-\t\tnot predictive\n",
        )
}

fn prediction_fixture_conversion(rows: &str) -> Arc<ConversionService> {
    let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
    source.push_str(rows);
    let entries = dictc::parse_entries("fixture.tsv", &source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let bytes = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("compile")
            .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(bytes).expect("service"))
}

fn prediction_query(prefix: &str) -> Query {
    let mut fixed_prefix = FixedStr::new();
    fixed_prefix.push_str(prefix).expect("prediction prefix");
    Query {
        sequence: 1,
        session: 1,
        generation: 1,
        prefix: fixed_prefix,
        domain_it_per_mille: 0,
        input_support: InputSupport::default(),
        skip_input_repair: false,
        allow_spelling_correction: true,
    }
}

fn predict_fixture(
    conversion: &Arc<ConversionService>,
    prefix: &str,
    history: Option<&LearningService>,
) -> PredictionResult {
    let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
    let user_dictionary = conversion.user_dictionary_snapshot();
    let query = prediction_query(prefix);
    let mut result = PredictionResult::default();
    index.predict_into(&query, user_dictionary.as_ref(), history, &mut result);
    result
}

fn capacity_user_dictionary(prefix: &str) -> UserDictionary {
    const HIRAGANA_DIGITS: [char; 10] =
        ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];

    let entries = (0..MAX_USER_DICTIONARY_ENTRIES)
        .map(|number| {
            let mut reading = prefix.to_owned();
            let mut value = number;
            for _ in 0..4 {
                reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                value /= HIRAGANA_DIGITS.len();
            }
            UserDictionaryEntry {
                reading,
                surface: if number < 2 {
                    "shared".to_owned()
                } else {
                    format!("user-{number:04}")
                },
                part_of_speech: UserPartOfSpeech::Noun,
                comment: String::new(),
            }
        })
        .collect();
    UserDictionary::from_entries(entries).expect("capacity user dictionary")
}

fn benchmark_user_dictionary(size: usize, matching_entries: usize) -> UserDictionary {
    const HIRAGANA_DIGITS: [char; 10] =
        ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];

    assert!(matching_entries <= size);
    let entries = (0..size)
        .map(|number| {
            let (prefix, mut value) = if number < matching_entries {
                ("さ", number)
            } else {
                ("あ", number - matching_entries)
            };
            let mut reading = prefix.to_owned();
            for _ in 0..4 {
                reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                value /= HIRAGANA_DIGITS.len();
            }
            UserDictionaryEntry {
                reading,
                surface: format!("bench-{number:05}"),
                part_of_speech: UserPartOfSpeech::Noun,
                comment: String::new(),
            }
        })
        .collect();
    UserDictionary::from_entries(entries).expect("benchmark user dictionary")
}

fn nanos_each(mut body: impl FnMut()) -> f64 {
    const ROUNDS: usize = 1_000;

    for _ in 0..ROUNDS / 10 {
        body();
    }
    let started = Instant::now();
    for _ in 0..ROUNDS {
        body();
    }
    started.elapsed().as_secs_f64() * 1e9 / ROUNDS as f64
}

fn sample_nanos(mut body: impl FnMut()) -> Vec<u128> {
    const WARMUP: usize = 200;
    const SAMPLES: usize = 2_000;

    for _ in 0..WARMUP {
        body();
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        body();
        samples.push(started.elapsed().as_nanos());
    }
    samples
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    assert!(!samples.is_empty());
    assert!(percentile <= 100);
    samples.sort_unstable();
    let index = ((samples.len() - 1) * percentile).div_ceil(100);
    samples[index]
}

#[test]
#[ignore = "timing evaluation: run with --release --ignored --nocapture and record the table"]
fn prediction_latency_percentiles() {
    const HIRAGANA_DIGITS: [char; 10] = [
        '\u{3042}', '\u{3044}', '\u{3046}', '\u{3048}', '\u{304A}', '\u{304B}', '\u{304D}',
        '\u{304F}', '\u{3051}', '\u{3053}',
    ];
    let conversion = conversion();
    let entries = (0..1_000)
        .map(|number| {
            let mut value = number;
            let mut reading = String::from('\u{3055}');
            for _ in 0..4 {
                reading.push(HIRAGANA_DIGITS[value % HIRAGANA_DIGITS.len()]);
                value /= HIRAGANA_DIGITS.len();
            }
            UserDictionaryEntry {
                reading,
                surface: format!("candidate-{number:04}"),
                part_of_speech: UserPartOfSpeech::Noun,
                comment: String::new(),
            }
        })
        .collect();
    let user_dictionary = UserDictionary::from_entries(entries).expect("user dictionary");
    conversion.replace_user_dictionary(user_dictionary);
    let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
    let mut prefix = FixedStr::new();
    prefix.push_str("\u{3055}").expect("benchmark prefix");
    let query = Query {
        sequence: 1,
        session: 1,
        generation: 1,
        prefix,
        domain_it_per_mille: 1_000,
        input_support: InputSupport::default(),
        skip_input_repair: false,
        allow_spelling_correction: true,
    };
    let user_dictionary = conversion.user_dictionary_snapshot();

    let mut ranked = PredictionResult::default();
    let mut ranking = sample_nanos(|| {
        index.predict_into(&query, user_dictionary.as_ref(), None, &mut ranked);
        std::hint::black_box(ranked.candidates().len());
    });

    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
    let service = runtime.service();
    let mut worker_result = PredictionResult::default();
    let mut generation = 1u64;
    let mut worker = sample_nanos(|| {
        generation += 1;
        assert!(service.request_into(
            1,
            generation,
            query.prefix.as_str(),
            query.domain_it_per_mille,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_secs(1),
            &mut worker_result,
        ));
        std::hint::black_box(worker_result.candidates().len());
    });
    runtime.stop().expect("joined worker");

    println!("prediction latency percentiles: 2,000 samples, release");
    println!("path       p50 ns  p95 ns  p99 ns  max ns");
    for (name, samples) in [("ranking", &mut ranking), ("worker", &mut worker)] {
        println!(
            "{name:8} {:>7} {:>7} {:>7} {:>7}",
            percentile(samples, 50),
            percentile(samples, 95),
            percentile(samples, 99),
            percentile(samples, 100),
        );
    }
}

#[test]
fn persistent_worker_merges_system_and_user_predictions_and_joins() {
    let conversion = conversion();
    conversion.replace_user_dictionary(
        UserDictionary::parse_tsv("reading\tsurface\tpos\tcomment\nかなた\t彼方\tnoun\tuser\n")
            .expect("user dictionary"),
    );
    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
    let service = runtime.service();

    let result = service
        .request(
            7,
            3,
            "かな",
            1_000,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_millis(100),
        )
        .expect("result");

    assert_eq!(result.session(), 7);
    assert_eq!(result.generation(), 3);
    assert_eq!(
        result
            .candidates()
            .iter()
            .map(PredictionCandidate::surface)
            .collect::<Vec<_>>(),
        ["彼方", "仮名"]
    );
    runtime.stop().expect("joined worker");
}

#[test]
fn capacity_user_dictionary_preserves_prediction_order_deduplication_and_limit() {
    let conversion = conversion();
    let user_dictionary = capacity_user_dictionary("か");
    let mut expected = Vec::new();
    for entry in user_dictionary.entries() {
        if !expected.iter().any(|surface| surface == &entry.surface) {
            expected.push(entry.surface.clone());
        }
        if expected.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    conversion.replace_user_dictionary(user_dictionary);

    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
    let result = runtime
        .service()
        .request(
            8,
            5,
            "か",
            1_000,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_secs(1),
        )
        .expect("capacity prediction result");

    assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
    assert!(result
        .candidates()
        .iter()
        .all(|candidate| candidate.source() == PredictionSource::User));
    assert_eq!(
        result
            .candidates()
            .iter()
            .map(|candidate| candidate.surface())
            .collect::<Vec<_>>(),
        expected
    );
    runtime.stop().expect("joined worker");
}

#[test]
#[ignore = "timing evaluation: run with --release --ignored --nocapture and record the table"]
fn user_dictionary_prediction_evaluation() {
    const SIZES: [usize; 4] = [0, 100, 1_000, MAX_USER_DICTIONARY_ENTRIES];
    const QUERIES: [(&str, usize); 5] = [
        ("no-match", 0),
        ("one-match", 1),
        ("nine-match", 9),
        ("hundred-match", 100),
        ("ten-thousand-match", MAX_USER_DICTIONARY_ENTRIES),
    ];

    let conversion = conversion();
    let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
    let mut query_prefix = FixedStr::new();
    query_prefix.push_str("さ").expect("benchmark prefix");
    let query = Query {
        sequence: 1,
        session: 1,
        generation: 1,
        prefix: query_prefix,
        domain_it_per_mille: 1_000,
        input_support: InputSupport::default(),
        skip_input_repair: false,
        allow_spelling_correction: true,
    };

    println!("user dictionary prediction evaluation: 1,000 rounds per row");
    println!("entries  query                matches  search ns  ranking ns  worker ns");
    for size in SIZES {
        for (name, matching_entries) in QUERIES {
            if matching_entries > size {
                continue;
            }
            let user_dictionary = benchmark_user_dictionary(size, matching_entries);
            let search_ns = nanos_each(|| {
                let mut visited = 0usize;
                user_dictionary.predictive_search("さ", |_| {
                    visited += 1;
                    true
                });
                std::hint::black_box(visited);
            });

            let mut ranked = PredictionResult::default();
            let ranking_ns = nanos_each(|| {
                index.predict_into(&query, &user_dictionary, None, &mut ranked);
                std::hint::black_box(ranked.candidates().len());
            });

            conversion.replace_user_dictionary(user_dictionary);
            let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
            let service = runtime.service();
            let mut worker_result = PredictionResult::default();
            let mut generation = 1u64;
            let worker_ns = nanos_each(|| {
                generation += 1;
                assert!(service.request_into(
                    1,
                    generation,
                    "さ",
                    1_000,
                    sakura_core::InputSupport::default(),
                    false,
                    Duration::from_secs(1),
                    &mut worker_result,
                ));
                std::hint::black_box(worker_result.candidates().len());
            });
            runtime.stop().expect("joined worker");

            println!(
                    "{size:7}  {name:19}  {matching_entries:7}  {search_ns:9.1}  {ranking_ns:10.1}  {worker_ns:9.1}"
                );
        }
    }
}

#[test]
fn learned_history_precedes_dictionary_results_and_deduplicates_surfaces() {
    let conversion = conversion();
    let learning = Arc::new(LearningService::memory());
    learning.learn("かなた", "彼方", 0, 7);
    learning.learn("かな", "仮名", 0, 3);
    learning.learn("かな", "仮名", 0, 3);
    let runtime =
        PredictionRuntime::start_with_learning(Arc::clone(&conversion), Arc::clone(&learning))
            .expect("runtime");

    let result = runtime
        .service()
        .request(
            9,
            4,
            "か",
            1_000,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_millis(100),
        )
        .expect("result");

    assert_eq!(result.candidates()[0].surface(), "仮名");
    assert_eq!(result.candidates()[0].annotation(), "履歴");
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface() == "仮名")
            .count(),
        1
    );
    assert!(result
        .candidates()
        .iter()
        .any(|candidate| candidate.surface() == "関数"));
    runtime.stop().expect("joined worker");
}

#[test]
fn direct_candidates_are_not_evicted_by_a_cheaper_advanced_repair() {
    let conversion = prediction_fixture_conversion(
            "かが\t直接候補\t0\t0\t5000\t5000\tpredict\tdirect\nいが\t高度補正\t0\t0\t1\t1000\tpredict\tadvanced\n",
        );
    let result = predict_fixture(&conversion, "かが", None);

    assert_eq!(result.candidates()[0].surface(), "直接候補");
    assert_eq!(result.candidates()[0].reading(), "かが");
    assert!(result
        .candidates()
        .iter()
        .any(|candidate| candidate.surface() == "高度補正"));
}

#[test]
fn word_sized_exact_reading_outranks_a_cheaper_longer_completion() {
    let conversion = prediction_fixture_conversion(
        "おらくる\tオラクル\t0\t0\t5000\t5000\tpredict\texact\n\
おらくるさぽーと\tOracleサポート\t0\t0\t4000\t4000\tpredict\tcompletion\n",
    );

    let result = predict_fixture(&conversion, "おらくる", None);

    assert_eq!(result.candidates()[0].surface(), "オラクル");
    assert!(result
        .candidates()
        .iter()
        .any(|candidate| candidate.surface() == "Oracleサポート"));
}

#[test]
fn a_known_non_predictive_word_does_not_trigger_reading_repairs() {
    let conversion = prediction_fixture_conversion(
        "すべき\tすべき\t0\t0\t7000\t-\t\tknown word\n\
すぺきゅれーしょんるーるず\t投機的ルール\t0\t0\t100\t100\tpredict\trepair trap\n",
    );

    let result = predict_fixture(&conversion, "すべき", None);

    assert!(
        result.candidates().is_empty(),
        "known exact word exposed repaired predictions: {:?}",
        result
            .candidates()
            .iter()
            .map(|candidate| (candidate.reading(), candidate.surface()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_repair_only_unlocks_local_completions() {
    let conversion = prediction_fixture_conversion(
        "すぺきあ\t近い補正\t0\t0\t200\t200\tpredict\tlocal repair\n\
すぺきゅれーしょんるーるず\t投機的ルール\t0\t0\t100\t100\tpredict\tdistant repair trap\n",
    );

    let result = predict_fixture(&conversion, "すべき", None);
    let surfaces = result
        .candidates()
        .iter()
        .map(PredictionCandidate::surface)
        .collect::<Vec<_>>();

    assert!(
        surfaces.contains(&"近い補正"),
        "local repair was lost: {surfaces:?}"
    );
    assert!(
        !surfaces.contains(&"投機的ルール"),
        "a repair unlocked a disproportionate completion: {surfaces:?}"
    );
}

#[test]
fn an_exact_history_word_does_not_trigger_reading_repairs() {
    let conversion =
        prediction_fixture_conversion("すぺきあ\t近い補正\t0\t0\t200\t200\tpredict\trepair trap\n");
    let learning = LearningService::memory();
    learning.learn("すべき", "すべき", 0, 1);

    let result = predict_fixture(&conversion, "すべき", Some(&learning));
    assert_eq!(result.candidates()[0].surface(), "すべき");
    assert!(
        result
            .candidates()
            .iter()
            .all(|candidate| candidate.reading().starts_with("すべき")),
        "an exact history word exposed repaired predictions: {:?}",
        result
            .candidates()
            .iter()
            .map(|candidate| (candidate.reading(), candidate.surface()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn completion_distance_is_inactive_for_one_character_prefixes() {
    assert_eq!(completion_cost("か", "か"), 0);
    assert_eq!(completion_cost("か", "かなた"), 0);
}

#[test]
fn decimal_counter_predictions_never_lookup_kana_repair_prefixes() {
    let conversion = prediction_fixture_conversion(
        "あかい\t赤い\t0\t0\t1\t1\tpredict\trepair trap\n\
あかい\t赤井\t0\t0\t2\t2\tpredict\trepair trap\n\
あかい\t紅い\t0\t0\t3\t3\tpredict\trepair trap\n",
    );
    let learning = LearningService::memory();
    learning.learn("2かいひょう", "2回表", 0, 2);
    learning.learn("5かい", "5回", 0, 5);

    for (prefix, expected_history) in [("2かい", "2回表"), ("5かい", "5回")] {
        let result = predict_fixture(&conversion, prefix, Some(&learning));
        let candidates = result.candidates();
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.surface() == expected_history),
            "{prefix} lost its history candidate: {:?}",
            candidates
                .iter()
                .map(PredictionCandidate::surface)
                .collect::<Vec<_>>()
        );
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.reading().starts_with(prefix)),
            "{prefix} looked up an unrelated kana repair: {:?}",
            candidates
                .iter()
                .map(|candidate| (candidate.reading(), candidate.surface()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn repair_ranking_uses_each_variant_penalty() {
    let conversion = prediction_fixture_conversion(
            "かが\t直接候補\t0\t0\t5000\t5000\tpredict\tdirect\nかか\t規則補正\t0\t0\t1\t3000\tpredict\trule\nいが\t高度補正\t0\t0\t1\t2500\tpredict\tadvanced\n",
        );
    let result = predict_fixture(&conversion, "かが", None);
    let surfaces: Vec<&str> = result
        .candidates()
        .iter()
        .map(PredictionCandidate::surface)
        .collect();

    assert_eq!(&surfaces[..3], ["直接候補", "規則補正", "高度補正"]);
}

#[test]
fn direct_system_provenance_wins_a_repair_surface_collision() {
    let conversion = prediction_fixture_conversion(
            "かが\t共有表面\t0\t0\t5000\t5000\tpredict\tdirect\nいが\t共有表面\t0\t0\t1\t1\tpredict\tadvanced\n",
        );
    let result = predict_fixture(&conversion, "かが", None);

    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface() == "共有表面")
            .count(),
        1
    );
    let shared = result
        .candidates()
        .iter()
        .find(|candidate| candidate.surface() == "共有表面")
        .expect("shared candidate");
    assert_eq!(shared.reading(), "かが");
    assert_eq!(shared.source(), PredictionSource::System);
}

#[test]
fn independent_prediction_omits_non_initial_dictionary_fragments() {
    let conversion = prediction_fixture_conversion(
            "ずかい\t使い\t0\t0\t100\t100\tpredict,non-initial\tfragment\nずかい\t図解\t0\t0\t1000\t1000\tpredict\tword\nつかい\t使い\t0\t0\t100\t100\tpredict\tword\n",
        );

    let voiced = predict_fixture(&conversion, "ず", None);
    assert!(voiced
        .candidates()
        .iter()
        .any(|candidate| candidate.surface() == "図解"));
    assert!(voiced
        .candidates()
        .iter()
        .all(|candidate| candidate.surface() != "使い"));

    let ordinary = predict_fixture(&conversion, "つ", None);
    assert!(ordinary
        .candidates()
        .iter()
        .any(|candidate| candidate.surface() == "使い"));
}

#[test]
fn direct_user_provenance_wins_over_a_system_repair() {
    let conversion =
        prediction_fixture_conversion("いが\tシステム補正\t0\t0\t1\t1000\tpredict\tadvanced\n");
    conversion.replace_user_dictionary(
        UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\nかが\tユーザー直接\tnoun\tdirect\n",
        )
        .expect("user dictionary"),
    );
    let result = predict_fixture(&conversion, "かが", None);

    assert_eq!(result.candidates()[0].surface(), "ユーザー直接");
    assert_eq!(result.candidates()[0].source(), PredictionSource::User);
}

#[test]
fn input_repair_never_looks_up_user_dictionary_entries() {
    let conversion = prediction_fixture_conversion("");
    conversion.replace_user_dictionary(
        UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\nいが\tユーザー補正\tnoun\trepair-only\n",
        )
        .expect("user dictionary"),
    );

    let result = predict_fixture(&conversion, "かが", None);

    assert!(result
        .candidates()
        .iter()
        .all(|candidate| candidate.surface() != "ユーザー補正"));
}

#[test]
fn direct_unique_surfaces_survive_more_than_the_ranked_scratch_window() {
    const KANA: [char; 10] = ['あ', 'い', 'う', 'え', 'お', 'か', 'き', 'く', 'け', 'こ'];
    let mut rows = String::new();
    let mut reading_index = 0usize;
    for first in KANA {
        for second in KANA {
            if reading_index >= 36 {
                break;
            }
            rows.push_str(&format!(
                "か{first}{second}\t共有表面\t0\t0\t{}\t{}\tpredict\tduplicate\n",
                reading_index + 1,
                reading_index + 1
            ));
            reading_index += 1;
        }
        if reading_index >= 36 {
            break;
        }
    }
    for unique in 0..8 {
        rows.push_str(&format!(
            "かさ{unique}\t直接候補{unique}\t0\t0\t{}\t{}\tpredict\tdirect\n",
            100 + unique,
            100 + unique
        ));
    }

    let conversion = prediction_fixture_conversion(&rows);
    let result = predict_fixture(&conversion, "か", None);

    assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface() == "共有表面")
            .count(),
        1
    );
    for unique in 0..8 {
        let surface = format!("直接候補{unique}");
        assert!(
            result
                .candidates()
                .iter()
                .any(|candidate| candidate.surface() == surface),
            "{surface} was lost: {:?}",
            result
                .candidates()
                .iter()
                .map(PredictionCandidate::surface)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn history_four_plus_direct_five_fills_without_repair_candidates() {
    let conversion = prediction_fixture_conversion(
            "かあ\t直接0\t0\t0\t100\t100\tpredict\tdirect\nかい\t直接1\t0\t0\t101\t101\tpredict\tdirect\nかう\t直接2\t0\t0\t102\t102\tpredict\tdirect\nかえ\t直接3\t0\t0\t103\t103\tpredict\tdirect\nかお\t直接4\t0\t0\t104\t104\tpredict\tdirect\n",
        );
    let learning = LearningService::memory();
    for index in 0..4 {
        learning.learn(
            &format!("か履歴{index}"),
            &format!("履歴{index}"),
            0,
            100 - index,
        );
    }

    let result = predict_fixture(&conversion, "か", Some(&learning));

    assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.source() == PredictionSource::History)
            .count(),
        4
    );
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface().starts_with("直接"))
            .count(),
        5
    );
    assert!(result
        .candidates()
        .iter()
        .all(|candidate| !candidate.surface().contains("補正")));
}

#[test]
fn history_four_plus_direct_four_leaves_one_repair_slot() {
    let conversion = prediction_fixture_conversion(
            "かが\t直接0\t0\t0\t100\t100\tpredict\tdirect\nかがあ\t直接1\t0\t0\t101\t101\tpredict\tdirect\nかがい\t直接2\t0\t0\t102\t102\tpredict\tdirect\nかがう\t直接3\t0\t0\t103\t103\tpredict\tdirect\nかか\t規則補正\t0\t0\t1000\t1000\tpredict\trule\nいが\t高度補正\t0\t0\t1000\t1000\tpredict\tadvanced\n",
        );
    let learning = LearningService::memory();
    for index in 0..4 {
        learning.learn(
            &format!("かが履歴{index}"),
            &format!("履歴{index}"),
            0,
            100 - index,
        );
    }

    let result = predict_fixture(&conversion, "かが", Some(&learning));

    assert_eq!(result.candidates().len(), MAX_SUGGESTIONS);
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.source() == PredictionSource::History)
            .count(),
        4
    );
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface().starts_with("直接"))
            .count(),
        4
    );
    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.surface().contains("補正"))
            .count(),
        1
    );
}

fn ranked_surface(surface: &str) -> FixedStr<MAX_PREDICTION_SURFACE_BYTES> {
    let mut fixed = FixedStr::new();
    fixed.push_str(surface).expect("ranked surface");
    fixed
}

#[test]
fn ranked_surface_invariants_hold_for_bounded_exhaustive_sequences() {
    const SURFACES: [&str; 3] = ["surface-a", "surface-b", "surface-c"];
    const STEPS: usize = 7;
    let cases = 3usize.pow(STEPS as u32);

    for mut encoded in 0..cases {
        let mut ranked = Ranked::new();
        let mut best: [Option<(i64, DictionarySource, u32)>; SURFACES.len()] =
            [None; SURFACES.len()];
        for step in 0..STEPS {
            let surface_index = encoded % SURFACES.len();
            encoded /= SURFACES.len();
            let source = if (step + surface_index).is_multiple_of(2) {
                DictionarySource::System
            } else {
                DictionarySource::User
            };
            let score = ((step * 7 + surface_index * 3) % 11) as i64;
            let index = u32::try_from(step).expect("step");
            let key = (score, source, index);
            if best[surface_index].is_none_or(|current| key < current) {
                best[surface_index] = Some(key);
            }
            ranked.insert(RankedItem::new(
                score,
                source,
                index,
                ranked_surface(SURFACES[surface_index]),
            ));
        }

        assert!(ranked
            .as_slice()
            .windows(2)
            .all(|items| items[0].key() <= items[1].key()));
        assert!(ranked
            .as_slice()
            .windows(2)
            .all(|items| items[0].surface.as_str() != items[1].surface.as_str()));
        for item in ranked.as_slice() {
            let surface_index = SURFACES
                .iter()
                .position(|surface| *surface == item.surface.as_str())
                .expect("known surface");
            assert_eq!(item.key(), best[surface_index].expect("best item"));
        }
    }
}

#[test]
fn prediction_limits_displayed_history_candidates_without_trimming_retention() {
    let conversion = conversion();
    let learning = Arc::new(LearningService::memory());
    for index in 0..(MAX_HISTORY_SUGGESTIONS_PER_RESULT + 2) {
        learning.learn(
            &format!("history-{index}"),
            &format!("history-surface-{index}"),
            0,
            0,
        );
    }

    let mut retained = Vec::new();
    learning.visit_prediction_history("history-", |reading, surface, _, _| {
        retained.push((reading.to_owned(), surface.to_owned()));
        true
    });
    assert_eq!(
        retained.len(),
        MAX_HISTORY_SUGGESTIONS_PER_RESULT + 2,
        "the retained history window remains broader than one displayed result"
    );

    let runtime =
        PredictionRuntime::start_with_learning(Arc::clone(&conversion), Arc::clone(&learning))
            .expect("runtime");
    let result = runtime
        .service()
        .request(
            1,
            1,
            "history-",
            0,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_millis(100),
        )
        .expect("prediction");

    assert_eq!(
        result
            .candidates()
            .iter()
            .filter(|candidate| candidate.source() == PredictionSource::History)
            .count(),
        MAX_HISTORY_SUGGESTIONS_PER_RESULT
    );
    assert_eq!(
        result.candidates().len(),
        MAX_HISTORY_SUGGESTIONS_PER_RESULT,
        "no dictionary fixture entry matches the dedicated history prefix"
    );
    runtime.stop().expect("joined worker");
}

#[test]
fn spelling_correction_follows_the_unified_prediction_gate() {
    let entries = dictc::parse_entries(
            "fixture.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあい\t藍\t0\t0\t10\t1\tcorrection,predict\t\nあい\t愛\t0\t0\t100\t50\tpredict\t\nあいう\t愛う\t0\t0\t90\t40\tpredict\t\n",
        )
        .expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let bytes = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("compile")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(ConversionService::from_static_bytes(bytes).expect("service"));
    let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
    let user = conversion.user_dictionary_snapshot();

    let mut prefix = FixedStr::new();
    prefix.push_str("あい").expect("prefix");

    let open = Query {
        sequence: 1,
        session: 1,
        generation: 1,
        prefix: prefix.clone(),
        domain_it_per_mille: 0,
        input_support: InputSupport::default(),
        skip_input_repair: false,
        allow_spelling_correction: true,
    };
    let mut ranked = PredictionResult::default();
    index.predict_into(&open, user.as_ref(), None, &mut ranked);
    assert!(
        ranked
            .candidates()
            .iter()
            .any(|candidate| candidate.surface() == "藍"),
        "positive control: SPELLING_CORRECTION must predict when the gate is open"
    );

    for (label, support, skip, allow) in [
        (
            "master-off",
            InputSupport {
                enabled: false,
                ..InputSupport::default()
            },
            false,
            false,
        ),
        ("skip", InputSupport::default(), true, false),
        (
            "fuzzy-off",
            InputSupport {
                fuzzy_proper_nouns: false,
                ..InputSupport::default()
            },
            false,
            false,
        ),
    ] {
        assert_eq!(
            support.allows_spelling_correction(skip),
            allow,
            "{label} admission snapshot"
        );
        let query = Query {
            sequence: 1,
            session: 1,
            generation: 1,
            prefix: prefix.clone(),
            domain_it_per_mille: 0,
            input_support: support,
            skip_input_repair: skip,
            allow_spelling_correction: allow,
        };
        let mut ranked = PredictionResult::default();
        index.predict_into(&query, user.as_ref(), None, &mut ranked);
        assert!(
            ranked
                .candidates()
                .iter()
                .all(|candidate| candidate.surface() != "藍"),
            "{label}: SPELLING_CORRECTION must stay out of the main prediction path"
        );
        assert!(
            ranked
                .candidates()
                .iter()
                .any(|candidate| candidate.surface() == "愛"),
            "{label}: normal predictive entries must remain"
        );
    }
}

#[test]
fn gated_spelling_correction_does_not_pollute_prediction_budget() {
    let mut tsv = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        );
    for index in 0..16 {
        tsv.push_str(&format!(
            "あい\t藍{index}\t0\t0\t1\t1\tcorrection,predict\t\n"
        ));
    }
    for index in 0..9 {
        tsv.push_str(&format!(
            "あい\t愛{index}\t0\t0\t100\t{}\tpredict\t\n",
            10 + index
        ));
    }
    let entries = dictc::parse_entries("fixture.tsv", &tsv).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let bytes = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("compile")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(ConversionService::from_static_bytes(bytes).expect("service"));
    let index = PredictionIndex::build(conversion.dictionary()).expect("prediction index");
    let user = conversion.user_dictionary_snapshot();
    let mut prefix = FixedStr::new();
    prefix.push_str("あい").expect("prefix");
    let query = Query {
        sequence: 1,
        session: 1,
        generation: 1,
        prefix,
        domain_it_per_mille: 0,
        input_support: InputSupport::default(),
        skip_input_repair: true,
        allow_spelling_correction: false,
    };
    let mut ranked = PredictionResult::default();
    index.predict_into(&query, user.as_ref(), None, &mut ranked);
    let surfaces: Vec<&str> = ranked
        .candidates()
        .iter()
        .map(PredictionCandidate::surface)
        .collect();
    assert!(
        surfaces.iter().all(|surface| !surface.starts_with('藍')),
        "gated SPELLING_CORRECTION must not occupy prediction slots: {surfaces:?}"
    );
    assert!(
        surfaces.iter().any(|surface| surface.starts_with('愛')),
        "ordinary predictive entries must fill the budget: {surfaces:?}"
    );
    assert_eq!(surfaces.len(), MAX_SUGGESTIONS);
}

#[test]
fn the_single_pending_slot_coalesces_to_the_newest_query() {
    let mailbox = Mailbox::new();
    let first = mailbox
        .publish(1, 1, "か", 0, InputSupport::default(), false)
        .expect("first");
    let second = mailbox
        .publish(2, 2, "かな", 0, InputSupport::default(), false)
        .expect("second");
    assert!(second > first);
    assert_eq!(mailbox.coalesced.load(Ordering::Relaxed), 1);
    let state = mailbox.state.lock().expect("mailbox");
    let pending = state.pending.as_ref().expect("newest pending");
    assert_eq!(pending.session, 2);
    assert_eq!(pending.prefix.as_str(), "かな");
}

#[test]
fn prediction_reads_do_not_consume_a_conversion_arena() {
    let conversion = conversion();
    let runtime = PredictionRuntime::start(Arc::clone(&conversion)).expect("runtime");
    let _ = runtime
        .service()
        .request(
            1,
            1,
            "か",
            0,
            sakura_core::InputSupport::default(),
            false,
            Duration::from_millis(100),
        )
        .expect("prediction");
    let converted = conversion
        .with_candidates("かな", ConversionOptions::default(), |candidates| {
            candidates
                .first()
                .map(|candidate| candidate.text().to_owned())
        })
        .expect("conversion slot");
    assert_eq!(converted.as_deref(), Some("仮名"));
    runtime.stop().expect("joined worker");
}
