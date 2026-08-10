use std::collections::BTreeSet;
use std::path::Path;

use dictc::{compile_with_details, extract_entry_details, parse_connection, parse_entries};
use sakura_core::dictionary::Dictionary;

const HEADER: &str = concat!(
    "# license: LicenseRef-Sakura-InHouse\n",
    "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n"
);

fn entries(body: &str) -> Vec<dictc::SourceEntry> {
    parse_entries("fixture.tsv", &format!("{HEADER}{body}")).expect("valid fixture")
}

fn data_entries(name: &str) -> Vec<dictc::SourceEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    parse_entries(&path.display().to_string(), &text).expect("valid curated detail source")
}

#[test]
fn reviewed_annotation_becomes_detail_and_leaves_candidate_annotation_empty() {
    let mut dictionary = entries("ことば\t言葉\t1\t1\t7600\t-\t\t簡潔な説明です。\n");
    let reviewed = dictionary.clone();

    let details = extract_entry_details(&mut dictionary, &reviewed).expect("exact source");

    assert_eq!(details.len(), 1);
    assert_eq!(details[0].description, "簡潔な説明です。");
    assert!(details[0].relations.is_empty());
    assert!(dictionary[0].annotation.is_empty());

    let connection = parse_connection(
        "connection.tsv",
        "# license: LicenseRef-Sakura-InHouse\nclasses\t2\ndefault\t7\n",
        false,
    )
    .expect("connection");
    let image = compile_with_details(&dictionary, &connection, &details).expect("compiled image");
    let runtime = Dictionary::parse(&image).expect("runtime dictionary");
    let mut matched = None;
    runtime
        .common_prefix_search("ことば", |candidate| {
            matched = Some((candidate.entry, candidate.entry_index));
            false
        })
        .expect("lookup");
    let (entry, ordinal) = matched.expect("curated entry");
    let mut annotation = String::new();
    runtime
        .write_annotation(entry, &mut annotation)
        .expect("candidate annotation");
    assert!(annotation.is_empty());
    let detail = runtime
        .detail_at(ordinal)
        .expect("detail lookup")
        .expect("curated detail");
    let mut description = String::new();
    detail
        .write_description(&mut description)
        .expect("description");
    assert_eq!(description, "簡潔な説明です。");
}

#[test]
fn changed_cost_or_annotation_fails_closed_without_mutating_entries() {
    let mut dictionary = entries("ことば\t言葉\t1\t1\t7600\t-\t\t簡潔な説明です。\n");
    let reviewed = entries("ことば\t言葉\t1\t1\t7500\t-\t\t簡潔な説明です。\n");

    let error = extract_entry_details(&mut dictionary, &reviewed).unwrap_err();

    assert!(error.to_string().contains("no longer matches"));
    assert_eq!(dictionary[0].annotation, "簡潔な説明です。");
}

#[test]
fn duplicate_or_missing_reviewed_identity_fails_closed() {
    let mut dictionary = entries("ことば\t言葉\t1\t1\t7600\t-\t\t簡潔な説明です。\n");
    let duplicate = entries(
        "ことば\t言葉\t1\t1\t7600\t-\t\t簡潔な説明です。\nことば\t言葉\t1\t1\t7600\t-\t\t簡潔な説明です。\n",
    );
    let missing = entries("べつ\t別\t1\t1\t7600\t-\t\t別の説明です。\n");

    assert!(extract_entry_details(&mut dictionary, &duplicate)
        .unwrap_err()
        .to_string()
        .contains("duplicate reviewed detail"));
    assert!(extract_entry_details(&mut dictionary, &missing)
        .unwrap_err()
        .to_string()
        .contains("is not in the final dictionary"));
}

#[test]
fn empty_description_is_rejected() {
    let mut dictionary = entries("ことば\t言葉\t1\t1\t7600\t-\t\t\n");
    let reviewed = dictionary.clone();

    assert!(extract_entry_details(&mut dictionary, &reviewed)
        .unwrap_err()
        .to_string()
        .contains("must not be empty"));
}

#[test]
fn checked_in_curated_sources_have_the_reviewed_release_shape() {
    let phrases = data_entries("curated-phrases.tsv");
    let general = data_entries("curated-general-details.tsv");
    let phrase_targets = data_entries("curated-phrase-target-entries.tsv");
    let general_targets = data_entries("curated-general-target-entries.tsv");
    assert_eq!(phrases.len(), 481);
    assert_eq!(general.len(), 1);
    assert_eq!(phrase_targets.len(), 14);
    assert_eq!(general_targets.len(), 3);

    let mut identities = BTreeSet::new();
    for entry in phrases.iter().chain(&general) {
        assert!(
            identities.insert((entry.reading.as_str(), entry.surface.as_str())),
            "duplicate curated pair {} -> {}",
            entry.reading,
            entry.surface
        );
        assert_eq!(entry.left_id, 1851);
        assert_eq!(entry.right_id, 1851);
        assert_eq!(entry.word_cost, 7600);
        assert_eq!(entry.prediction_cost, i32::MAX);
        assert_eq!(entry.flags.bits(), 0);
        assert!(!entry.annotation.trim().is_empty());
        assert!(!entry.annotation.contains(['\r', '\n', '\t', '\0']));
    }

    for entry in phrase_targets.iter().chain(&general_targets) {
        assert!(
            identities.insert((entry.reading.as_str(), entry.surface.as_str())),
            "duplicate entry-only pair {} -> {}",
            entry.reading,
            entry.surface
        );
        assert_eq!(entry.left_id, 1851);
        assert_eq!(entry.right_id, 1851);
        assert_eq!(entry.word_cost, 7600);
        assert_eq!(entry.prediction_cost, i32::MAX);
        assert_eq!(entry.flags.bits(), 0);
        assert!(entry.annotation.is_empty());
    }

    for required in [
        ("このおやにしてこのこあり", "この親にしてこの子あり"),
        ("いつもありがとうございます", "いつもありがとうございます"),
        ("ごかくにんをおねがいいたします", "ご確認をお願いいたします"),
        ("こんきょしりょう", "根拠資料"),
        ("いそがばまわれ", "急がば回れ"),
        ("はいしょう", "拝承"),
    ] {
        assert!(identities.contains(&required), "missing {required:?}");
    }
}
