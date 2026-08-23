use dictc::{compile, parse_connection, parse_entries};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::{Dictionary, EntryFlags};
use sakura_core::user_dictionary::UserDictionary;
use sakura_core::ConversionMethod;

const ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
ぷるりくえすと\tPull request\t1\t1\t120\t80\tit,predict\tIT term\n\
きょう\t今日\t1\t1\t100\t-\t\t\n\
は\tは\t1\t1\t100\t-\t\t\n\
きょうは\t今日は\t1\t1\t500\t-\t\t\n\
あさって\t明後日\t1\t1\t100\t-\t\t\n\
らいしゅう\t来週\t1\t1\t100\t-\t\t\n\
せんしゅう\t先週\t1\t1\t100\t-\t\t\n\
かんすう\t函数\t1\t1\t1000\t-\t\t\n\
かんすう\t関数\t1\t1\t1100\t600\tit,predict\tprogramming\n\
じょうたい\t状態\t1\t1\t100\t-\t\t\n\
せんい\t遷移\t1\t1\t100\t-\t\t\n\
だみー\tダミー\t1\t1\t9000\t-\t\t\n\
go\tGo\t1\t1\t100\t-\tit,predict\tlanguage\n\
lvm\tLVM\t1\t1\t100\t-\tit,predict\tstorage\n\
gitlab\tGitLab\t1\t1\t100\t-\tit,predict\thosting\n\
ipあどれす\tIPアドレス\t1\t1\t100\t-\tit,predict\tnetwork\n";

const CONNECTION: &str = "# license: BSD-3-Clause\n\
classes\t3\n\
default\t0\n";

fn fixture() -> Vec<u8> {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let matrix = parse_connection("matrix.tsv", CONNECTION, false).expect("matrix");
    compile(&entries, &matrix).expect("image")
}

fn compile_fixture(entries: &str) -> Vec<u8> {
    let entries = parse_entries("quality-gate.tsv", entries).expect("entries");
    let matrix = parse_connection("matrix.tsv", CONNECTION, false).expect("matrix");
    compile(&entries, &matrix).expect("image")
}

#[test]
fn viterbi_finds_a_multiword_path_and_astar_returns_unique_n_best() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "きょうは",
            ConversionOptions {
                max_candidates: 5,
                method: ConversionMethod::MultiSegment,
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");

    assert_eq!(candidates[0].text(), "今日は");
    assert_eq!(candidates[0].cost, 200);
    let segments = candidates[0].segments();
    assert_eq!(segments.len(), 2);
    assert_eq!((segments[0].reading_start, segments[0].reading_end), (0, 9));
    assert_eq!(
        (segments[1].reading_start, segments[1].reading_end),
        (9, 12)
    );
    assert_eq!(
        &candidates[0].text()
            [usize::from(segments[0].text_start)..usize::from(segments[0].text_end)],
        "今日"
    );
    assert!(candidates
        .windows(2)
        .all(|pair| pair[0].cost <= pair[1].cost));
    for (index, candidate) in candidates.iter().enumerate() {
        assert!(
            candidates[..index]
                .iter()
                .all(|before| before.text() != candidate.text()),
            "duplicate surface {}",
            candidate.text()
        );
    }
}

#[test]
fn single_segment_method_builds_only_whole_reading_candidates() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "きょうは",
            ConversionOptions {
                method: ConversionMethod::SingleSegment,
                ..ConversionOptions::default()
            },
        )
        .expect("single-segment conversion");

    assert!(!candidates.is_empty());
    assert!(candidates
        .iter()
        .all(|candidate| candidate.segments().len() == 1));
    assert!(candidates
        .iter()
        .all(|candidate| candidate.segments()[0].reading_start == 0
            && candidate.segments()[0].reading_end
                == u16::try_from("きょうは".len()).expect("fixture fits")));
}

#[test]
fn english_surfaces_do_not_offer_identifier_case_candidates() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ぷるりくえすと", ConversionOptions::default())
        .expect("conversion");

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text() == "Pull request"),
        "the dictionary surface itself must remain"
    );
    for unexpected in [
        "pullRequest",
        "pull_request",
        "PULL_REQUEST",
        "pull-request",
    ] {
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.text() != unexpected),
            "generated identifier variant {unexpected} must not appear"
        );
    }
}

