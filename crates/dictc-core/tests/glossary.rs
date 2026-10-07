use dictc_core::glossary::{
    detail_sources, import, normalize_reading, parse_part, GlossaryTerm, ImportResult, Importer,
    OverlayDefaults,
};
use dictc_core::{
    entries_to_tsv, parse_entries, parse_mozc_connection, parse_mozc_entries, ConnectionMatrix,
    SourceEntry,
};
use sakura_core::dictionary::{DetailRelationKind, EntryFlags};

const PART: &str = r#"[
  {"term":"Docker","reading":"ドッカー","aliases":["docker","Docker"],"senses":[{"definition":"コンテナを扱う基盤。","domain":"containers","keywords":["image"]}],"future":{"enabled":true}},
  {"term":"活性・非活性","reading":"かっせい・ひかっせい","senses":[{"definition":"状態の区別。"}]},
  {"term":"未読語","senses":[{"definition":"読みが未整備。"}]},
  {"term":"かな","senses":[{"definition":"自分自身を読みにできる。"}]}
]"#;

const MOZC: &str = "どっかー\t7\t8\t1000\tDocker\n";

/// A 16-class grammar whose connections are all free, so a standalone total
/// is the word cost less its IT reduction. Class 3 is a proper noun, 4 a verb
/// in a form that needs a following word, 5 a verb in its dictionary form, 9
/// an adverb, 14 a case particle and 15 a noun suffix; every other class is a
/// common noun.
fn grammar() -> (ConnectionMatrix, Vec<String>) {
    let connection = parse_mozc_connection(
        "connection.txt",
        &format!("16\n{}", "0\n".repeat(256)),
        false,
    )
    .expect("connection fixture");
    let pos_features = (0..16)
        .map(|class| {
            match class {
                0 => "BOS/EOS,*,*",
                3 => "名詞,固有名詞,一般,*,*,*,*",
                4 => "動詞,自立,*,*,五段・バ行,連用タ接続,*",
                5 => "動詞,自立,*,*,五段・ラ行,基本形,*",
                9 => "副詞,一般,*",
                14 => "助詞,格助詞,一般",
                15 => "名詞,接尾,一般",
                _ => "名詞,一般,*",
            }
            .to_owned()
        })
        .collect();
    (connection, pos_features)
}

fn import_fixture(
    terms: &[GlossaryTerm],
    mozc: &[SourceEntry],
    defaults: OverlayDefaults,
) -> ImportResult {
    let (connection, pos_features) = grammar();
    import(terms, mozc, &connection, &pos_features, defaults).expect("overlay")
}

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
    assert_eq!(terms[0].senses[0].keywords, ["image"]);
}

#[test]
fn details_preserve_definition_and_only_link_unique_keywords() {
    let terms = parse_part(
        "detail.json",
        r#"[
          {"term":"Docker","reading":"どっかー","aliases":["docker"],"senses":[{"definition":"原文\nを保存する。","keywords":["Container","Ambiguous"]}]},
          {"term":"Container","reading":"こんてな","senses":[{"definition":"target"}]},
          {"term":"Ambiguous","reading":"あんび","senses":[{"definition":"one"}]},
          {"term":"Ambiguous","reading":"あんびぐ","senses":[{"definition":"two"}]}
        ]"#,
    )
    .expect("terms");
    let imported = import_fixture(&terms, &[], defaults());
    let details = detail_sources(&terms, &imported.entries);
    let docker = details
        .iter()
        .find(|detail| detail.surface == "Docker" && detail.reading == "どっかー")
        .expect("Docker detail");
    assert_eq!(docker.description, "原文\nを保存する。");
    assert!(docker
        .relations
        .iter()
        .any(|relation| relation.kind == DetailRelationKind::Alias && relation.target == "docker"));
    assert!(docker.relations.iter().any(|relation| {
        relation.kind == DetailRelationKind::Related && relation.target == "Container"
    }));
    assert!(!docker
        .relations
        .iter()
        .any(|relation| relation.target == "Ambiguous"));
    assert!(docker.relations.iter().all(|relation| {
        !matches!(
            relation.kind,
            DetailRelationKind::Synonym | DetailRelationKind::Antonym
        )
    }));
}

