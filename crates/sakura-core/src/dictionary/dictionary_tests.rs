use super::{image_format as format, Dictionary, Entry, EntryFlags, Error};

#[derive(Clone)]
struct TestTable {
    tag: [u8; 4],
    bytes: Vec<u8>,
    count: usize,
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.char_indices())
        .take_while(|((left_at, left_char), (right_at, right_char))| {
            left_at == right_at && left_char == right_char
        })
        .map(|((at, ch), _)| at + ch.len_utf8())
        .last()
        .unwrap_or(0)
}

fn v2_surfaces(count: usize, trailing_byte: bool) -> (Vec<String>, Vec<u8>, Vec<u8>) {
    let values = (0..count)
        .map(|index| format!("表面{index:02}"))
        .collect::<Vec<_>>();
    let mut offsets = Vec::new();
    let mut data = Vec::new();
    let mut previous = "";
    for (index, value) in values.iter().enumerate() {
        if index % format::SURFACE_RESTART_INTERVAL == 0 {
            put_u32(&mut offsets, u32::try_from(data.len()).unwrap());
        }
        let prefix = if index % format::SURFACE_RESTART_INTERVAL == 0 {
            0
        } else {
            common_prefix_bytes(previous, value)
        };
        let suffix = &value[prefix..];
        put_u16(&mut data, u16::try_from(prefix).unwrap());
        put_u16(&mut data, u16::try_from(suffix.len()).unwrap());
        data.extend_from_slice(suffix.as_bytes());
        previous = value;
    }
    if trailing_byte {
        data.push(0xaa);
    }
    (values, offsets, data)
}

fn v2_image(
    surface_count: usize,
    annotation_index: &[(u32, u32)],
    include_labels: bool,
    trailing_surface_byte: bool,
) -> Vec<u8> {
    let mut louds = Vec::new();
    put_u32(&mut louds, 5);
    louds.push(0b0000_0011);

    // Root has two sorted children. All fixture entries belong to "あ";
    // "い" keeps child-order validation observable without changing values.
    let mut nodes = Vec::new();
    for (first_child, child_count, value_count, value_start, label) in [
        (1u32, 2u16, 0u16, 0u32, '\0'),
        (0, 0, u16::try_from(surface_count).unwrap(), 0, 'あ'),
        (0, 0, 0, u32::try_from(surface_count).unwrap(), 'い'),
    ] {
        put_u32(&mut nodes, first_child);
        put_u16(&mut nodes, child_count);
        put_u16(&mut nodes, value_count);
        put_u32(&mut nodes, value_start);
        put_u32(&mut nodes, label as u32);
    }

    let mut entries = Vec::new();
    for index in 0..surface_count {
        put_u32(&mut entries, u32::try_from(index).unwrap());
        put_u16(&mut entries, 0);
        put_u16(&mut entries, 0);
        put_u16(&mut entries, u16::try_from(100 + index).unwrap());
        put_u16(
            &mut entries,
            if index % 2 == 0 {
                u16::MAX
            } else {
                u16::try_from(200 + index).unwrap()
            },
        );
        put_u16(&mut entries, EntryFlags::PREDICTION.bits());
        put_u16(&mut entries, 0);
    }

    let (surfaces, surface_offsets, surface_data) =
        v2_surfaces(surface_count, trailing_surface_byte);

    let annotations = if annotation_index.is_empty() {
        Vec::new()
    } else {
        vec!["注釈0", "annotation one"]
    };
    let mut annotation_offsets = Vec::new();
    let mut annotation_data = Vec::new();
    for annotation in &annotations {
        put_u32(
            &mut annotation_offsets,
            u32::try_from(annotation_data.len()).unwrap(),
        );
        annotation_data.extend_from_slice(annotation.as_bytes());
    }
    let mut annotation_index_data = Vec::new();
    for &(entry, annotation) in annotation_index {
        put_u32(&mut annotation_index_data, entry);
        put_u32(&mut annotation_index_data, annotation);
    }

    let mut matrix = Vec::new();
    matrix.extend_from_slice(&format::MATRIX_MAGIC);
    put_u16(&mut matrix, 1);
    put_u16(&mut matrix, 0);
    put_u32(&mut matrix, 0);
    put_u32(&mut matrix, 0);
    put_u16(&mut matrix, 0);
    put_u16(&mut matrix, 0);
    put_u32(&mut matrix, 0);
    put_u32(&mut matrix, 0);

    let mut tables = vec![
        TestTable {
            tag: format::TAG_LOUDS,
            bytes: louds,
            count: 5,
        },
        TestTable {
            tag: format::TAG_NODES,
            bytes: nodes,
            count: 3,
        },
        TestTable {
            tag: format::TAG_ENTRIES,
            bytes: entries,
            count: surface_count,
        },
        TestTable {
            tag: format::TAG_SURFACE_OFFSETS,
            bytes: surface_offsets,
            count: surface_count.div_ceil(format::SURFACE_RESTART_INTERVAL),
        },
        TestTable {
            tag: format::TAG_SURFACES,
            bytes: surface_data,
            count: surfaces.len(),
        },
        TestTable {
            tag: format::TAG_ANNOTATION_OFFSETS,
            bytes: annotation_offsets,
            count: annotations.len(),
        },
        TestTable {
            tag: format::TAG_ANNOTATIONS,
            bytes: annotation_data,
            count: annotations.len(),
        },
        TestTable {
            tag: format::TAG_ANNOTATION_INDEX,
            bytes: annotation_index_data,
            count: annotation_index.len(),
        },
        TestTable {
            tag: format::TAG_MATRIX,
            bytes: matrix,
            count: 1,
        },
    ];
    if include_labels {
        let mut labels = Vec::new();
        for label in ['\0', 'あ', 'い'] {
            put_u32(&mut labels, label as u32);
        }
        tables.push(TestTable {
            tag: format::TAG_LABELS,
            bytes: labels,
            count: 3,
        });
    }
    assemble_v2(surface_count, 3, tables)
}

