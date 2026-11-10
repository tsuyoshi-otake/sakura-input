use dictc_core::{compile, parse_mozc_connection, parse_mozc_entries};
use sakura_core::conversion::{
    IT_SUFFIX_CLASS_RANGE, PLAIN_NOUN_STEM_RANGES, SOU_STEM_CLAIM_RANGES,
};
use sakura_core::dictionary::{Dictionary, EntryFlags};
use std::path::PathBuf;

const MOZC_ENTRIES: &str = "かんじ\t1\t2\t1200\t漢字\n\
かんじ\t2\t1\t7200\t感じ\n";

const MOZC_MATRIX: &str = "3\n\
0\n6000\n6000\n\
6000\n100\n42\n\
6000\n75\n25\n";

#[test]
fn pinned_mozc_rows_preserve_ids_costs_and_prediction_metadata() {
    let entries = parse_mozc_entries("dictionary00.txt", MOZC_ENTRIES).expect("Mozc entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].reading, "かんじ");
    assert_eq!(entries[0].surface, "漢字");
    assert_eq!(entries[0].left_id, 1);
    assert_eq!(entries[0].right_id, 2);
    assert_eq!(entries[0].word_cost, 1200);
    assert_eq!(entries[0].prediction_cost, 2400);
    assert!(entries[0].flags.contains(EntryFlags::PREDICTION));
    assert_eq!(entries[1].prediction_cost, i32::MAX);
    assert!(!entries[1].flags.contains(EntryFlags::PREDICTION));
}

#[test]
fn mozc_symbol_readings_accept_spaces_and_combining_marks() {
    let entries = parse_mozc_entries(
        "dictionary04.txt",
        " \u{301}\t2644\t2644\t14094\t \u{301}\n",
    )
    .expect("official Mozc symbol row");
    assert_eq!(entries[0].reading, " \u{301}");
    assert_eq!(entries[0].surface, " \u{301}");
}

#[test]
fn mozc_spelling_correction_label_is_preserved_without_prediction() {
    let entries = parse_mozc_entries(
        "dictionary09.txt",
        "しゅみれーしょん\t1851\t1851\t6175\tシミュレーション\tSPELLING_CORRECTION\n",
    )
    .expect("official Mozc special label");
    assert!(entries[0].flags.contains(EntryFlags::SPELLING_CORRECTION));
    assert!(!entries[0].flags.contains(EntryFlags::PREDICTION));
    assert_eq!(entries[0].prediction_cost, i32::MAX);
}

/// Issue #454 review guard: `sakura_core::conversion` encodes the 名詞-接尾
/// band (`IT_SUFFIX_CLASS_RANGE`), the plain-noun stem set
/// (`PLAIN_NOUN_STEM_RANGES`) and the そう-inflecting stem set
/// (`SOU_STEM_CLAIM_RANGES`) of the pinned Mozc `id.def`, but the engine
/// must not read `id.def` itself. This test cross-checks all three
/// constants against the same pinned checkout `build-dictionary.ps1`
/// feeds to dictc (the managed clone under
/// `~/tmp/sakura-input-dictionary-sources/mozc`, revision
/// `3f235b4eb6fcff7d14ef5f0fb8ee56de7ee4c732`). The checkout is a local
/// build cache, not a CI fixture: where it is absent the test skips and
/// the `conversion.rs` fixture still exercises the sets semantically.
#[test]
fn pinned_id_def_suffix_classes_match_the_engine_range() {
    let id_def = std::env::var_os("SAKURA_MOZC_SOURCE").map_or_else(default_mozc_id_def, |root| {
        PathBuf::from(root).join(r"src\data\dictionary_oss\id.def")
    });
    let Ok(text) = std::fs::read_to_string(&id_def) else {
        eprintln!("skipping: pinned Mozc id.def is not present at {id_def:?}");
        return;
    };
    let features =
        dictc_core::segmenter::parse_mozc_pos_features("id.def", &text).expect("pinned id.def");
    /// 名詞 classes the engine excludes from plain stems: inflecting stems
    /// (ナイ形容詞語幹, 動詞非自立的, 形容動詞語幹), the 接尾 band itself,
    /// and the bound 特殊/非自立 nouns. 接尾可能 and 接続詞的 are spelled
    /// differently in field 2 so the exact-comma match does not catch them.
    const EXCLUDED_NOUN_STEMS: &[&str] = &[
        "名詞,ナイ形容詞語幹",
        "名詞,動詞非自立的",
        "名詞,形容動詞語幹",
        "名詞,接尾,",
        "名詞,特殊",
        "名詞,非自立",
    ];
    /// 名詞 classes whose words claim a following そう as inflection.
    const SOU_STEM_CLASSES: &[&str] = &[
        "名詞,ナイ形容詞語幹",
        "名詞,動詞非自立的",
        "名詞,形容動詞語幹",
    ];
    let mut suffix_ids = 0u32;
    let mut plain_ids = 0u32;
    let mut claim_ids = 0u32;
    for (id, feature) in features.iter().enumerate() {
        let id = id as u16;
        let is_noun_suffix = feature.starts_with("名詞,接尾,");
        assert_eq!(
            is_noun_suffix,
            IT_SUFFIX_CLASS_RANGE.contains(&id),
            "suffix class {id}: {feature}"
        );
        suffix_ids += u32::from(is_noun_suffix);

        let is_plain_noun = feature.starts_with("名詞,")
            && !EXCLUDED_NOUN_STEMS
                .iter()
                .any(|prefix| feature.starts_with(prefix));
        assert_eq!(
            is_plain_noun,
            PLAIN_NOUN_STEM_RANGES
                .iter()
                .any(|range| range.contains(&id)),
            "plain noun class {id}: {feature}"
        );
        plain_ids += u32::from(is_plain_noun);

        let is_stem_claim = SOU_STEM_CLASSES
            .iter()
            .any(|prefix| feature.starts_with(prefix));
        assert_eq!(
            is_stem_claim,
            SOU_STEM_CLAIM_RANGES
                .iter()
                .any(|range| range.contains(&id)),
            "stem-claim class {id}: {feature}"
        );
        claim_ids += u32::from(is_stem_claim);
    }
    assert!(suffix_ids > 0, "the pinned id.def has no 名詞-接尾 classes");
    assert!(plain_ids > 0, "the pinned id.def has no plain noun classes");
    assert!(claim_ids > 0, "the pinned id.def has no そう-stem classes");
}

fn default_mozc_id_def() -> PathBuf {
    PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE"))
        .join(r"tmp\sakura-input-dictionary-sources\mozc\src\data\dictionary_oss\id.def")
}

#[test]
fn mozc_single_column_matrix_round_trips_every_cell() {
    let entries = parse_mozc_entries("dictionary00.txt", MOZC_ENTRIES).expect("Mozc entries");
    let matrix = parse_mozc_connection("connection.txt", MOZC_MATRIX, false).expect("Mozc matrix");
    let image = compile(&entries, &matrix).expect("image");
    let dictionary = Dictionary::parse(&image).expect("dictionary");

    for right in 0..matrix.class_count() {
        for left in 0..matrix.class_count() {
            assert_eq!(
                dictionary.connection_cost(right, left),
                matrix.cost(right, left),
                "cell ({right}, {left})"
            );
        }
    }
}

#[test]
fn mozc_matrix_rejects_wrong_cell_counts_and_shipping_taxonomy() {
    let truncated = MOZC_MATRIX.lines().take(9).collect::<Vec<_>>().join("\n");
    assert!(parse_mozc_connection("truncated.txt", &truncated, false)
        .expect_err("truncated matrix")
        .to_string()
        .contains("truncated"));

    let extra = format!("{MOZC_MATRIX}123\n");
    assert!(parse_mozc_connection("extra.txt", &extra, false)
        .expect_err("extra matrix cell")
        .to_string()
        .contains("more than"));

    assert!(parse_mozc_connection("small.txt", MOZC_MATRIX, true)
        .expect_err("shipping taxonomy mismatch")
        .to_string()
        .contains("2672"));
}