#[test]
fn detail_sources_indexes_a_large_lexicon_once() {
    let term_json = (0..512)
        .map(|index| {
            format!(r#"{{"term":"term-{index}","senses":[{{"definition":"definition-{index}"}}]}}"#)
        })
        .collect::<Vec<_>>()
        .join(",");
    let terms = parse_part("scale.json", &format!("[{term_json}]")).expect("scale glossary terms");
    let mut tsv = String::from(
        "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
    );
    for index in 0..20_000 {
        tsv.push_str(&format!(
            "reading-{index}\tunmatched-{index}\t1\t1\t1\t1\t\t\n"
        ));
    }
    for index in 0..512 {
        tsv.push_str(&format!("reading-{index}\tterm-{index}\t1\t1\t1\t1\t\t\n"));
    }
    let entries = parse_entries("scale.tsv", &tsv).expect("scale entries");

    let details = detail_sources(&terms, &entries);
    assert_eq!(details.len(), 512);
    assert!(details
        .iter()
        .all(|detail| detail.surface.starts_with("term-")));
}

#[test]
fn importer_matches_mozc_then_uses_visible_shape_defaults() {
    let terms = parse_part("ja_part1.json", PART).expect("glossary part");
    let mozc = parse_mozc_entries("dictionary00.txt", MOZC).expect("Mozc fixture");
    let imported = import_fixture(&terms, &mozc, defaults());

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
    assert_eq!(docker.annotation, "");

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
fn importer_keeps_definition_brackets_in_typed_details_not_inline_annotations() {
    let terms = parse_part(
        "ja_part1.json",
        r#"[
          {
            "term": "Brackets",
            "reading": "brackets",
            "senses": [{
              "definition": "Keep [literal] definition text.",
              "domain": "it-basics"
            }]
          }
        ]"#,
    )
    .expect("glossary part");
    let imported = import_fixture(&terms, &[], defaults());
    let entry = imported
        .entries
        .iter()
        .find(|entry| entry.surface == "Brackets" && entry.reading == "brackets")
        .expect("bracketed definition entry");

    assert_eq!(entry.annotation, "");
    let details = detail_sources(&terms, &imported.entries);
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].description, "Keep [literal] definition text.");
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
    let (connection, pos_features) = grammar();
    let imported = importer.finish(&connection, &pos_features);
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
    let imported = import_fixture(&terms, &mozc, defaults());
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
    let imported = import_fixture(&terms, &mozc, defaults());
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
    let imported = import_fixture(&terms, &mozc, defaults());

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
    let imported = import_fixture(&terms, &mozc, defaults());

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
    let imported = import_fixture(&terms, &[], defaults());

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

/// One glossary term as glossary JSON.
fn term(surface: &str, reading: &str, aliases: &[&str]) -> String {
    let aliases: Vec<String> = aliases.iter().map(|alias| format!("\"{alias}\"")).collect();
    format!(
        r#"{{"term":"{surface}","reading":"{reading}","aliases":[{}],"senses":[{{"definition":"d"}}]}}"#,
        aliases.join(",")
    )
}

/// Imports `terms` against `mozc`, which serves as both the Mozc match source
/// and the pre-overlay dictionary that decides who owns each reading.
fn yield_fixture(terms: &[String], mozc: &str, base_word_cost: i32) -> ImportResult {
    let terms = parse_part("yield.json", &format!("[{}]", terms.join(","))).expect("terms");
    let mozc = parse_mozc_entries("yield.txt", mozc).expect("Mozc rows");
    import_fixture(
        &terms,
        &mozc,
        OverlayDefaults {
            base_word_cost,
            ..defaults()
        },
    )
}

fn overlay_entry<'a>(imported: &'a ImportResult, reading: &str, surface: &str) -> &'a SourceEntry {
    imported
        .entries
        .iter()
        .find(|entry| entry.reading == reading && entry.surface == surface)
        .unwrap_or_else(|| panic!("no overlay entry {reading}/{surface}"))
}

const IKOU: &str = "いこう\t1\t1\t4000\t以降\nいこう\t1\t1\t4300\t移行\n";