fn assemble_v2(entry_count: usize, node_count: usize, tables: Vec<TestTable>) -> Vec<u8> {
    let prefix = format::HEADER_LEN + tables.len() * format::DIRECTORY_ENTRY_LEN;
    let mut image = vec![0; prefix];
    let mut directory = Vec::new();
    for table in tables {
        while !image.len().is_multiple_of(8) {
            image.push(0);
        }
        let offset = image.len();
        image.extend_from_slice(&table.bytes);
        directory.push((table.tag, offset, table.bytes.len(), table.count));
    }
    image[..8].copy_from_slice(&format::MAGIC);
    write_u16(&mut image, 8, format::VERSION_V2);
    write_u16(&mut image, 10, format::HEADER_LEN as u16);
    write_u16(&mut image, 12, u16::try_from(directory.len()).unwrap());
    write_u16(&mut image, 14, 1);
    write_u32(&mut image, 16, u32::try_from(entry_count).unwrap());
    write_u32(&mut image, 20, u32::try_from(node_count).unwrap());
    let image_len = u32::try_from(image.len()).unwrap();
    write_u32(&mut image, 24, image_len);
    write_u32(&mut image, 28, 0);
    for (index, (tag, offset, len, count)) in directory.into_iter().enumerate() {
        let at = format::HEADER_LEN + index * format::DIRECTORY_ENTRY_LEN;
        image[at..at + 4].copy_from_slice(&tag);
        write_u32(&mut image, at + 4, u32::try_from(offset).unwrap());
        write_u32(&mut image, at + 8, u32::try_from(len).unwrap());
        write_u32(&mut image, at + 12, u32::try_from(count).unwrap());
    }
    image
}

fn table_location(image: &[u8], wanted: [u8; 4]) -> (usize, usize, usize) {
    let count = usize::from(u16::from_le_bytes(image[12..14].try_into().unwrap()));
    for index in 0..count {
        let directory = format::HEADER_LEN + index * format::DIRECTORY_ENTRY_LEN;
        if image[directory..directory + 4] == wanted {
            let offset =
                u32::from_le_bytes(image[directory + 4..directory + 8].try_into().unwrap());
            let len = u32::from_le_bytes(image[directory + 8..directory + 12].try_into().unwrap());
            return (
                usize::try_from(offset).unwrap(),
                usize::try_from(len).unwrap(),
                directory,
            );
        }
    }
    panic!("missing fixture table {wanted:?}");
}

fn assert_parse_error(image: &[u8], expected: Error) {
    assert_eq!(
        Dictionary::parse(image).expect_err("image must fail"),
        expected
    );
}