#[test]
fn bounded_it_prior_can_change_a_close_choice_but_zero_bias_cannot() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let general = converter
        .convert(
            &dictionary,
            "かんすう",
            ConversionOptions {
                max_candidates: 2,
                method: ConversionMethod::MultiSegment,
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("general");
    assert_eq!(general[0].text(), "函数");

    let technical = converter
        .convert(
            &dictionary,
            "かんすう",
            ConversionOptions {
                max_candidates: 2,
                method: ConversionMethod::MultiSegment,
                it_bias_per_mille: 200,
                max_it_boost: 800,
                initial_right_id: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("technical");
    assert_eq!(technical[0].text(), "関数");
}

#[test]
fn word_sized_compounds_accumulate_it_evidence_without_changing_standalone_words() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
きのう\t昨日\t1\t1\t1000\t-\t\t\n\
きのう\t機能\t1\t1\t2056\t-\tit\t\n\
とうごう\t統合\t1\t1\t1000\t-\t\t\n\
いがい\t以外\t1\t1\t1000\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let standalone = converter
        .convert(&dictionary, "きのう", ConversionOptions::default())
        .expect("standalone conversion");
    assert_eq!(standalone[0].text(), "昨日");

    let compound = converter
        .convert(&dictionary, "きのうとうごう", ConversionOptions::default())
        .expect("word-sized compound");
    assert_eq!(compound[0].text(), "機能統合");
    assert!(compound
        .iter()
        .any(|candidate| candidate.text() == "昨日統合"));

    let short_phrase = converter
        .convert(&dictionary, "きのういがい", ConversionOptions::default())
        .expect("short ordinary phrase");
    assert_eq!(short_phrase[0].text(), "昨日以外");
}

#[test]
fn exact_reading_gate_keeps_close_splits_and_removes_only_the_far_mosaic_tail() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あじゅーる\tアジュール\t1\t1\t1000\t-\tit\t\n\
あじゅーる\tAzure\t1\t1\t1500\t-\tit\t\n\
あ\t亜\t1\t1\t1000\t-\t\t\n\
あ\t阿\t1\t1\t6500\t-\t\t\n\
じゅーる\tジュール\t1\t1\t1000\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert(&dictionary, "あじゅーる", ConversionOptions::default())
        .expect("exact loanword");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "アジュール"));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "Azure"));
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "亜ジュール"));
    assert!(candidates
        .iter()
        .all(|candidate| candidate.text() != "阿ジュール"));
}

#[test]
fn exact_japanese_compounds_keep_far_but_lexical_split_alternatives() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
ぎせいほうじん\t犠牲法人\t1\t1\t1000\t-\t\t\n\
ぎせい\t犠牲\t1\t1\t1000\t-\t\t\n\
ぎせい\t擬制\t1\t1\t6500\t-\t\t\n\
ほうじん\t法人\t1\t1\t1000\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert(&dictionary, "ぎせいほうじん", ConversionOptions::default())
        .expect("Japanese compound");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "擬制法人"));
}

#[test]
fn multiword_conversion_is_unchanged_when_no_whole_reading_entry_exists() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あ\t阿\t1\t1\t6500\t-\t\t\n\
じゅーる\tジュール\t1\t1\t1000\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert(&dictionary, "あじゅーる", ConversionOptions::default())
        .expect("multiword conversion");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "阿ジュール"));
}

#[test]
fn an_oversized_english_surface_does_not_invent_identifier_variants() {
    let surface = format!("{}B{}", "a".repeat(768), "b".repeat(767));
    assert_eq!(surface.len(), 1536);
    let entries = format!(
        "# license: BSD-3-Clause\n\
         reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
         てすと\t{surface}\t1\t1\t100\t-\t\t\n"
    );
    let bytes = compile_fixture(&entries);
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "てすと", ConversionOptions::default())
        .expect("a max-length English surface must still convert");

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.text() == surface),
        "the base dictionary candidate must survive"
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate.text().contains('_') && !candidate.text().contains('-')),
        "no identifier-case variant may appear"
    );
}

