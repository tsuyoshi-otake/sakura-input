use dictc::glossary::{import, normalize_reading, parse_part, Importer, OverlayDefaults};
use dictc::{entries_to_tsv, parse_entries, parse_mozc_entries};
use sakura_core::dictionary::EntryFlags;

const PART: &str = r#"[
  {"term":"Docker","reading":"ドッカー","aliases":["docker","Docker"],"senses":[{"definition":"コンテナを扱う基盤。","domain":"containers","keywords":["image"]}],"future":{"enabled":true}},
  {"term":"活性・非活性","reading":"かっせい・ひかっせい","senses":[{"definition":"状態の区別。"}]},
  {"term":"未読語","senses":[{"definition":"読みが未整備。"}]},
  {"term":"かな","senses":[{"definition":"自分自身を読みにできる。"}]}
]"#;

const MOZC: &str = "どっかー\t7\t8\t1000\tDocker\n";

fn defaults() -> OverlayDefaults {
    OverlayDefaults {
        katakana_left_id: 10,
        katakana_right_id: 11,
        ascii_left_id: 12,
        ascii_right_id: 13,
        base_word_cost: 4_800,
    }
}

#[test]
fn parser_accepts_the_glossary_schema_and_skips_future_fields() {
    let terms = parse_part("ja_part1.json", PART).expect("glossary part");
    assert_eq!(terms.len(), 4);
    assert_eq!(terms[0].term, "Docker");
    assert_eq!(terms[0].reading.as_deref(), Some("ドッカー"));
    assert_eq!(terms[0].aliases, ["docker", "Docker"]);
    assert_eq!(terms[0].senses[0].domain.as_deref(), Some("containers"));
}

#[test]
fn importer_matches_mozc_then_uses_visible_shape_defaults() {
    let terms = parse_part("ja_part1.json", PART).expect("glossary part");
    let mozc = parse_mozc_entries("dictionary00.txt", MOZC).expect("Mozc fixture");
    let imported = import(&terms, &mozc, defaults()).expect("overlay");

    assert_eq!(imported.report.terms, 4);
    assert_eq!(imported.report.surfaces, 6);
    assert_eq!(imported.report.ascii_aliases, 2);
    assert_eq!(imported.report.ascii_only_terms, 0);
    assert_eq!(imported.report.matched_to_mozc, 1);
    assert_eq!(imported.report.defaulted, 5);
    assert_eq!(imported.report.duplicate_surfaces, 1);
    assert_eq!(imported.report.gaps, ["未読語: missing reading"]);

    let docker = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "Docker" && entry.reading == "どっかー")
        .expect("Docker");
    assert_eq!(docker.reading, "どっかー");
    assert_eq!((docker.left_id, docker.right_id), (7, 8));
    assert_eq!(docker.word_cost, 600);
    assert!(docker.flags.contains(EntryFlags::IT));
    assert!(docker.flags.contains(EntryFlags::PREDICTION));
    assert_eq!(docker.annotation, "[containers] コンテナを扱う基盤。");

    let alias = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "docker" && entry.reading == "どっかー")
        .expect("English alias");
    assert_eq!((alias.left_id, alias.right_id), (12, 13));
    assert_eq!(
        alias.word_cost, 7_130,
        "an unmatched ASCII alias stays available without outranking native forms"
    );

    let shifted_canonical = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "Docker" && entry.reading == "docker")
        .expect("Shift+Docker reading");
    assert_eq!(
        (shifted_canonical.left_id, shifted_canonical.right_id),
        (12, 13)
    );
    assert_eq!(shifted_canonical.word_cost, 7_010);

    let shifted_alias = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "docker" && entry.reading == "docker")
        .expect("Shift+docker alias reading");
    assert_eq!(shifted_alias.word_cost, 7_130);

    assert!(
        imported
            .entries
            .iter()
            .all(|entry| entry.surface != "ドッカー"),
        "unattested mechanical katakana must not enter the overlay"
    );

    let phrase = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "活性・非活性")
        .expect("middle-dot phrase");
    assert_eq!(phrase.reading, "かっせいひかっせい");
    assert_eq!((phrase.left_id, phrase.right_id), (10, 11));
}

#[test]
fn importer_streams_shards_and_retains_the_lowest_cost_match() {
    let terms = parse_part("ja_part1.json", PART).expect("glossary part");
    let first = parse_mozc_entries("dictionary00.txt", "どっかー\t1\t2\t2200\tDocker\n")
        .expect("first shard");
    let second = parse_mozc_entries(
        "dictionary01.txt",
        "どっかー\t7\t8\t1000\tDocker\n\u{1}invalid\t7\t8\t10\tvalue\n",
    );
    assert!(second.is_err(), "control characters remain observable");
    let second = parse_mozc_entries(
        "dictionary01.txt",
        "どっかー\t7\t8\t1000\tDocker\nかんけいない\t7\t8\t10\tvalue\n",
    )
    .expect("second shard");

    let mut importer = Importer::new(&terms, defaults()).expect("importer");
    importer.match_mozc(&first);
    importer.match_mozc(&second);
    let imported = importer.finish();
    let docker = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "Docker" && entry.reading == "どっかー")
        .expect("Docker");
    assert_eq!(
        (docker.left_id, docker.right_id, docker.word_cost),
        (7, 8, 600)
    );
    assert_eq!(imported.report.matched_to_mozc, 1);
}