#[test]
fn writer_facing_aliases_remain_v1_and_entry_stays_compact() {
    assert_eq!(format::VERSION, 1);
    assert_eq!(format::NODE_LEN, 16);
    assert_eq!(format::ENTRY_LEN, 24);
    assert_eq!(format::VERSION_V2, 2);
    assert_eq!(format::NODE_LEN_V2, 16);
    assert_eq!(format::ENTRY_LEN_V2, 16);
    assert_eq!(format::ANNOTATION_INDEX_LEN_V2, 8);
    assert_eq!(core::mem::size_of::<Entry>(), 24);
    assert_eq!(Entry::default().annotation_locator, format::NO_ANNOTATION);
}

#[test]
fn v2_node_entry_surface_and_annotation_round_trip() {
    let image = v2_image(3, &[(0, 0), (2, 1)], false, false);
    let dictionary = Dictionary::parse(&image).expect("valid v2 fixture");
    let mut found = Vec::new();
    dictionary
        .common_prefix_search("あ", |matched| {
            let mut surface = String::new();
            dictionary
                .write_surface(matched.entry, &mut surface)
                .expect("surface");
            let mut annotation = String::new();
            dictionary
                .write_annotation(matched.entry, &mut annotation)
                .expect("annotation");
            found.push((surface, annotation, matched.entry));
            true
        })
        .expect("lookup");

    assert_eq!(found.len(), 3);
    assert_eq!(
        (&found[0].0, &found[0].1),
        (&"表面00".to_owned(), &"注釈0".to_owned())
    );
    assert_eq!(
        (&found[1].0, &found[1].1),
        (&"表面01".to_owned(), &String::new())
    );
    assert_eq!(
        (&found[2].0, &found[2].1),
        (&"表面02".to_owned(), &"annotation one".to_owned())
    );
    assert_eq!(found[0].2.word_cost, 100);
    assert_eq!(found[0].2.prediction_cost, i32::MAX);
    assert_eq!(found[1].2.prediction_cost, 201);
    assert!(found[2].2.flags.contains(EntryFlags::PREDICTION));
}

#[test]
fn versions_and_version_specific_tables_are_strict() {
    let mut unknown = v2_image(1, &[], false, false);
    write_u16(&mut unknown, 8, 3);
    assert_parse_error(&unknown, Error::UnsupportedVersion(3));

    let mixed = v2_image(1, &[], true, false);
    assert_parse_error(&mixed, Error::BadTable(format::TAG_LABELS));

    let mut missing_aidx = v2_image(1, &[], false, false);
    let (_, _, directory) = table_location(&missing_aidx, format::TAG_ANNOTATION_INDEX);
    missing_aidx[directory..directory + 4].copy_from_slice(b"XXXX");
    assert_parse_error(
        &missing_aidx,
        Error::MissingTable(format::TAG_ANNOTATION_INDEX),
    );

    let mut duplicate_aidx = v2_image(1, &[], false, false);
    let (_, _, matrix_directory) = table_location(&duplicate_aidx, format::TAG_MATRIX);
    duplicate_aidx[matrix_directory..matrix_directory + 4]
        .copy_from_slice(&format::TAG_ANNOTATION_INDEX);
    assert_parse_error(
        &duplicate_aidx,
        Error::DuplicateTable(format::TAG_ANNOTATION_INDEX),
    );
}

#[test]
fn every_truncated_v2_fixture_is_rejected() {
    let image = v2_image(17, &[(0, 0), (16, 1)], false, false);
    for end in 0..image.len() {
        assert!(
            Dictionary::parse(&image[..end]).is_err(),
            "truncation at {end} unexpectedly parsed"
        );
    }
}

#[test]
fn v2_nodes_reject_invalid_scalars_and_unsorted_children() {
    let mut invalid_scalar = v2_image(1, &[], false, false);
    let (nodes, _, _) = table_location(&invalid_scalar, format::TAG_NODES);
    write_u32(
        &mut invalid_scalar,
        nodes + format::NODE_LEN_V2 + 12,
        0xd800,
    );
    assert_parse_error(&invalid_scalar, Error::BadTree);

    let mut duplicate_child = v2_image(1, &[], false, false);
    let (nodes, _, _) = table_location(&duplicate_child, format::TAG_NODES);
    write_u32(
        &mut duplicate_child,
        nodes + 2 * format::NODE_LEN_V2 + 12,
        'あ' as u32,
    );
    assert_parse_error(&duplicate_child, Error::BadTree);
}

#[test]
fn v2_entries_reject_nonzero_reserved_bytes() {
    let mut image = v2_image(1, &[], false, false);
    let (entries, _, _) = table_location(&image, format::TAG_ENTRIES);
    write_u16(&mut image, entries + 14, 1);
    assert_parse_error(&image, Error::BadEntry);
}