#[test]
fn glossary_discount_yields_rank_one_to_the_upstream_owner() {
    let imported = yield_fixture(&[term("移行", "いこう", &[])], IKOU, 4_800);

    // 移行 matches Mozc at 4300, so its discounted 3900 (3510 after the IT
    // reduction) would outrank 以降 at 4000; it lands 60 behind instead.
    assert_eq!(
        imported.report.upstream_yields,
        ["いこう/移行: 3900 -> 4511, below 以降"]
    );
    let yielded = overlay_entry(&imported, "いこう", "移行");
    assert_eq!((yielded.word_cost, yielded.prediction_cost), (4_511, 4_811));
    assert_eq!(
        yielded.flags,
        EntryFlags::IT | EntryFlags::PREDICTION | EntryFlags::READING_YIELD,
        "the runtime needs to know this edge yielded (Issue #291)"
    );
}

#[test]
fn yielding_edges_keep_their_order_right_behind_the_owner() {
    let imported = yield_fixture(
        &[term("移行", "いこう", &["移行処理", "Migration"])],
        IKOU,
        4_000,
    );

    assert_eq!(
        imported.report.upstream_yields,
        [
            "いこう/移行: 3900 -> 4511, below 以降",
            "いこう/移行処理: 4260 -> 4512, below 以降",
        ]
    );
    assert_eq!(
        overlay_entry(&imported, "いこう", "Migration").word_cost,
        6_435,
        "an edge already behind the owner keeps its price"
    );
    for surface in ["移行", "移行処理", "Migration"] {
        assert!(
            overlay_entry(&imported, "いこう", surface)
                .flags
                .contains(EntryFlags::READING_YIELD),
            "every glossary edge of a yielded reading stays behind the owner at runtime: {surface}"
        );
    }
}

#[test]
fn a_glossary_edge_already_behind_the_owner_does_not_yield() {
    let imported = yield_fixture(
        &[term("移行", "いこう", &[])],
        "いこう\t1\t1\t4000\t以降\nいこう\t1\t1\t5000\t移行\n",
        4_800,
    );

    assert!(imported.report.upstream_yields.is_empty());
    let kept = overlay_entry(&imported, "いこう", "移行");
    assert_eq!(kept.word_cost, 4_600);
    assert_eq!(kept.flags, EntryFlags::IT | EntryFlags::PREDICTION);
}

#[test]
fn a_glossary_that_supplies_the_owner_does_not_yield() {
    let imported = yield_fixture(&[term("以降", "いこう", &["移行"])], IKOU, 4_800);

    assert!(imported.report.upstream_yields.is_empty());
    assert_eq!(overlay_entry(&imported, "いこう", "移行").word_cost, 3_900);
}

#[test]
fn a_reading_without_an_owning_word_does_not_yield() {
    for (case, reading, surface, mozc) in [
        (
            "a particle-initial leader is not a word",
            "でんち",
            "電池",
            "でんち\t14\t14\t4000\tで賃\nでんち\t1\t1\t4300\t電池\n",
        ),
        (
            "a noun's hiragana echo is a spelling fallback",
            "けつごう",
            "結合",
            "けつごう\t1\t1\t4000\tけつごう\nけつごう\t1\t1\t4300\t結合\n",
        ),
        (
            "a cheaper split shows something else first",
            "たいま",
            "対魔",
            "たいま\t1\t1\t6000\t大麻\nた\t1\t1\t1000\tた\nいま\t1\t1\t2000\t今\nたいま\t1\t1\t6300\t対魔\n",
        ),
        (
            "a proper noun shares the reading by coincidence of name",
            "にっとう",
            "日当",
            "にっとう\t3\t3\t4000\t日東\nにっとう\t1\t1\t4300\t日当\n",
        ),
        (
            "a verb form that needs a following word does not stand alone",
            "ころん",
            "湖論",
            "ころん\t4\t4\t4000\t転ん\nころん\t1\t1\t4300\t湖論\n",
        ),
    ] {
        let imported = yield_fixture(&[term(surface, reading, &[])], mozc, 4_800);

        assert!(imported.report.upstream_yields.is_empty(), "{case}");
        let mozc_cost = if reading == "たいま" { 6_300 } else { 4_300 };
        assert_eq!(
            overlay_entry(&imported, reading, surface).word_cost,
            mozc_cost - 400,
            "{case}"
        );
    }
}

