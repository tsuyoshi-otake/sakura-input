use sakura_store::learning::{
    crc32, encode_record, header, read_header, record_at, scan_records, HEADER_LEN,
    LEARNING_FORMAT_VERSION, REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE,
};

#[test]
fn pre_extraction_writer_fixture_preserves_all_fields_and_exact_bytes() {
    let hex = include_str!("fixtures/learning-v3-pre-extraction.hex").trim();
    assert_eq!(hex.len() % 2, 0);
    let bytes: Vec<_> = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect();
    assert_eq!(read_header(&bytes).unwrap(), LEARNING_FORMAT_VERSION);
    assert_eq!(
        scan_records(&bytes, LEARNING_FORMAT_VERSION).unwrap(),
        (bytes.len(), 3)
    );
    let expected = [
        ("synthetic-reading", "Synthetic\tSurface", 3, 4),
        (
            "synthetic-suppression",
            REPAIR_SUPPRESS_SURFACE,
            REPAIR_SUPPRESS_CONTEXT,
            REPAIR_SUPPRESS_CONTEXT,
        ),
        ("second-reading", "Second\nSurface", 5, 6),
    ];
    let mut offset = HEADER_LEN;
    let mut encoded = header(LEARNING_FORMAT_VERSION).to_vec();
    for (reading, surface, left, right) in expected {
        let (next, record) = record_at(&bytes, LEARNING_FORMAT_VERSION, offset).unwrap();
        assert_eq!(
            (
                record.reading,
                record.surface,
                record.left_context,
                record.right_context,
                record.day
            ),
            (reading, surface, left, right, 20708)
        );
        let payload = encode_record(
            record.reading,
            record.surface,
            record.left_context,
            record.right_context,
            record.day,
        )
        .unwrap();
        encoded.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&crc32(&payload).to_le_bytes());
        encoded.extend_from_slice(&payload);
        offset = next;
    }
    assert_eq!(offset, bytes.len());
    assert_eq!(encoded, bytes);
}
