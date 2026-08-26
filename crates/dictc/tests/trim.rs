use dictc::{parse_mozc_entries, MozcTrimmer, TrimPolicy};

#[test]
fn trimmer_filters_deduplicates_and_caps_each_reading_deterministically() {
    let first = parse_mozc_entries(
        "dictionary00.txt",
        "あい\t1\t1\t1000\t愛\nあい\t1\t1\t3000\t藍\nあい\t1\t1\t9500\t会い\n",
    )
    .expect("first shard");
    let second = parse_mozc_entries(
        "dictionary01.txt",
        "あい\t1\t1\t900\t愛\nあい\t1\t1\t2000\t哀\nうえ\t2\t2\t500\t上\n",
    )
    .expect("second shard");
    let mut trimmer = MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        legacy_row_evidence_cap: 2,
        max_surfaces_per_reading: Some(2),
    })
    .expect("policy");
    trimmer.push_shard(first);
    trimmer.push_shard(second);
    let (entries, report) = trimmer.finish();

    assert_eq!(report.input_entries, 6);
    assert_eq!(report.cost_eligible, 5);
    assert_eq!(report.duplicate_entries, 1);
    assert_eq!(report.legacy_row_capped_entries, 1);
    assert_eq!(report.capped_entries, 1);
    assert_eq!(report.capped_surfaces, 1);
    assert_eq!(report.surface_cap_rescued_entries, 0);
    assert_eq!(report.surface_cap_rescued_surfaces, 0);
    assert_eq!(report.output_entries, 3);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry.reading.as_str(),
                entry.surface.as_str(),
                entry.word_cost
            ))
            .collect::<Vec<_>>(),
        [
            ("あい", "愛", 900),
            ("あい", "哀", 2000),
            ("うえ", "上", 500)
        ]
    );
}

#[test]
fn candidate_cap_counts_distinct_surfaces_without_discarding_selected_pos_rows() {
    let entries = parse_mozc_entries(
        "dictionary00.txt",
        concat!(
            "たて\t1\t1\t100\tたて\n",
            "たて\t2\t2\t200\tたて\n",
            "たて\t3\t3\t300\t建て\n",
            "たて\t4\t4\t400\t縦\n",
            "たて\t5\t5\t500\t建て\n",
        ),
    )
    .expect("source");
    let mut trimmer = MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        legacy_row_evidence_cap: 2,
        max_surfaces_per_reading: Some(2),
    })
    .expect("policy");
    trimmer.push_shard(entries);
    let (entries, legacy_evidence, report) = trimmer.finish_with_legacy_evidence();

    assert_eq!(report.legacy_row_capped_entries, 3);
    assert_eq!(report.capped_entries, 1);
    assert_eq!(report.capped_surfaces, 1);
    assert_eq!(report.surface_cap_rescued_entries, 2);
    assert_eq!(report.surface_cap_rescued_surfaces, 1);
    assert_eq!(report.output_entries, 4);
    assert_eq!(legacy_evidence, [true, true, false, false]);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.surface.as_str(), entry.left_id, entry.word_cost))
            .collect::<Vec<_>>(),
        [
            ("たて", 1, 100),
            ("たて", 2, 200),
            ("建て", 3, 300),
            ("建て", 5, 500),
        ]
    );
}

/// The shipped policy drops the per-reading surface cap, so `max_word_cost`
/// alone decides admission and a reading keeps every affordable homophone.
/// The legacy evidence boundary must stay where the former row cap was, or
/// growing coverage would retroactively reclassify already-shipped rows.
#[test]
fn uncapped_policy_keeps_affordable_surfaces_and_freezes_legacy_evidence() {
    let source = concat!(
        "きかん\t1\t1\t3000\t期間\n",
        "きかん\t1\t1\t4000\t機関\n",
        "きかん\t1\t1\t5655\t旗艦\n",
        "きかん\t1\t1\t5662\t気管\n",
        "きかん\t1\t1\t9500\t汽缶\n",
    );
    let mut trimmer = MozcTrimmer::new(TrimPolicy {
        max_word_cost: 6_900,
        legacy_row_evidence_cap: 2,
        max_surfaces_per_reading: None,
    })
    .expect("policy");
    trimmer.push_shard(parse_mozc_entries("dictionary00.txt", source).expect("source"));
    let (entries, legacy_evidence, report) = trimmer.finish_with_legacy_evidence();

    assert_eq!(report.cost_eligible, 4);
    assert_eq!(report.capped_entries, 0);
    assert_eq!(report.capped_surfaces, 0);
    assert_eq!(report.legacy_row_capped_entries, 2);
    assert_eq!(report.surface_cap_rescued_entries, 2);
    assert_eq!(report.surface_cap_rescued_surfaces, 2);
    assert_eq!(report.output_entries, 4);
    assert_eq!(legacy_evidence, [true, true, false, false]);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.surface.as_str())
            .collect::<Vec<_>>(),
        ["期間", "機関", "旗艦", "気管"]
    );

    let mut capped = MozcTrimmer::new(TrimPolicy {
        max_word_cost: 6_900,
        legacy_row_evidence_cap: 2,
        max_surfaces_per_reading: Some(2),
    })
    .expect("policy");
    capped.push_shard(parse_mozc_entries("dictionary00.txt", source).expect("source"));
    let (capped_entries, capped_report) = capped.finish();

    assert_eq!(capped_report.capped_surfaces, 2);
    assert_eq!(
        capped_entries
            .iter()
            .map(|entry| entry.surface.as_str())
            .collect::<Vec<_>>(),
        ["期間", "機関"]
    );
}

#[test]
fn trimmer_rejects_unbounded_policies() {
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: -1,
        legacy_row_evidence_cap: 1,
        max_surfaces_per_reading: Some(1),
    })
    .is_err());
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        legacy_row_evidence_cap: 0,
        max_surfaces_per_reading: Some(1),
    })
    .is_err());
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        legacy_row_evidence_cap: 1,
        max_surfaces_per_reading: Some(0),
    })
    .is_err());
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        legacy_row_evidence_cap: 12,
        max_surfaces_per_reading: None,
    })
    .is_ok());
}
