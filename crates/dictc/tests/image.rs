use dictc::{compile, merge_entries, parse_connection, parse_entries};
use sakura_core::dictionary::{image_format as format, Dictionary, EntryFlags};

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

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().expect("u16 bytes"))
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("u32 bytes"))
}

fn directory(image: &[u8]) -> Vec<([u8; 4], usize, usize, usize)> {
    let count = usize::from(read_u16(image, 12));
    (0..count)
        .map(|index| {
            let at = format::HEADER_LEN + index * format::DIRECTORY_ENTRY_LEN;
            (
                image[at..at + 4].try_into().expect("table tag"),
                usize::try_from(read_u32(image, at + 4)).expect("table offset"),
                usize::try_from(read_u32(image, at + 8)).expect("table length"),
                usize::try_from(read_u32(image, at + 12)).expect("table count"),
            )
        })
        .collect()
}

fn table(image: &[u8], tag: [u8; 4]) -> (&[u8], usize) {
    let (_, offset, len, count) = directory(image)
        .into_iter()
        .find(|(candidate, _, _, _)| *candidate == tag)
        .unwrap_or_else(|| panic!("missing table {:?}", String::from_utf8_lossy(&tag)));
    (&image[offset..offset + len], count)
}

fn compile_text(source: &str, text: &str) -> Result<Vec<u8>, dictc::Error> {
    let entries = parse_entries(source, text)?;
    let connection = parse_connection("connection.tsv", CONNECTION, false).expect("matrix");
    compile(&entries, &connection)
}

fn single_entry_text(word_cost: i32, prediction_cost: &str) -> String {
    format!(
        "# license: BSD-3-Clause\n{header}\nあ\t値\t1\t2\t{word_cost}\t{prediction_cost}\t\tnote\n",
        header =
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation"
    )
}

#[test]
fn production_writer_emits_v2_packed_node_and_entry_records() {
    let bytes = compile_text("golden.tsv", &single_entry_text(65_535, "65534"))
        .expect("compile v2 golden image");
    assert_eq!(read_u16(&bytes, 8), format::VERSION_V2);

    let tags = directory(&bytes)
        .into_iter()
        .map(|(tag, _, _, _)| tag)
        .collect::<Vec<_>>();
    assert!(!tags.contains(&format::TAG_LABELS));
    assert!(tags.contains(&format::TAG_ANNOTATION_INDEX));

    let (nodes, node_count) = table(&bytes, format::TAG_NODES);
    assert_eq!(node_count, 2);
    assert_eq!(nodes.len(), node_count * format::NODE_LEN_V2);
    let mut expected_nodes = Vec::new();
    expected_nodes.extend_from_slice(&1u32.to_le_bytes());
    expected_nodes.extend_from_slice(&1u16.to_le_bytes());
    expected_nodes.extend_from_slice(&0u16.to_le_bytes());
    expected_nodes.extend_from_slice(&0u32.to_le_bytes());
    expected_nodes.extend_from_slice(&0u32.to_le_bytes());
    expected_nodes.extend_from_slice(&0u32.to_le_bytes());
    expected_nodes.extend_from_slice(&0u16.to_le_bytes());
    expected_nodes.extend_from_slice(&1u16.to_le_bytes());
    expected_nodes.extend_from_slice(&0u32.to_le_bytes());
    expected_nodes.extend_from_slice(&u32::from('あ').to_le_bytes());
    assert_eq!(nodes, expected_nodes);

    let (entries, entry_count) = table(&bytes, format::TAG_ENTRIES);
    assert_eq!(entry_count, 1);
    assert_eq!(entries.len(), format::ENTRY_LEN_V2);
    let mut expected_entry = Vec::new();
    expected_entry.extend_from_slice(&0u32.to_le_bytes());
    expected_entry.extend_from_slice(&1u16.to_le_bytes());
    expected_entry.extend_from_slice(&2u16.to_le_bytes());
    expected_entry.extend_from_slice(&u16::MAX.to_le_bytes());
    expected_entry.extend_from_slice(&65_534u16.to_le_bytes());
    expected_entry.extend_from_slice(&0u16.to_le_bytes());
    expected_entry.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(entries, expected_entry);

    let (annotation_index, annotation_count) = table(&bytes, format::TAG_ANNOTATION_INDEX);
    assert_eq!(annotation_count, 1);
    assert_eq!(annotation_index, [0u8; format::ANNOTATION_INDEX_LEN_V2]);
}