#[test]
fn v2_annotation_index_rejects_order_duplicates_and_ranges() {
    let valid = v2_image(3, &[(0, 0), (2, 1)], false, false);
    let (index, _, _) = table_location(&valid, format::TAG_ANNOTATION_INDEX);

    let mut reversed = valid.clone();
    write_u32(&mut reversed, index, 2);
    write_u32(&mut reversed, index + 8, 0);
    assert_parse_error(&reversed, Error::BadTable(format::TAG_ANNOTATION_INDEX));

    let mut duplicate = valid.clone();
    write_u32(&mut duplicate, index + 8, 0);
    assert_parse_error(&duplicate, Error::BadTable(format::TAG_ANNOTATION_INDEX));

    let mut entry_out_of_range = valid.clone();
    write_u32(&mut entry_out_of_range, index + 8, 3);
    assert_parse_error(
        &entry_out_of_range,
        Error::BadTable(format::TAG_ANNOTATION_INDEX),
    );

    let mut annotation_out_of_range = valid;
    write_u32(&mut annotation_out_of_range, index + 4, 2);
    assert_parse_error(
        &annotation_out_of_range,
        Error::BadTable(format::TAG_ANNOTATION_INDEX),
    );
}

#[test]
fn v2_surface_restart_boundaries_round_trip() {
    for count in [0, 1, 15, 16, 17, 32] {
        let image = v2_image(count, &[], false, false);
        let dictionary = Dictionary::parse(&image).expect("boundary fixture");
        assert_eq!(dictionary.entry_count(), count);
        for index in 0..count {
            let entry = dictionary.entry_at(index).expect("entry");
            let mut surface = String::new();
            dictionary
                .write_surface(entry, &mut surface)
                .expect("surface");
            assert_eq!(surface, format!("表面{index:02}"));
        }
    }
}

#[test]
fn v2_surface_offsets_reject_reverse_and_out_of_range_values() {
    let valid = v2_image(17, &[], false, false);
    let (offsets, _, _) = table_location(&valid, format::TAG_SURFACE_OFFSETS);
    let (surfaces, surface_len, _) = table_location(&valid, format::TAG_SURFACES);
    let second = u32::from_le_bytes(valid[offsets + 4..offsets + 8].try_into().unwrap());

    let mut reversed = valid.clone();
    write_u32(&mut reversed, offsets, second);
    write_u32(&mut reversed, offsets + 4, 0);
    assert!(Dictionary::parse(&reversed).is_err());

    let mut out_of_range = valid;
    write_u32(
        &mut out_of_range,
        offsets + 4,
        u32::try_from(surface_len + 1).unwrap(),
    );
    assert_parse_error(&out_of_range, Error::BadTable(format::TAG_SURFACE_OFFSETS));
    assert!(surfaces > offsets);
}

#[test]
fn v2_surfaces_reject_mid_scalar_prefix_bad_lengths_and_restart_prefixes() {
    let valid = v2_image(17, &[], false, false);
    let (surfaces, _, _) = table_location(&valid, format::TAG_SURFACES);
    let first_suffix_len = usize::from(u16::from_le_bytes(
        valid[surfaces + 2..surfaces + 4].try_into().unwrap(),
    ));
    let second_record = surfaces + 4 + first_suffix_len;

    let mut mid_scalar = valid.clone();
    write_u16(&mut mid_scalar, second_record, 1);
    assert_parse_error(&mid_scalar, Error::BadTable(format::TAG_SURFACES));

    let mut bad_length = valid.clone();
    write_u16(&mut bad_length, second_record + 2, u16::MAX);
    assert_parse_error(&bad_length, Error::BadTable(format::TAG_SURFACES));

    let (offsets, _, _) = table_location(&valid, format::TAG_SURFACE_OFFSETS);
    let restart = usize::try_from(u32::from_le_bytes(
        valid[offsets + 4..offsets + 8].try_into().unwrap(),
    ))
    .unwrap();
    let mut nonzero_restart_prefix = valid;
    write_u16(&mut nonzero_restart_prefix, surfaces + restart, 3);
    assert_parse_error(
        &nonzero_restart_prefix,
        Error::BadTable(format::TAG_SURFACES),
    );
}

#[test]
fn v2_surfaces_reject_trailing_bytes() {
    let image = v2_image(1, &[], false, true);
    assert_parse_error(&image, Error::BadTable(format::TAG_SURFACES));
}