#[test]
fn owning_words_include_dictionary_form_verbs_and_kana_adverbs() {
    for (reading, surface, mozc, report) in [
        (
            "ぬる",
            "濡",
            "ぬる\t5\t5\t4000\t塗る\nぬる\t1\t1\t4300\t濡\n",
            "ぬる/濡: 3900 -> 4511, below 塗る",
        ),
        (
            "ようやく",
            "要約",
            "ようやく\t9\t9\t4000\tようやく\nようやく\t1\t1\t4100\t要約\n",
            "ようやく/要約: 3700 -> 4511, below ようやく",
        ),
    ] {
        let imported = yield_fixture(&[term(surface, reading, &[])], mozc, 4_800);

        assert_eq!(imported.report.upstream_yields, [report]);
    }
}

#[test]
fn a_non_initial_upstream_edge_does_not_own_the_reading() {
    let terms =
        parse_part("yield.json", &format!("[{}]", term("移行", "いこう", &[]))).expect("terms");
    let mut mozc = parse_mozc_entries(
        "yield.txt",
        "いこう\t1\t1\t3500\t以降\nいこう\t1\t1\t4000\t意向\nいこう\t1\t1\t4300\t移行\n",
    )
    .expect("Mozc rows");
    mozc.iter_mut()
        .find(|entry| entry.surface == "以降")
        .expect("以降")
        .flags = EntryFlags::NON_INITIAL;
    let imported = import_fixture(&terms, &mozc, defaults());

    assert_eq!(
        imported.report.upstream_yields,
        ["いこう/移行: 3900 -> 4511, below 意向"]
    );
}

#[test]
fn a_cheaper_split_with_the_owners_spelling_sets_the_owner_total() {
    let imported = yield_fixture(
        &[term("測りやすい", "はかりやすい", &[])],
        "はかりやすい\t1\t1\t7000\t図りやすい\nはかり\t1\t1\t3000\t図り\n\
         やすい\t1\t1\t3000\tやすい\nはかりやすい\t1\t1\t7000\t測りやすい\n",
        4_800,
    );

    // 図り+やすい totals 6000, below the whole-reading 図りやすい at 7000.
    assert_eq!(
        imported.report.upstream_yields,
        ["はかりやすい/測りやすい: 6600 -> 6733, below 図りやすい"]
    );
}

#[test]
fn a_glossary_term_on_part_of_the_reading_can_take_the_owners_place() {
    let mozc = "からむ\t5\t5\t5000\t絡む\nか\t1\t1\t1000\t蚊\nからむ\t1\t1\t5200\t殻無\n";
    let alone = yield_fixture(&[term("殻無", "からむ", &[])], mozc, 4_800);
    assert_eq!(
        alone.report.upstream_yields,
        ["からむ/殻無: 4800 -> 5622, below 絡む"]
    );

    let composed = yield_fixture(
        &[term("殻無", "からむ", &[]), term("ラム", "らむ", &[])],
        mozc,
        4_800,
    );
    assert!(
        composed.report.upstream_yields.is_empty(),
        "蚊+ラム, not 絡む, would rank first once 殻無 stepped back"
    );
    assert_eq!(overlay_entry(&composed, "からむ", "殻無").word_cost, 4_800);
}

#[test]
fn a_glossary_only_rank_one_does_not_yield() {
    let imported = yield_fixture(
        &[term("移行", "いこう", &[])],
        "いこう\t1\t1\t4000\t以降\n",
        3_000,
    );

    assert!(imported.report.upstream_yields.is_empty());
    assert_eq!(overlay_entry(&imported, "いこう", "移行").word_cost, 3_070);
}

#[test]
fn a_loanword_spelled_as_its_reading_keeps_rank_one() {
    let imported = yield_fixture(
        &[term("ヌル", "ぬる", &["ナル"])],
        "ぬる\t5\t5\t3374\t塗る\nぬる\t1\t1\t4860\tヌル\n",
        4_800,
    );

    assert!(
        imported.report.upstream_yields.is_empty(),
        "a lifted ヌル would also fall behind in ヌルチェック and ナルポインタ"
    );
    assert_eq!(overlay_entry(&imported, "ぬる", "ヌル").word_cost, 1_460);
    assert_eq!(overlay_entry(&imported, "ぬる", "ナル").word_cost, 4_990);
}