#[test]
fn conversion_does_not_re_spell_english_paths_as_identifier_cases() {
    let entries = "# license: BSD-3-Clause\n\
         reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
         てすと\tTest\t1\t1\t100\t-\t\t\n\
         えーぴーあい\tAPI\t1\t1\t100\t-\tit,predict\tinterface\n";
    let bytes = compile_fixture(entries);
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "てすとえーぴーあい",
            ConversionOptions::default(),
        )
        .expect("conversion");

    let base = candidates
        .iter()
        .find(|candidate| candidate.text() == "TestAPI")
        .expect("the multi-word path itself");
    assert_eq!(base.segments().len(), 2);
    assert!(!base.segments()[0].flags.contains(EntryFlags::IT));
    assert!(base.segments()[1].flags.contains(EntryFlags::IT));

    for unexpected in ["testApi", "test_api", "TEST_API", "test-api"] {
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.text() != unexpected),
            "generated identifier variant {unexpected} must not appear"
        );
    }
}

#[test]
fn irregular_counter_and_katakana_synthetic_edges_are_available() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let counter = converter
        .convert(&dictionary, "さんぼん", ConversionOptions::default())
        .expect("counter");
    assert_eq!(counter[0].text(), "3本");
    assert!(counter
        .iter()
        .any(|candidate| candidate.text() == "サンボン"));
}

#[test]
fn a_long_synthetic_run_does_not_flatten_a_known_multiword_phrase() {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let matrix = parse_connection(
        "matrix.tsv",
        "# license: BSD-3-Clause\nclasses\t3\ndefault\t5000\n",
        false,
    )
    .expect("matrix");
    let bytes = compile(&entries, &matrix).expect("image");
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(
            &dictionary,
            "じょうたいせんい",
            ConversionOptions::default(),
        )
        .expect("conversion");

    assert_eq!(candidates[0].text(), "状態遷移");
    assert!(candidates
        .iter()
        .any(|candidate| candidate.text() == "ジョウタイセンイ"));
}

