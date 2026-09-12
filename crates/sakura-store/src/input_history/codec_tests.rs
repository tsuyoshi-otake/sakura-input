use sakura_values::{AiTextOperation, AiTextStatus};

use super::*;

fn round_trip(record: InputHistoryRecord, tag: u8, scope: HistoryScope) {
    let bytes = record.encode().unwrap();
    assert_eq!(bytes[0], tag);
    // kind + sequence + timestamp + session precede the durable three-value scope.
    assert_eq!(bytes[25], scope as u8);
    assert_eq!(InputHistoryRecord::decode(&bytes).unwrap(), record);
}

#[test]
fn all_v2_record_tags_and_history_scope_values_round_trip() {
    round_trip(
        InputHistoryRecord::Key(KeyHistoryRecord {
            sequence: 1,
            timestamp_ms: 2,
            session: 3,
            scope: HistoryScope::Unclassified,
            key_code: 4,
            character: Some('桜'),
            modifiers: 5,
            repeat: false,
            consumed: true,
            state_before: 6,
            state_after: 7,
            mode_before: 8,
            mode_after: 9,
            preedit_before: "前".into(),
            preedit_after: "後".into(),
            commit: "確定".into(),
            delete_before: 10,
            beep: false,
            action: "入力".into(),
            dropped_before: 11,
        }),
        1,
        HistoryScope::Unclassified,
    );
    round_trip(
        InputHistoryRecord::Commit(CommitHistoryRecord {
            sequence: 1,
            timestamp_ms: 2,
            session: 3,
            scope: HistoryScope::Normal,
            reading: "さくら".into(),
            surface: "桜".into(),
            left_context: 4,
            right_context: 5,
        }),
        2,
        HistoryScope::Normal,
    );
    round_trip(
        InputHistoryRecord::AiText(AiTextHistoryRecord {
            sequence: 1,
            timestamp_ms: 2,
            session: 3,
            scope: HistoryScope::Sensitive,
            operation: AiTextOperation::Transform,
            status: AiTextStatus::Applied,
            source: "src".into(),
            result: "dst".into(),
            model: "m".into(),
            provider: "p".into(),
            style: "s".into(),
            error_code: String::new(),
            latency_ms: 4,
            input_tokens: 5,
            output_tokens: 6,
            cached_tokens: 7,
            attempts: 8,
        }),
        3,
        HistoryScope::Sensitive,
    );
    round_trip(
        InputHistoryRecord::Engine(EngineHistoryRecord {
            sequence: 1,
            timestamp_ms: 2,
            session: 3,
            scope: HistoryScope::Normal,
            package_version: "1.0.0".into(),
            release_label: "release".into(),
        }),
        4,
        HistoryScope::Normal,
    );
}

#[test]
fn malformed_or_oversized_payloads_fail_closed() {
    let record = InputHistoryRecord::Engine(EngineHistoryRecord {
        sequence: 1,
        timestamp_ms: 2,
        session: 3,
        scope: HistoryScope::Normal,
        package_version: "1.0.0".into(),
        release_label: "release".into(),
    });
    let mut trailing = record.encode().unwrap();
    trailing.push(0);
    assert!(InputHistoryRecord::decode(&trailing).is_err());

    let oversized = InputHistoryRecord::Commit(CommitHistoryRecord {
        sequence: 1,
        timestamp_ms: 2,
        session: 3,
        scope: HistoryScope::Normal,
        reading: "x".repeat(MAX_RECORD_BYTES),
        surface: String::new(),
        left_context: 0,
        right_context: 0,
    });
    assert!(oversized.encode().is_err());
}
