use dictc::{compile, parse_mozc_connection, parse_mozc_entries};
use sakura_core::dictionary::{Dictionary, EntryFlags};

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