#[test]
fn bounded_it_completion_coherence_resolves_a_compound_homophone() {
    let entries = parse_entries(
        "coherence.tsv",
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nこうせい\t構成\t1\t1\t100\t-\t\t\nいぞん\t異存\t1\t1\t100\t-\t\t\nいぞん\t依存\t1\t1\t800\t-\t\t\nこうせいいぞんせい\t構成依存性\t1\t1\t5000\t5000\tit,predict\ttechnical completion\n",
    )
    .expect("entries");
    let matrix = parse_connection(
        "coherence-matrix.tsv",
        "# license: MIT\nclasses\t3\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = compile(&entries, &matrix).expect("image");
    let dictionary = Dictionary::parse(&image).expect("dictionary");
    let mut converter = Converter::new();

    let general = converter
        .convert(
            &dictionary,
            "こうせいいぞん",
            ConversionOptions {
                it_bias_per_mille: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("general conversion");
    assert_eq!(general[0].text(), "構成異存");

    let technical = converter
        .convert(&dictionary, "こうせいいぞん", ConversionOptions::default())
        .expect("technical conversion");
    assert_eq!(technical[0].text(), "構成依存");
}

#[test]
fn every_option_and_arena_bound_has_an_explicit_error_or_fallback() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    assert!(converter
        .convert(&dictionary, "", ConversionOptions::default())
        .is_err());
    assert!(converter
        .convert(
            &dictionary,
            "かな",
            ConversionOptions {
                max_candidates: 0,
                ..ConversionOptions::default()
            },
        )
        .is_err());

    let oov = converter
        .convert(&dictionary, "みちのご", ConversionOptions::default())
        .expect("synthetic fallback keeps the lattice complete");
    assert!(!oov.is_empty());
}

#[test]
fn carried_right_context_changes_the_initial_connection_cost() {
    let entries = parse_entries(
        "context.tsv",
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nいった\t言った\t1\t1\t50\t50\t\tgeneric\nいった\t行った\t2\t2\t100\t100\t\tcontextual\n",
    )
    .expect("entries");
    let matrix = parse_connection(
        "context-matrix.tsv",
        "# license: MIT\nclasses\t4\ndefault\t0\ncost\t3\t1\t1000\ncost\t3\t2\t0\n",
        false,
    )
    .expect("matrix");
    let image = compile(&entries, &matrix).expect("image");
    let dictionary = Dictionary::parse(&image).expect("dictionary");
    let mut converter = Converter::new();

    let generic = converter
        .convert(
            &dictionary,
            "いった",
            ConversionOptions {
                it_bias_per_mille: 0,
                max_it_boost: 0,
                ..ConversionOptions::default()
            },
        )
        .expect("generic conversion");
    assert_eq!(generic[0].text(), "言った");

    let contextual = converter
        .convert(
            &dictionary,
            "いった",
            ConversionOptions {
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 3,
                ..ConversionOptions::default()
            },
        )
        .expect("contextual conversion");
    assert_eq!(contextual[0].text(), "行った");
}

#[test]
fn user_dictionary_entries_join_the_bounded_lattice() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let user_dictionary = UserDictionary::parse_tsv(
        "# Sakura Input user dictionary v1\n\
reading\tsurface\tpos\tcomment\n\
さくら\tSakura Input\tproper-noun\tproject name\n",
    )
    .expect("user dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert_with_user_dictionary(
            &dictionary,
            Some(&user_dictionary),
            "さくらは",
            ConversionOptions::default(),
        )
        .expect("conversion");

    let candidate = candidates
        .iter()
        .find(|candidate| candidate.text() == "Sakura Inputは")
        .expect("user entry followed by a system entry");
    assert_eq!(candidate.annotation(), "project name");
    assert_eq!(candidate.segments().len(), 2);
    assert_eq!(candidate.segments()[0].reading_start, 0);
    assert_eq!(candidate.segments()[0].reading_end, 9);
    assert_eq!(candidate.segments()[1].reading_start, 9);
    assert_eq!(candidate.segments()[1].reading_end, 12);
}

#[test]
fn a_latin_token_is_never_segmented_into_partial_dictionary_entries() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    // `go` and `lvm` are real entries, but they only cover part of what the
    // user typed. Joining them to the rest of the token used to win top-1 with
    // mixed-case nonsense -- the shipping dictionary converted `goto` to `GoTO`
    // and `llvm` to `lLVM`, both ahead of the reading itself.
    for reading in ["goto", "llvm"] {
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion");
        assert_eq!(
            candidates[0].text(),
            reading,
            "{reading} should fall back to the token the user typed"
        );
        assert!(
            candidates.iter().all(|candidate| candidate
                .segments()
                .iter()
                .all(|segment| usize::from(segment.reading_end) == reading.len())),
            "{reading} still offers a candidate stitched from a partial entry: {:?}",
            candidates.iter().map(|c| c.text()).collect::<Vec<_>>()
        );
    }

    // An entry that covers the whole Latin token still wins, and so does one
    // that starts at the token and continues past it into kana.
    let candidates = converter
        .convert(&dictionary, "gitlab", ConversionOptions::default())
        .expect("conversion");
    assert_eq!(candidates[0].text(), "GitLab");

    let candidates = converter
        .convert(&dictionary, "ipあどれす", ConversionOptions::default())
        .expect("conversion");
    assert_eq!(candidates[0].text(), "IPアドレス");
}