#[test]
fn v2_cost_boundaries_are_lossless_or_rejected_with_source_position() {
    let valid = concat!(
        "# license: BSD-3-Clause\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "あ\t零\t1\t1\t0\t0\t\t\n",
        "い\t最大\t1\t1\t65535\t65534\t\t\n",
        "う\tなし\t1\t1\t1\t-\t\t\n",
    );
    let bytes = compile_text("cost.tsv", valid).expect("compile boundary costs");
    let dictionary = Dictionary::parse(&bytes).expect("parse boundary costs");
    let mut costs = Vec::new();
    for reading in ["あ", "い", "う"] {
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes == reading.len() {
                    costs.push((matched.entry.word_cost, matched.entry.prediction_cost));
                }
                true
            })
            .expect("lookup boundary cost");
    }
    assert_eq!(costs, [(0, 0), (65_535, 65_534), (1, i32::MAX)]);

    for (text, field) in [
        (single_entry_text(-1, "0"), "word_cost"),
        (single_entry_text(0, "-1"), "prediction_cost"),
    ] {
        let error = compile_text("cost.tsv", &text).expect_err("negative cost must fail");
        assert!(error.to_string().contains("cost.tsv:3:"));
        assert!(error.to_string().contains(field));
    }
    for (text, expected) in [
        (single_entry_text(65_536, "0"), "0..=65535"),
        (single_entry_text(0, "65535"), "unavailable sentinel"),
        (single_entry_text(0, "2147483646"), "0..=65534"),
    ] {
        let error = compile_text("cost.tsv", &text).expect_err("unrepresentable cost must fail");
        assert!(error.to_string().contains("cost.tsv:3:"));
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn v2_annotation_index_uses_sparse_final_bfs_ordinals() {
    let text = concat!(
        "# license: BSD-3-Clause\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "ああ\t共有\t1\t1\t100\t-\t\tsame\n",
        "い\t共有\t1\t2\t100\t-\t\tshort\n",
        "い\t共有\t2\t1\t200\t-\t\tsame\n",
        "う\t注記なし\t1\t1\t100\t-\t\t\n",
    );
    let bytes = compile_text("annotations.tsv", text).expect("compile annotations");
    let (index, count) = table(&bytes, format::TAG_ANNOTATION_INDEX);
    assert_eq!(count, 3);
    assert_eq!(index.len(), count * format::ANNOTATION_INDEX_LEN_V2);
    let records = index
        .chunks_exact(format::ANNOTATION_INDEX_LEN_V2)
        .map(|record| (read_u32(record, 0), read_u32(record, 4)))
        .collect::<Vec<_>>();
    assert_eq!(records, [(0, 1), (1, 0), (3, 0)]);

    let (annotation_offsets, annotation_count) = table(&bytes, format::TAG_ANNOTATION_OFFSETS);
    let (_, annotation_data_count) = table(&bytes, format::TAG_ANNOTATIONS);
    assert_eq!(annotation_count, 2);
    assert_eq!(annotation_data_count, annotation_count);
    assert_eq!(annotation_offsets.len(), annotation_count * 4);

    let dictionary = Dictionary::parse(&bytes).expect("parse annotations");
    let mut found = Vec::new();
    for reading in ["い", "う", "ああ"] {
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes == reading.len() {
                    let mut surface = String::new();
                    dictionary
                        .write_surface(matched.entry, &mut surface)
                        .expect("surface");
                    let mut annotation = String::new();
                    dictionary
                        .write_annotation(matched.entry, &mut annotation)
                        .expect("annotation");
                    found.push((
                        reading.to_owned(),
                        matched.entry.left_id,
                        matched.entry.right_id,
                        surface,
                        annotation,
                    ));
                }
                true
            })
            .expect("lookup annotation");
    }
    assert_eq!(
        found,
        [
            ("い".to_owned(), 1, 2, "共有".to_owned(), "short".to_owned()),
            ("い".to_owned(), 2, 1, "共有".to_owned(), "same".to_owned()),
            ("う".to_owned(), 1, 1, "注記なし".to_owned(), String::new()),
            (
                "ああ".to_owned(),
                1,
                1,
                "共有".to_owned(),
                "same".to_owned()
            ),
        ]
    );

    let empty = compile_text(
        "empty.tsv",
        "# license: BSD-3-Clause\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
    )
    .expect("compile empty dictionary");
    let (empty_index, empty_count) = table(&empty, format::TAG_ANNOTATION_INDEX);
    assert!(empty_index.is_empty());
    assert_eq!(empty_count, 0);
}

