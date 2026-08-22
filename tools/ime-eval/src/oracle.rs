use crate::types::{OracleVerdict, SemanticCase, SystemOutput};

pub fn evaluate(case: &SemanticCase, output: &SystemOutput) -> OracleVerdict {
    if !case.constraints.literal_token {
        return OracleVerdict::Pass;
    }
    match output.top1() {
        Some(top) if top == case.input.reading => OracleVerdict::Pass,
        _ => OracleVerdict::LiteralCorruption,
    }
}
