use std::collections::BTreeSet;
use std::path::Path;

use dictc::{
    attach_entry_details, clear_candidate_list_annotations, compile, compile_with_details,
    extract_entry_details, parse_category_entries, parse_connection, parse_entries,
};
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
fn detail_only_source_attaches_without_mutating_or_replacing_the_candidate() {
    let dictionary = entries("ことば\t言葉\t1\t1\t7600\t-\tpredict\t\n");
    let reviewed = entries("ことば\t言葉\t1\t1\t7600\t-\tpredict\t簡潔な説明です。\n");

    let details = attach_entry_details(&dictionary, &reviewed).expect("exact detached source");

    assert_eq!(details.len(), 1);
    assert_eq!(details[0].description, "簡潔な説明です。");
    assert!(dictionary[0].annotation.is_empty());
    assert_eq!(dictionary[0].word_cost, 7600);
    assert_eq!(dictionary[0].flags, reviewed[0].flags);

    let changed_cost = entries("ことば\t言葉\t1\t1\t7500\t-\tpredict\t簡潔な説明です。\n");
    assert!(attach_entry_details(&dictionary, &changed_cost)
        .unwrap_err()
        .to_string()
        .contains("no longer matches"));
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
    let homophones = data_entries("curated-homophone-details.tsv");
    let system_homophones = data_entries("curated-homophone-system-details.tsv");
    let phrase_targets = data_entries("curated-phrase-target-entries.tsv");
    let general_targets = data_entries("curated-general-target-entries.tsv");
    assert_eq!(phrases.len(), 481);
    assert_eq!(general.len(), 480);
    assert_eq!(homophones.len(), 1_163);
    assert_eq!(system_homophones.len(), 319);
    assert_eq!(phrase_targets.len(), 14);
    assert_eq!(general_targets.len(), 3);

    let mut identities = BTreeSet::new();
    for entry in &phrases {
        assert!(
            identities.insert((
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.left_id,
                entry.right_id,
            )),
            "duplicate curated identity {} -> {} ({}, {})",
            entry.reading,
            entry.surface,
            entry.left_id,
            entry.right_id
        );
        assert_eq!(entry.left_id, 1851);
        assert_eq!(entry.right_id, 1851);
        assert_eq!(entry.word_cost, 7600);
        assert_eq!(entry.prediction_cost, i32::MAX);
        assert_eq!(entry.flags.bits(), 0);
        assert!(!entry.annotation.trim().is_empty());
        assert!(!entry.annotation.contains(['\r', '\n', '\t', '\0']));
    }

    for entry in general.iter().chain(&homophones).chain(&system_homophones) {
        assert!(
            identities.insert((
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.left_id,
                entry.right_id,
            )),
            "duplicate curated identity {} -> {} ({}, {})",
            entry.reading,
            entry.surface,
            entry.left_id,
            entry.right_id
        );
        assert!(!entry.annotation.trim().is_empty());
        assert!(!entry.annotation.contains(['\r', '\n', '\t', '\0']));
    }

    for entry in phrase_targets.iter().chain(&general_targets) {
        assert!(
            identities.insert((
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.left_id,
                entry.right_id,
            )),
            "duplicate entry-only identity {} -> {} ({}, {})",
            entry.reading,
            entry.surface,
            entry.left_id,
            entry.right_id
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
        assert!(
            identities.contains(&(required.0, required.1, 1851, 1851)),
            "missing {required:?}"
        );
    }

    for required in [
        ("ついしょう", "追従", 1841, 1841),
        ("ついじゅう", "追従", 1841, 1841),
        ("ついずい", "追随", 1841, 1841),
        ("たいおう", "対応", 1841, 1841),
        ("いがい", "意外", 1931, 1931),
        ("いがい", "以外", 2163, 2163),
        ("きょうつう", "共通", 1931, 1931),
        ("きょうどう", "共同", 1929, 1929),
        ("いそんせい", "依存性", 1841, 1970),
        ("いにん", "委任", 1841, 1841),
        ("うけおい", "請負", 1841, 1841),
        ("みこうかい", "未公開", 2624, 1841),
        ("さいこうせい", "再構成", 2610, 1841),
        ("ろうしゅつ", "漏出", 1841, 1841),
        ("かいせき", "解析", 1841, 1841),
        ("せいど", "精度", 1851, 1851),
        ("せんこう", "選好", 1841, 1841),
        ("かいふく", "快復", 1841, 1841),
        ("かんけつ", "完結", 1841, 1841),
        ("かんしょう", "鑑賞", 1841, 1841),
        ("かんしょう", "観賞", 1841, 1841),
        ("かんれい", "慣例", 1851, 1851),
        ("きょうそう", "競走", 1841, 1841),
        ("しょくせき", "職責", 1851, 1851),
        ("さくそう", "錯綜", 1841, 1841),
        ("せいさん", "清算", 1841, 1841),
        ("きやく", "規約", 1841, 1841),
        ("ろんきょ", "論拠", 1851, 1851),
        ("かねつ", "過熱", 1841, 1841),
        ("きてい", "既定", 1841, 1841),
        ("きのう", "昨日", 1841, 1841),
        ("きのう", "昨日", 1851, 1851),
        ("きのう", "昨日", 1909, 1909),
        ("じりつ", "自立", 1841, 1841),
        ("せいさく", "政策", 1841, 1841),
        ("たいせい", "大勢", 1851, 1851),
        ("ほそく", "捕捉", 1841, 1841),
    ] {
        assert!(identities.contains(&required), "missing {required:?}");
    }
}

#[test]
fn leftover_bracket_tag_is_stripped_before_the_image_ships() {
    let mut dictionary = parse_category_entries(
        "baked.tsv",
        concat!(
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "きのう\t昨日\t1\t1\t1100\t2300\tpredict\t[calibration] date expression\n",
        ),
    )
    .expect("generated category files may still carry a baked tag");
    assert_eq!(dictionary[0].annotation, "[calibration] date expression");

    clear_candidate_list_annotations(&mut dictionary);
    assert!(dictionary[0].annotation.is_empty());

    let connection = parse_connection(
        "connection.tsv",
        "# license: LicenseRef-Sakura-InHouse\nclasses\t2\ndefault\t7\n",
        false,
    )
    .expect("connection");
    let image = compile(&dictionary, &connection).expect("compiled without a list note");
    let runtime = Dictionary::parse(&image).expect("runtime dictionary");
    let mut matched = None;
    runtime
        .common_prefix_search("きのう", |candidate| {
            matched = Some(candidate.entry);
            false
        })
        .expect("lookup");
    let mut annotation = String::new();
    runtime
        .write_annotation(matched.expect("yesterday"), &mut annotation)
        .expect("candidate annotation");
    assert!(annotation.is_empty());
}

#[test]
fn compile_rejects_a_bracket_tag_that_escaped_the_strip() {
    let dictionary = parse_category_entries(
        "baked.tsv",
        concat!(
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "きのう\t昨日\t1\t1\t1100\t2300\tpredict\t[calibration] date expression\n",
        ),
    )
    .expect("generated category files may still carry a baked tag");
    let connection = parse_connection(
        "connection.tsv",
        "# license: LicenseRef-Sakura-InHouse\nclasses\t2\ndefault\t7\n",
        false,
    )
    .expect("connection");
    let error = compile(&dictionary, &connection).expect_err("bracket tag must not compile");
    assert!(
        error
            .to_string()
            .contains("candidate annotation must not start with '['"),
        "got {error}"
    );
}
