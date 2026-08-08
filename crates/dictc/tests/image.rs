use dictc::{compile, merge_entries, parse_connection, parse_entries};
use sakura_core::dictionary::{Dictionary, EntryFlags};

const ENTRIES: &str = "# license: BSD-3-Clause\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
か\t蚊\t1\t1\t3100\t-\t\t昆虫\n\
かんじ\t漢字\t1\t2\t1200\t900\tpredict\t表意文字\n\
かんじ\t感じ\t1\t1\t1500\t-\t\t感覚\n\
かんすう\t関数\t2\t2\t700\t300\tit,predict\tprogramming function\n";

const CONNECTION: &str = "# license: BSD-3-Clause\n\
classes\t4\n\
default\t6000\n\
cost\t0\t0\t0\n\
cost\t1\t1\t100\n\
cost\t1\t2\t42\n\
cost\t2\t1\t75\n\
cost\t2\t2\t25\n";

fn image() -> Vec<u8> {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("valid fixture entries");
    let connection =
        parse_connection("connection.tsv", CONNECTION, false).expect("valid fixture matrix");
    compile(&entries, &connection).expect("compile fixture")
}

#[test]
fn image_is_byte_deterministic_and_borrowed_by_the_reader() {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    let first = compile(&entries, &connection).expect("first compile");
    let second = compile(&entries, &connection).expect("second compile");
    assert_eq!(first, second);

    let dictionary = Dictionary::parse(&first).expect("parse compiled image");
    assert_eq!(dictionary.class_count(), 4);
    assert_eq!(dictionary.entry_count(), 4);
    assert_eq!(dictionary.connection_cost(1, 2), Some(42));
    for right in 0..connection.class_count() {
        for left in 0..connection.class_count() {
            assert_eq!(
                dictionary.connection_cost(right, left),
                connection.cost(right, left),
                "connection cost at ({right}, {left})"
            );
        }
    }
}

#[test]
fn row_mode_matrix_is_smaller_for_a_realistically_sparse_cost_model() {
    let entries = parse_entries("fixture.tsv", ENTRIES).expect("entries");
    let connection_text = "# license: BSD-3-Clause\n\
classes\t64\n\
default\t6000\n\
cost\t0\t0\t0\n\
cost\t1\t2\t42\n\
cost\t2\t1\t75\n";
    let connection = parse_connection("sparse.tsv", connection_text, false).expect("sparse matrix");
    let bytes = compile(&entries, &connection).expect("compile sparse matrix");
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");

    assert!(
        dictionary.matrix_bytes_len()
            < usize::from(connection.class_count()).pow(2) * std::mem::size_of::<u16>(),
        "row-mode encoding should beat a flat matrix once fixed overhead is amortized"
    );
    assert_eq!(dictionary.connection_cost(1, 2), Some(42));
    assert_eq!(dictionary.connection_cost(63, 63), Some(6000));
}

#[test]
fn louds_prefix_search_returns_every_terminal_in_reading_order() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut found = Vec::new();

    dictionary
        .common_prefix_search("かんじる", |matched| {
            let mut surface = String::new();
            dictionary
                .write_surface(matched.entry, &mut surface)
                .expect("surface");
            let mut annotation = String::new();
            dictionary
                .write_annotation(matched.entry, &mut annotation)
                .expect("annotation");
            found.push((
                matched.matched_bytes,
                surface,
                annotation,
                matched.entry.flags,
            ));
            true
        })
        .expect("lookup");

    assert_eq!(found.len(), 3);
    assert_eq!(found[0].0, "か".len());
    assert_eq!(found[0].1, "蚊");
    assert_eq!(found[1].0, "かんじ".len());
    assert_eq!(found[1].1, "漢字");
    assert_eq!(found[1].2, "表意文字");
    assert!(found[1].3.contains(EntryFlags::PREDICTION));
    assert_eq!(found[2].1, "感じ");
}

