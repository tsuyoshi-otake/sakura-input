//! Settings-owned viewing policy and TSV presentation for input history.

use sakura_store::input_history::persistence::retention_cutoff;
use sakura_store::input_history::{InputHistoryRecord, InputHistorySnapshot};
use sakura_values::{AiTextOperation, AiTextStatus};

pub trait InputHistorySnapshotExt {
    fn retain_current_records(&mut self, now_ms: u64);
    fn last_engine_identity(&self) -> Option<(&str, &str)>;
    fn to_tsv(&self) -> String;
}

impl InputHistorySnapshotExt for InputHistorySnapshot {
    fn retain_current_records(&mut self, now_ms: u64) {
        let cutoff = retention_cutoff(now_ms);
        self.records
            .retain(|record| record.timestamp_ms() >= cutoff);
    }

    fn last_engine_identity(&self) -> Option<(&str, &str)> {
        self.records.iter().rev().find_map(|record| match record {
            InputHistoryRecord::Engine(record) => Some((
                record.package_version.as_str(),
                record.release_label.as_str(),
            )),
            _ => None,
        })
    }

    fn to_tsv(&self) -> String {
        let (package_version, release_label) = self.last_engine_identity().unwrap_or(("-", "-"));
        let mut output = format!(
            "# sakura-input-history-format: {}\n\
# package-version: {package_version}\n\
# release-label: {release_label}\n\
# records are DPAPI-protected on disk\n\
kind\tsequence\ttimestamp-ms\tsession\tscope\tkey-code\tcharacter\tmodifiers\t\
repeat\tconsumed\tstate-before\tstate-after\tmode-before\tmode-after\tpreedit-before\t\
preedit-after\tcommit\tdelete-before\tbeep\taction\tdropped-before\treading\tsurface\t\
left-context\tright-context\tai-operation\tai-status\tai-source\tai-result\tai-model\t\
ai-provider\tai-style\tai-error-code\tai-latency-ms\tai-input-tokens\tai-output-tokens\t\
ai-cached-tokens\tai-http-attempts\tengine-package-version\tengine-release-label\n",
            self.format_version
        );
        for record in &self.records {
            match record {
                InputHistoryRecord::Key(record) => {
                    let mut fields = vec![
                        "key".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                        record.key_code.to_string(),
                        record
                            .character
                            .map_or_else(String::new, |c| escape(&c.to_string())),
                        record.modifiers.to_string(),
                        record.repeat.to_string(),
                        record.consumed.to_string(),
                        record.state_before.to_string(),
                        record.state_after.to_string(),
                        record.mode_before.to_string(),
                        record.mode_after.to_string(),
                        escape(&record.preedit_before),
                        escape(&record.preedit_after),
                        escape(&record.commit),
                        record.delete_before.to_string(),
                        record.beep.to_string(),
                        escape(&record.action),
                        record.dropped_before.to_string(),
                    ];
                    fields.extend((0..17).map(|_| String::new()));
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::Commit(record) => {
                    let mut fields = vec![
                        "commit".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..16).map(|_| String::new()));
                    fields.extend([
                        escape(&record.reading),
                        escape(&record.surface),
                        record.left_context.to_string(),
                        record.right_context.to_string(),
                    ]);
                    fields.extend((0..13).map(|_| String::new()));
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::AiText(record) => {
                    let mut fields = vec![
                        "ai-text".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..20).map(|_| String::new()));
                    fields.extend([
                        ai_operation_name(record.operation).to_owned(),
                        ai_status_name(record.status).to_owned(),
                        escape(&record.source),
                        escape(&record.result),
                        escape(&record.model),
                        escape(&record.provider),
                        escape(&record.style),
                        escape(&record.error_code),
                        record.latency_ms.to_string(),
                        record.input_tokens.to_string(),
                        record.output_tokens.to_string(),
                        record.cached_tokens.to_string(),
                        record.attempts.to_string(),
                    ]);
                    fields.push(String::new());
                    fields.push(String::new());
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
                InputHistoryRecord::Engine(record) => {
                    let mut fields = vec![
                        "engine".to_owned(),
                        record.sequence.to_string(),
                        record.timestamp_ms.to_string(),
                        record.session.to_string(),
                        record.scope.name().to_owned(),
                    ];
                    fields.extend((0..33).map(|_| String::new()));
                    fields.push(escape(&record.package_version));
                    fields.push(escape(&record.release_label));
                    output.push_str(&fields.join("\t"));
                    output.push('\n');
                }
            }
        }
        output
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

const fn ai_operation_name(operation: AiTextOperation) -> &'static str {
    match operation {
        AiTextOperation::Transform => "transform",
        AiTextOperation::Proofread => "proofread",
    }
}

const fn ai_status_name(status: AiTextStatus) -> &'static str {
    match status {
        AiTextStatus::Applied => "applied",
        AiTextStatus::Cancelled => "cancelled",
        AiTextStatus::Timeout => "timeout",
        AiTextStatus::MissingKey => "missing-key",
        AiTextStatus::WorkerError => "worker-error",
        AiTextStatus::ApiError => "api-error",
        AiTextStatus::Rejected => "rejected",
    }
}
