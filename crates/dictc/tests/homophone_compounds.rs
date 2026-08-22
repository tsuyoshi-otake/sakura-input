use std::collections::BTreeSet;
use std::path::Path;

use dictc::{compile_with_details, parse_connection, parse_entries, SourceDetail};
use sakura_core::conversion::{ConversionOptions, Converter};
use sakura_core::dictionary::Dictionary;
use sakura_core::ConversionMethod;

#[test]
fn priority_homophone_fixture_keeps_the_reviewed_groups() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("eval")
        .join("corpus")
        .join("behavioral")
        .join("homophone-details")
        .join("fixture.tsv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let mut cases = BTreeSet::new();
    let mut readings = BTreeSet::new();
    let mut candidate_surfaces = 0;

    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') || line.starts_with("case_id\t") {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        assert_eq!(
            columns.len(),
            4,
            "malformed fixture line {}",
            line_index + 1
        );
        assert!(cases.insert(columns[0]), "duplicate case id {}", columns[0]);
        assert!(
            readings.insert(columns[1]),
            "duplicate priority reading {}",
            columns[1]
        );
        assert_eq!(columns[3], "exact_candidate_detail");
        let surfaces = columns[2].split(" / ").collect::<BTreeSet<_>>();
        assert!(
            surfaces.len() >= 2,
            "too few alternatives on {}",
            columns[0]
        );
        assert!(surfaces.iter().all(|surface| !surface.is_empty()));
        candidate_surfaces += surfaces.len();
    }

    assert_eq!(cases.len(), 10);
    assert_eq!(readings.len(), 10);
    assert_eq!(candidate_surfaces, 23);
    assert_eq!(
        readings,
        BTreeSet::from([
            "かいてい",
            "きてい",
            "きのう",
            "けっさい",
            "じりつ",
            "せいさく",
            "たいしょう",
            "ほしょう",
            "ほそく",
            "ようけん",
        ])
    );
}

#[test]
fn submitted_compound_fixture_is_bounded_and_explicit() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("eval")
        .join("corpus")
        .join("behavioral")
        .join("homophone-compounds")
        .join("fixture.tsv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let mut cases = BTreeSet::new();
    let mut observations = 0;
    let mut contexts = 0;
    let mut observation_surfaces = 0;

    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') || line.starts_with("case_id\t") {
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        assert_eq!(
            columns.len(),
            7,
            "malformed fixture line {}",
            line_index + 1
        );
        assert!(cases.insert(columns[0]), "duplicate case id {}", columns[0]);
        assert!(!columns[2].is_empty(), "empty reading on {}", columns[0]);
        let surfaces = columns[3].split(" / ").collect::<BTreeSet<_>>();
        assert!(
            surfaces.len() >= 2,
            "too few alternatives on {}",
            columns[0]
        );
        assert!(surfaces.iter().all(|surface| !surface.is_empty()));

        match columns[1] {
            "candidate_observation" => {
                observations += 1;
                observation_surfaces += surfaces.len();
                assert!(columns[4..].iter().all(|value| *value == "-"));
            }
            "context_required" => {
                contexts += 1;
                assert!(!columns[4].is_empty());
                assert!(!columns[5].is_empty());
                assert!(surfaces.contains(columns[6]));
            }
            scope => panic!("unknown fixture scope {scope}"),
        }
    }

    assert_eq!(cases.len(), 160);
    assert_eq!(observations, 158);
    assert_eq!(observation_surfaces, 330);
    assert_eq!(contexts, 2);
    assert!(!text.contains("昨日製"));
    assert!(!text.contains("仕様方法"));
}

#[test]
fn synthesized_compound_does_not_inherit_a_component_detail() {
    let source = concat!(
        "# license: LicenseRef-Sakura-InHouse\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "きのう\t機能\t1\t1\t100\t-\t\t\n",
        "きのう\t昨日\t1\t1\t200\t-\t\t\n",
        "てすと\tテスト\t1\t1\t100\t-\t\t\n",
    );
    let entries = parse_entries("compound.tsv", source).expect("entries");
    let connection = parse_connection(
        "connection.tsv",
        "# license: LicenseRef-Sakura-InHouse\nclasses\t2\ndefault\t0\n",
        false,
    )
    .expect("connection");
    let image = compile_with_details(
        &entries,
        &connection,
        &[SourceDetail {
            reading: "きのう".into(),
            surface: "機能".into(),
            left_id: 1,
            right_id: 1,
            description: "物や仕組みが果たす働き。".into(),
            relations: Vec::new(),
        }],
    )
    .expect("dictionary image");
    let dictionary = Dictionary::parse(&image).expect("dictionary");
    let mut converter = Converter::new();
    let options = ConversionOptions {
        max_candidates: 9,
        method: ConversionMethod::MultiSegment,
        it_bias_per_mille: 0,
        max_it_boost: 0,
        initial_right_id: 0,
        ..ConversionOptions::default()
    };

    let word = converter
        .convert(&dictionary, "きのう", options)
        .expect("word conversion");
    let exact = word
        .iter()
        .find(|candidate| candidate.text() == "機能")
        .expect("exact word");
    let ordinal = exact.system_entry_index().expect("exact entry ordinal");
    assert!(dictionary
        .detail_at(ordinal as usize)
        .expect("detail lookup")
        .is_some());

    let compound = converter
        .convert(&dictionary, "きのうてすと", options)
        .expect("compound conversion");
    let synthesized = compound
        .iter()
        .find(|candidate| candidate.text() == "機能テスト")
        .expect("synthesized compound");
    assert_eq!(synthesized.segments().len(), 2);
    assert_eq!(synthesized.system_entry_index(), None);
}