#[test]
fn v2_surface_restarts_cover_boundaries_and_multibyte_prefixes() {
    for surface_count in [0usize, 1, 15, 16, 17, 32] {
        let mut text = concat!(
            "# license: BSD-3-Clause\n",
            "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        )
        .to_owned();
        for index in 0..surface_count {
            text.push_str(&format!(
                "{}\t共通接頭辞{index:02}🍣\t1\t1\t100\t-\t\t\n",
                "あ".repeat(index + 1)
            ));
        }
        let bytes = compile_text("surfaces.tsv", &text).expect("compile surface boundary");
        let (offsets, offset_count) = table(&bytes, format::TAG_SURFACE_OFFSETS);
        let (surface_data, directory_surface_count) = table(&bytes, format::TAG_SURFACES);
        assert_eq!(directory_surface_count, surface_count);
        assert_eq!(
            offset_count,
            surface_count.div_ceil(format::SURFACE_RESTART_INTERVAL)
        );
        assert_eq!(offsets.len(), offset_count * 4);

        let mut cursor = 0usize;
        for index in 0..surface_count {
            if index % format::SURFACE_RESTART_INTERVAL == 0 {
                assert_eq!(
                    usize::try_from(read_u32(
                        offsets,
                        index / format::SURFACE_RESTART_INTERVAL * 4
                    ))
                    .expect("restart offset"),
                    cursor
                );
                assert_eq!(read_u16(surface_data, cursor), 0);
            }
            let suffix_len = usize::from(read_u16(surface_data, cursor + 2));
            cursor += 4 + suffix_len;
        }
        assert_eq!(cursor, surface_data.len());

        let dictionary = Dictionary::parse(&bytes).expect("parse surface boundary");
        for index in 0..surface_count {
            let reading = "あ".repeat(index + 1);
            let expected = format!("共通接頭辞{index:02}🍣");
            let mut actual = None;
            dictionary
                .common_prefix_search(&reading, |matched| {
                    if matched.matched_bytes == reading.len() {
                        let mut surface = String::new();
                        dictionary
                            .write_surface(matched.entry, &mut surface)
                            .expect("surface round trip");
                        actual = Some(surface);
                    }
                    true
                })
                .expect("surface lookup");
            assert_eq!(actual.as_deref(), Some(expected.as_str()));
        }
    }
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

    let atok36 = ENTRIES.replace("BSD-3-Clause", "LicenseRef-ATOK36-LGPL");
    assert!(
        parse_entries("atok36.tsv", &atok36).is_ok(),
        "the local ATOK 36 provenance reference must remain buildable"
    );

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
fn system_and_overlay_keep_precedence_over_a_supplement() {
    let supplement_text = concat!(
        "# license: LicenseRef-ATOK36-LGPL\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "かんじ\t漢字\t1\t2\t7800\t-\t\timported supplement\n",
    );
    let system_text = concat!(
        "# license: BSD-3-Clause\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "かんじ\t漢字\t1\t2\t1200\t900\tpredict\tcore system\n",
    );
    let overlay_text = concat!(
        "# license: LicenseRef-Sakura-InHouse\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "かんじ\t漢字\t1\t2\t600\t300\tit,predict\tcurated override\n",
    );
    let supplement = parse_entries("supplement.tsv", supplement_text).expect("supplement");
    let system = parse_entries("system.tsv", system_text).expect("system");
    let overlay = parse_entries("overlay.tsv", overlay_text).expect("overlay");

    let after_system = merge_entries(supplement, system).expect("system wins");
    assert_eq!(after_system.len(), 1);
    assert_eq!(after_system[0].word_cost, 1200);
    assert_eq!(after_system[0].annotation, "core system");

    let final_entries = merge_entries(after_system, overlay).expect("overlay wins");
    assert_eq!(final_entries.len(), 1);
    assert_eq!(final_entries[0].word_cost, 600);
    assert_eq!(final_entries[0].annotation, "curated override");
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