#[test]
fn n_best_rejects_spliced_kana_fallbacks_but_keeps_lossless_whole_reading_fallbacks() {
    // These short, inexpensive dictionary entries intentionally reproduce the
    // bad N-best shape reported for `ぷろんふと`: a partial entry, an unmatched
    // kana fallback, and more unrelated partial entries. They are individual
    // dictionary matches, but the concatenation is not a conversion candidate
    // the user can trust.
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
ぷろ\tプロ\t1\t1\t10\t-\t\t\n\
ふ\t布\t1\t1\t10\t-\t\t\n\
ふ\t富\t1\t1\t11\t-\t\t\n\
ふ\t婦\t1\t1\t12\t-\t\t\n\
ふ\t夫\t1\t1\t13\t-\t\t\n\
と\t戸\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    // The lowest-cost unfiltered path would have been `プロん布戸`. It must not
    // occupy even the one-candidate conversion result; the converter must keep
    // searching until it finds a complete lossless fallback.
    let top_one = converter
        .convert(
            &dictionary,
            "ぷろんふと",
            ConversionOptions {
                max_candidates: 1,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");
    assert_eq!(top_one.len(), 1);
    assert_eq!(top_one[0].text(), "ぷろんふと");

    let candidates = converter
        .convert(&dictionary, "ぷろんふと", ConversionOptions::default())
        .expect("conversion");
    let texts: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.text())
        .collect();
    assert_eq!(texts, ["ぷろんふと", "プロンフト"]);
    assert!(texts
        .iter()
        .all(|text| !text.contains('ん') || *text == "ぷろんふと"));
}

#[test]
fn quality_gate_covers_every_unicode_prefix_boundary_without_hiding_complete_segmentation() {
    // Run every character boundary through a dictionary that recognizes only
    // the prefix. This is a compact property-style check: no partial lexical
    // prefix may be joined to the remaining kana fallback, including across
    // UTF-8 boundaries. The one fully lexical two-word phrase stays valid.
    let reading = "あいうえお";
    let expected_fallbacks = ["あいうえお", "アイウエオ"];
    let mut converter = Converter::new();
    for boundary in 1..reading.chars().count() {
        let prefix: String = reading.chars().take(boundary).collect();
        let entries = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{prefix}\t語\t1\t1\t10\t-\t\t\n"
        );
        let bytes = compile_fixture(&entries);
        let dictionary = Dictionary::parse(&bytes).expect("dictionary");
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("partial conversion");
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.text())
                .collect::<Vec<_>>(),
            expected_fallbacks,
            "partial prefix boundary {boundary} leaked a spliced candidate"
        );
    }

    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あい\t愛\t1\t1\t10\t-\t\t\n\
うえお\t上尾\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let candidates = converter
        .convert(&dictionary, reading, ConversionOptions::default())
        .expect("complete multiword conversion");
    assert_eq!(candidates[0].text(), "愛上尾");
    assert_eq!(candidates[0].segments().len(), 2);
}

#[cfg(feature = "conversion-test-support")]
#[test]
fn state_budget_exhaustion_is_explicit_and_keeps_the_lossless_reading() {
    use sakura_core::conversion::ConversionSearchTerminal;

    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あ\t亜\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    converter.set_search_state_budget_for_test(0);

    let result = converter
        .convert_detailed(&dictionary, "あいう", ConversionOptions::default())
        .expect("budget exhaustion degrades to the lossless fallback");

    assert_eq!(
        result.diagnostics().terminal,
        ConversionSearchTerminal::StateBudgetReached
    );
    assert_eq!(result.diagnostics().states_pushed, 0);
    assert!(result.diagnostics().lossless_fallback_inserted);
    assert_eq!(result.candidates().len(), 1);
    assert_eq!(result.candidates()[0].text(), "あいう");
    assert_eq!(result.candidates()[0].segments().len(), 1);
}

#[cfg(feature = "conversion-test-support")]
#[test]
fn lattice_budget_exhaustion_is_explicit_and_keeps_the_lossless_reading() {
    use sakura_core::conversion::ConversionSearchTerminal;

    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    converter.set_lattice_node_budget_for_test(0);

    let result = converter
        .convert_detailed(&dictionary, "さくら", ConversionOptions::default())
        .expect("lattice exhaustion degrades to the lossless fallback");

    assert_eq!(
        result.diagnostics().terminal,
        ConversionSearchTerminal::LatticeBudgetReached
    );
    assert!(result.diagnostics().lossless_fallback_inserted);
    assert_eq!(result.candidates().len(), 1);
    assert_eq!(result.candidates()[0].text(), "さくら");
}

#[test]
fn incoherent_prefixes_are_pruned_before_they_consume_search_states() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あ\t亜\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let result = converter
        .convert_detailed(&dictionary, "あいう", ConversionOptions::default())
        .expect("conversion");

    assert!(result.diagnostics().incoherent_prefixes_pruned > 0);
    assert!(result
        .candidates()
        .iter()
        .all(|candidate| candidate.text() == "あいう" || candidate.text() == "アイウ"));
}

