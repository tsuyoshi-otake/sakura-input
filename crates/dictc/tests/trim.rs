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
        max_candidates_per_reading: 2,
    })
    .expect("policy");
    trimmer.push_shard(first);
    trimmer.push_shard(second);
    let (entries, report) = trimmer.finish();

    assert_eq!(report.input_entries, 6);
    assert_eq!(report.cost_eligible, 5);
    assert_eq!(report.duplicate_entries, 1);
    assert_eq!(report.capped_entries, 1);
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
fn trimmer_rejects_unbounded_policies() {
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: -1,
        max_candidates_per_reading: 1,
    })
    .is_err());
    assert!(MozcTrimmer::new(TrimPolicy {
        max_word_cost: 9_000,
        max_candidates_per_reading: 0,
    })
    .is_err());
}