#[test]
fn generated_overlay_tsv_round_trips_through_the_strict_parser() {
    let terms = parse_part("ja_part1.json", PART).expect("glossary part");
    let mozc = parse_mozc_entries("dictionary00.txt", MOZC).expect("Mozc fixture");
    let imported = import(&terms, &mozc, defaults()).expect("overlay");
    let tsv =
        entries_to_tsv(&imported.entries, "LicenseRef-Sakura-InHouse").expect("generated TSV");
    let reparsed = parse_entries("it-terms.tsv", &tsv).expect("strict parser accepts output");

    assert_eq!(reparsed.len(), imported.entries.len());
    assert_eq!(
        reparsed
            .iter()
            .map(|entry| (&entry.reading, &entry.surface, entry.flags.bits()))
            .collect::<Vec<_>>(),
        imported
            .entries
            .iter()
            .map(|entry| (&entry.reading, &entry.surface, entry.flags.bits()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn inherited_cost_boost_is_clamped_at_zero() {
    let terms = parse_part(
        "small.json",
        r#"[{"term":"Docker","reading":"どっかー","senses":[{"definition":"container"}]}]"#,
    )
    .expect("term");
    let mozc = parse_mozc_entries("small.txt", "どっかー\t1\t1\t100\tDocker\n").expect("Mozc row");
    let imported = import(&terms, &mozc, defaults()).expect("overlay");
    let docker = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "Docker" && entry.reading == "どっかー")
        .expect("Docker");
    assert_eq!(docker.word_cost, 0);
    let tsv =
        entries_to_tsv(&imported.entries, "LicenseRef-Sakura-InHouse").expect("non-negative TSV");
    parse_entries("small.tsv", &tsv).expect("strict parser accepts clamped cost");
}

#[test]
fn phonetic_spelling_is_synthesized_and_beats_a_semantic_alias_generically() {
    let terms = parse_part(
        "technical.json",
        r#"[{"term":"Compiler tool","reading":"こんぱいら","aliases":["翻訳器","コンパイラ"],"senses":[{"definition":"program translator"}]}]"#,
    )
    .expect("term");
    let mozc = parse_mozc_entries(
        "technical.txt",
        "こんぱいら\t1\t1\t6000\tコンパイラ\nこんぱいら\t1\t1\t4600\t翻訳器\n",
    )
    .expect("Mozc rows");
    let imported = import(&terms, &mozc, defaults()).expect("overlay");

    let phonetic = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "コンパイラ")
        .expect("synthesized Mozc-backed phonetic surface");
    let semantic = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "翻訳器")
        .expect("semantic alias");
    assert_eq!((phonetic.left_id, phonetic.right_id), (1, 1));
    assert!(phonetic.word_cost < semantic.word_cost);
    assert!(phonetic.flags.contains(EntryFlags::IT));
}

#[test]
fn ascii_term_gains_a_phonetic_surface_only_when_mozc_attests_it() {
    let terms = parse_part(
        "technical.json",
        r#"[{"term":"Build cache","reading":"びるどきゃっしゅ","senses":[{"definition":"cached build output"}]}]"#,
    )
    .expect("term");
    let mozc = parse_mozc_entries(
        "technical.txt",
        "びるどきゃっしゅ\t7\t8\t6000\tビルドキャッシュ\n",
    )
    .expect("Mozc row");
    let imported = import(&terms, &mozc, defaults()).expect("overlay");

    let phonetic = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "ビルドキャッシュ")
        .expect("Mozc-attested synthesized spelling");
    assert_eq!((phonetic.left_id, phonetic.right_id), (7, 8));
    assert_eq!(phonetic.word_cost, 2_600);
}

#[test]
fn ascii_only_terms_get_shift_readings_without_becoming_import_gaps() {
    let terms = parse_part(
        "technical.json",
        r#"[
            {"term":"Claude","senses":[{"definition":"assistant"}]},
            {"term":"Claude Code","senses":[{"definition":"coding assistant"}]}
        ]"#,
    )
    .expect("ASCII-only terms");
    let imported = import(&terms, &[], defaults()).expect("overlay");

    assert_eq!(imported.report.ascii_aliases, 3);
    assert_eq!(imported.report.ascii_only_terms, 2);
    assert!(imported.report.gaps.is_empty());
    assert!(imported
        .entries
        .iter()
        .any(|entry| entry.reading == "claude" && entry.surface == "Claude"));
    assert!(imported
        .entries
        .iter()
        .any(|entry| entry.reading == "claude" && entry.surface == "Claude Code"));
    assert!(imported
        .entries
        .iter()
        .any(|entry| entry.reading == "claudecode" && entry.surface == "Claude Code"));
}

#[test]
fn reading_normalization_removes_metadata_and_converts_katakana() {
    assert_eq!(
        normalize_reading("クー・バネティス"),
        Some("くーばねてぃす".into())
    );
    assert_eq!(
        normalize_reading("もにたりんぐ（おーしーあい）"),
        Some("もにたりんぐ".into())
    );
    assert_eq!(normalize_reading("not-kana"), None);
}

#[test]
fn parser_decodes_surrogate_pairs_and_rejects_malformed_json() {
    let escaped =
        r#"[{"term":"\ud83d\ude80","reading":"ろけっと","senses":[{"definition":"rocket"}]}]"#;
    let terms = parse_part("escaped.json", escaped).expect("escaped scalar");
    assert_eq!(terms[0].term, "🚀");

    let error = parse_part("broken.json", "[{\"term\":}]").expect_err("bad JSON");
    assert!(error.to_string().contains("JSON"));
}