#[test]
fn over_segmented_lexical_paths_degrade_to_one_lossless_segment() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
あ\t亜\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let reading = "あ".repeat(sakura_proto::MAX_SEGMENTS + 1);

    let candidates = converter
        .convert(&dictionary, &reading, ConversionOptions::default())
        .expect("segment overflow degrades to the lossless fallback");

    let fallback = candidates
        .iter()
        .find(|candidate| candidate.text() == reading)
        .expect("lossless fallback");
    assert_eq!(fallback.segments().len(), 1);
}

#[test]
fn a_full_top_one_result_is_not_displaced_by_the_reserved_fallback() {
    let bytes = compile_fixture(
        "# license: MIT\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
かな\t仮名\t1\t1\t10\t-\t\t\n",
    );
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert(
            &dictionary,
            "かな",
            ConversionOptions {
                max_candidates: 1,
                ..ConversionOptions::default()
            },
        )
        .expect("conversion");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].text(), "仮名");
}

#[test]
fn today_readings_offer_reiwa_gregorian_and_weekday_date_surfaces() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    converter.set_civil_date(sakura_core::CivilDate::from_ymd(2026, 8, 19));
    let candidates = converter
        .convert(&dictionary, "きょう", ConversionOptions::default())
        .expect("conversion");

    assert_eq!(candidates[0].text(), "今日");
    for expected in [
        ("令和8年8月19日", "和暦"),
        ("令和8年8月19日（水）", "和暦・曜日"),
        ("2026年8月19日", "西暦"),
        ("2026年8月19日（水）", "西暦・曜日"),
    ] {
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.text() == expected.0)
            .unwrap_or_else(|| panic!("missing date candidate {}", expected.0));
        assert_eq!(candidate.annotation(), expected.1);
        assert!(candidate.cost > candidates[0].cost);
        assert_eq!(candidate.system_entry_index(), None);
        assert_eq!(candidate.segments().len(), 1);
    }
    assert!(
        candidates
            .iter()
            .all(|candidate| !candidate.text().contains("2026/")
                && !candidate.text().contains("2026-")),
        "slash and ISO date surfaces were not requested"
    );
}

#[test]
fn phrase_readings_that_only_contain_today_do_not_grow_date_surfaces() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    converter.set_civil_date(sakura_core::CivilDate::from_ymd(2026, 8, 19));
    let candidates = converter
        .convert(&dictionary, "きょうは", ConversionOptions::default())
        .expect("conversion");

    assert_eq!(candidates[0].text(), "今日は");
    assert!(candidates
        .iter()
        .all(|candidate| !candidate.text().contains("令和")
            && !candidate.text().contains("2026年")
            && !candidate.text().contains("2026/")));
}

#[test]
fn date_surfaces_are_absent_until_a_civil_date_is_supplied() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "きょう", ConversionOptions::default())
        .expect("conversion");

    assert_eq!(candidates[0].text(), "今日");
    assert!(candidates.iter().all(|candidate| candidate.text() == "今日"
        || !candidate.text().chars().any(|c| c.is_ascii_digit())));
}

#[test]
fn relative_date_readings_offer_offset_reiwa_and_gregorian_surfaces() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let today = sakura_core::CivilDate::from_ymd(2026, 8, 19);
    for (reading, lexical, expected) in [
        (
            "あさって",
            "明後日",
            [
                "令和8年8月21日",
                "令和8年8月21日（金）",
                "2026年8月21日",
                "2026年8月21日（金）",
            ],
        ),
        (
            "らいしゅう",
            "来週",
            [
                "令和8年8月26日",
                "令和8年8月26日（水）",
                "2026年8月26日",
                "2026年8月26日（水）",
            ],
        ),
        (
            "せんしゅう",
            "先週",
            [
                "令和8年8月12日",
                "令和8年8月12日（水）",
                "2026年8月12日",
                "2026年8月12日（水）",
            ],
        ),
    ] {
        let mut converter = Converter::new();
        converter.set_civil_date(today);
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion");
        assert_eq!(
            candidates[0].text(),
            lexical,
            "{reading} stays lexical first"
        );
        for surface in expected {
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.text() == surface),
                "{reading} missing {surface}"
            );
        }
        assert!(
            candidates
                .iter()
                .all(|candidate| !candidate.text().contains('/')
                    && !candidate.text().contains("2026-")),
            "{reading} kept a slash or ISO date surface"
        );
    }
}

