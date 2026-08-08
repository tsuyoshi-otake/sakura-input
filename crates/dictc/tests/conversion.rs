use dictc::{compile, parse_connection, parse_entries};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::Dictionary;
use sakura_core::user_dictionary::UserDictionary;

const ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
ぷるりくえすと\tPull request\t1\t1\t120\t80\tit,predict\tIT term\n\
きょう\t今日\t1\t1\t100\t-\t\t\n\
は\tは\t1\t1\t100\t-\t\t\n\
きょうは\t今日は\t1\t1\t500\t-\t\t\n\
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
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
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
fn english_surfaces_offer_identifier_case_candidates() {
    let bytes = fixture();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut converter = Converter::new();
    let candidates = converter
        .convert(&dictionary, "ぷるりくえすと", ConversionOptions::default())
        .expect("conversion");

    for expected in [
        "Pull request",
        "pullRequest",
        "pull_request",
        "PULL_REQUEST",
        "pull-request",
    ] {
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.text() == expected),
            "missing generated candidate {expected}"
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
                it_bias_per_mille: 0,
                max_it_boost: 0,
                initial_right_id: 0,
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
                it_bias_per_mille: 200,
                max_it_boost: 800,
                initial_right_id: 0,
            },
        )
        .expect("technical");
    assert_eq!(technical[0].text(), "関数");
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