#[test]
fn front_coded_surfaces_resolve_independently() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut surfaces = Vec::new();

    dictionary
        .common_prefix_search("かんすう", |matched| {
            let mut surface = String::new();
            dictionary
                .write_surface(matched.entry, &mut surface)
                .expect("surface");
            surfaces.push(surface);
            true
        })
        .expect("lookup");

    assert_eq!(surfaces, ["蚊", "関数"]);
}

#[test]
fn prediction_index_walk_yields_only_marked_entries_with_complete_readings() {
    let bytes = image();
    let dictionary = Dictionary::parse(&bytes).expect("dictionary");
    let mut found = Vec::new();

    dictionary
        .visit_prediction_entries(|reading, entry| {
            let mut surface = String::new();
            dictionary
                .write_surface(entry, &mut surface)
                .expect("surface");
            found.push((reading.to_owned(), surface, entry.prediction_cost));
            true
        })
        .expect("prediction walk");

    assert_eq!(
        found,
        [
            ("かんじ".to_owned(), "漢字".to_owned(), 900),
            ("かんすう".to_owned(), "関数".to_owned(), 300),
        ]
    );
}

#[test]
fn source_license_and_frozen_taxonomy_are_real_gates() {
    let proprietary = ENTRIES.replace("BSD-3-Clause", "LicenseRef-Unknown-Proprietary");
    let error = parse_entries("bad.tsv", &proprietary).expect_err("license must be rejected");
    assert!(error.to_string().contains("license"));

    let error = parse_connection("small.tsv", CONNECTION, true)
        .expect_err("shipping compiler freezes the taxonomy at 2672 classes");
    assert!(error.to_string().contains("2672"));
}

#[test]
fn overlay_replaces_the_same_system_edge_and_layer_duplicates_fail() {
    let system = parse_entries("system.tsv", ENTRIES).expect("system");
    let overlay_text = "# license: LicenseRef-Sakura-InHouse\n\
reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
かんじ\t漢字\t1\t2\t600\t300\tit,predict\tIT override\n";
    let overlay = parse_entries("overlay.tsv", overlay_text).expect("overlay");
    let merged = merge_entries(system.clone(), overlay.clone()).expect("merge");
    let kanji = merged
        .iter()
        .find(|entry| entry.reading == "かんじ" && entry.surface == "漢字")
        .expect("overridden edge");
    assert_eq!(kanji.word_cost, 600);
    assert_eq!(kanji.annotation, "IT override");
    assert!(kanji.flags.contains(EntryFlags::IT));
    assert_eq!(merged.len(), system.len());

    let duplicate_overlay = [overlay.clone(), overlay].concat();
    assert!(merge_entries(system, duplicate_overlay).is_err());
}

#[test]
fn every_truncated_image_is_rejected_without_panicking() {
    let bytes = image();
    for end in 0..bytes.len() {
        assert!(
            Dictionary::parse(&bytes[..end]).is_err(),
            "truncation at {end} unexpectedly parsed"
        );
    }
}

#[test]
fn corrupt_table_offsets_and_payloads_never_panic() {
    let bytes = image();
    for index in 0..bytes.len() {
        let mut corrupt = bytes.clone();
        corrupt[index] ^= 0xff;
        let result = std::panic::catch_unwind(|| {
            if let Ok(dictionary) = Dictionary::parse(&corrupt) {
                let _ = dictionary.common_prefix_search("かんじる", |matched| {
                    let mut surface = String::new();
                    let _ = dictionary.write_surface(matched.entry, &mut surface);
                    let mut annotation = String::new();
                    let _ = dictionary.write_annotation(matched.entry, &mut annotation);
                    true
                });
                let _ = dictionary.visit_prediction_entries(|_, entry| {
                    let mut surface = String::new();
                    let _ = dictionary.write_surface(entry, &mut surface);
                    true
                });
            }
        });
        assert!(result.is_ok(), "mutation at byte {index} panicked");
    }
}