const NUMBER_JUNK_ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
2\t²\t1\t1\t50\t-\t\tsuperscript\n\
4\t4\t1\t1\t50\t-\t\t\n\
にち\t日\t1\t1\t50\t-\t\t\n\
に\tに\t1\t1\t100\t-\t\t\n\
に\t²\t1\t1\t80\t-\t\tsuperscript\n\
じゅう\t十\t1\t1\t100\t-\t\t\n\
じゅう\t重\t1\t1\t90\t-\t\t\n\
じゅう\t銃\t1\t1\t95\t-\t\t\n\
よん\t4\t1\t1\t50\t-\t\t\n\
よん\t四\t1\t1\t100\t-\t\t\n\
よん\t呼ん\t1\t1\t90\t-\t\t\n\
よん\t読ん\t1\t1\t91\t-\t\t\n\
にじゅう\t二重\t1\t1\t80\t-\t\t\n\
せん\t線\t1\t1\t80\t-\t\t\n\
せん\t戦\t1\t1\t90\t-\t\t\n\
さんぜん\t産前\t1\t1\t80\t-\t\t\n\
じゅうよん\t十四\t1\t1\t80\t-\t\t\n\
じゅうよん\t⑭\t1\t1\t70\t-\t\tcircled\n";

fn number_junk_dictionary() -> Vec<u8> {
    compile_fixture(NUMBER_JUNK_ENTRIES)
}

#[test]
fn twenty_four_day_readings_offer_arabic_fullwidth_and_kanji_dates() {
    let bytes = number_junk_dictionary();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    for reading in ["にじゅうよんにち", "にじゅうよっか", "24にち", "２４にち"]
    {
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion");
        let texts: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.text())
            .collect();
        assert_eq!(texts[0], "24日", "{reading} should convert to 24日 first");
        assert!(
            texts.contains(&"２４日"),
            "{reading} missing full-width ２４日: {texts:?}"
        );
        assert!(
            texts.contains(&"二十四日"),
            "{reading} missing 二十四日: {texts:?}"
        );
        assert!(
            texts
                .iter()
                .all(|text| !text.contains('²') && !text.contains('⑭')),
            "{reading} kept a decorative numeral: {texts:?}"
        );
        assert!(
            !texts.contains(&"二重呼ん") && !texts.iter().any(|text| text.contains("呼ん")),
            "{reading} kept a homophone splice: {texts:?}"
        );
    }
}

#[test]
fn ascii_digit_runs_are_not_split_into_superscript_dates() {
    let bytes = number_junk_dictionary();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "24にち", ConversionOptions::default())
        .expect("conversion");
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.text() != "²4日"),
        "24にち must not become ²4日"
    );
}

#[test]
fn twenty_four_without_a_counter_still_drops_homophone_splices() {
    let bytes = number_junk_dictionary();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "にじゅうよん", ConversionOptions::default())
        .expect("conversion");
    let texts: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.text())
        .collect();
    assert_eq!(texts[0], "24");
    assert!(texts.contains(&"２４"), "missing ２４: {texts:?}");
    assert!(texts.contains(&"二十四"), "missing 二十四: {texts:?}");
    assert!(
        texts.iter().all(|text| !text.contains("呼ん")
            && !text.contains("読ん")
            && !text.contains('⑭')
            && !text.contains('²')),
        "にじゅうよん kept a splice or decorative numeral: {texts:?}"
    );
}

#[test]
fn senjitsu_does_not_offer_a_thousand_days() {
    let entries = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
せんじつ\t先日\t1\t1\t100\t-\t\t\n\
ぜんじつ\t全日\t1\t1\t100\t-\t\t\n\
せん\t千\t1\t1\t80\t-\t\t\n\
じつ\t日\t1\t1\t50\t-\t\t\n\
にち\t日\t1\t1\t50\t-\t\t\n";
    let bytes = compile_fixture(entries);
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    for reading in ["せんじつ", "ぜんじつ"] {
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion");
        let texts: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.text())
            .collect();
        assert!(
            !texts
                .iter()
                .any(|text| *text == "1000日" || *text == "１０００日" || *text == "千日"),
            "{reading} must not rewrite a lexical じつ word into a day count: {texts:?}"
        );
        assert_eq!(
            texts[0],
            if reading == "せんじつ" {
                "先日"
            } else {
                "全日"
            },
            "{reading} first candidate: {texts:?}"
        );
    }
}

#[test]
fn twenty_keeps_its_lexical_word_alongside_numeric_forms() {
    let bytes = number_junk_dictionary();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "にじゅう", ConversionOptions::default())
        .expect("conversion");
    let texts: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.text())
        .collect();
    assert!(
        texts.contains(&"二重"),
        "にじゅう should still offer 二重: {texts:?}"
    );
    assert!(texts.contains(&"20"), "にじゅう missing 20: {texts:?}");
    assert!(texts.contains(&"２０"), "にじゅう missing ２０: {texts:?}");
    assert!(texts.contains(&"二十"), "にじゅう missing 二十: {texts:?}");
}

#[test]
fn bare_spoken_numbers_defer_to_lexical_homophones() {
    let bytes = number_junk_dictionary();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    for (reading, lexical, numeric_forms) in [
        ("せん", "線", ["1000", "１０００", "千"]),
        ("にじゅう", "二重", ["20", "２０", "二十"]),
        ("さんぜん", "産前", ["3000", "３０００", "三千"]),
    ] {
        let mut converter = Converter::new();
        let candidates = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("conversion");
        let texts: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.text())
            .collect();
        assert_eq!(
            texts.first().copied(),
            Some(lexical),
            "{reading}: {texts:?}"
        );
        for numeric in numeric_forms {
            assert!(
                texts.contains(&numeric),
                "{reading} lost numeric form {numeric}: {texts:?}"
            );
        }
    }
}

#[test]
#[ignore = "real built dictionary; set SAKURA_PHASE2_DICTIONARY"]
fn real_dictionary_keeps_non_initial_allomorphs_out_of_independent_conversion() {
    let path = std::env::var_os("SAKURA_PHASE2_DICTIONARY")
        .expect("SAKURA_PHASE2_DICTIONARY must name a freshly built system.dic");
    let bytes = std::fs::read(&path).expect("read real dictionary");
    let dictionary = Dictionary::parse(&bytes).expect("parse real dictionary");
    let mut converter = Converter::new();

    let candidates = converter
        .convert(&dictionary, "ずかい", ConversionOptions::default())
        .expect("ずかい conversion");
    let ranking = candidates
        .iter()
        .map(|candidate| (candidate.text(), candidate.cost))
        .collect::<Vec<_>>();
    assert_eq!(ranking.first().map(|candidate| candidate.0), Some("図解"));
    for unexpected in ["使い", "遣い", "頭蓋", "図書い"] {
        assert!(
            ranking.iter().all(|candidate| candidate.0 != unexpected),
            "ずかい retained {unexpected}: {ranking:?}"
        );
    }

    let ordinary = converter
        .convert(&dictionary, "つかい", ConversionOptions::default())
        .expect("つかい conversion");
    assert!(ordinary.iter().any(|candidate| candidate.text() == "使い"));
    assert!(ordinary.iter().any(|candidate| candidate.text() == "遣い"));

    for (reading, expected) in [
        ("きづかい", "気遣い"),
        ("こづかい", "小遣い"),
        ("ことばづかい", "言葉遣い"),
    ] {
        let compound = converter
            .convert(&dictionary, reading, ConversionOptions::default())
            .expect("compound conversion");
        assert!(
            compound
                .iter()
                .any(|candidate| candidate.text() == expected),
            "{reading} lost {expected}: {:?}",
            compound
                .iter()
                .map(|candidate| candidate.text())
                .collect::<Vec<_>>()
        );
    }
}
