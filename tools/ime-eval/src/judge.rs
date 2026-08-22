use crate::backend::{systems_for_assignment, JudgeBackend};
use crate::oracle;
use crate::prompt::{self, PreparedPrompt};
use crate::types::{
    CaseRecord, Error, OracleVerdict, SemanticCase, SemanticOutcome, Side, SystemOutput, Verdict,
};

pub fn judge_pair(
    case: &SemanticCase,
    baseline: &SystemOutput,
    candidate: &SystemOutput,
    seed: u64,
    backend: &mut impl JudgeBackend,
) -> Result<CaseRecord, Error> {
    let oracle_baseline = oracle::evaluate(case, baseline);
    let oracle_candidate = oracle::evaluate(case, candidate);
    let mut record = CaseRecord {
        case_id: case.case_id.clone(),
        opaque_id: crate::blind::opaque_id(seed, &case.case_id),
        oracle_candidate: oracle_name(oracle_candidate),
        oracle_baseline: oracle_name(oracle_baseline),
        semantic: None,
        severity: 0,
        unstable: false,
        hard_failure: oracle_candidate.is_hard_failure(),
    };
    if record.hard_failure {
        return Ok(record);
    }

    let first = prompt::prepare(case, baseline, candidate, seed, false)?;
    let first_result = evaluate_prepared(case, baseline, candidate, &first, backend)?;
    if !first_result.verdict.is_decisive() {
        record.semantic = Some(if first_result.verdict == Verdict::Tie {
            SemanticOutcome::Tie
        } else {
            SemanticOutcome::Ungradable
        });
        record.severity = first_result.severity;
        return Ok(record);
    }

    let second = prompt::prepare(case, baseline, candidate, seed, true)?;
    let second_result = evaluate_prepared(case, baseline, candidate, &second, backend)?;
    if first_result.verdict.is_decisive() && first_result.verdict == second_result.verdict {
        record.semantic = Some(SemanticOutcome::Unstable);
        record.unstable = true;
        record.severity = first_result.severity.max(second_result.severity);
        return Ok(record);
    }
    if crate::blind::swap_is_consistent(
        first_result.verdict,
        first.assignment,
        second_result.verdict,
        second.assignment,
    ) {
        let winner = first
            .assignment
            .side_for(first_result.verdict)
            .expect("decisive verdict maps to a side");
        record.semantic = Some(match winner {
            Side::Candidate => SemanticOutcome::CandidateBetter,
            Side::Baseline => SemanticOutcome::BaselineBetter,
        });
        record.severity = first_result.severity.max(second_result.severity);
        return Ok(record);
    }

    let third = prompt::prepare(case, baseline, candidate, seed.wrapping_add(1), false)?;
    let third_result = evaluate_prepared(case, baseline, candidate, &third, backend)?;
    let votes = [
        first.assignment.side_for(first_result.verdict),
        second.assignment.side_for(second_result.verdict),
        third.assignment.side_for(third_result.verdict),
    ];
    if let Some(winner) = majority_side(votes) {
        record.semantic = Some(match winner {
            Side::Candidate => SemanticOutcome::CandidateBetter,
            Side::Baseline => SemanticOutcome::BaselineBetter,
        });
        record.severity = first_result
            .severity
            .max(second_result.severity)
            .max(third_result.severity);
    } else {
        record.semantic = Some(SemanticOutcome::Unstable);
        record.unstable = true;
        record.severity = first_result
            .severity
            .max(second_result.severity)
            .max(third_result.severity);
    }
    Ok(record)
}

fn evaluate_prepared(
    case: &SemanticCase,
    baseline: &SystemOutput,
    candidate: &SystemOutput,
    prepared: &PreparedPrompt,
    backend: &mut impl JudgeBackend,
) -> Result<crate::types::JudgeResult, Error> {
    if prepared.user_prompt.contains("resume") {
        return Err(crate::types::err("judge prompt must not mention resume"));
    }
    let (system_a, system_b) = systems_for_assignment(prepared.assignment, baseline, candidate);
    let result = backend.evaluate(case, prepared, system_a, system_b)?;
    if result.case_id != prepared.opaque_id {
        return Err(crate::types::err(
            "judge result case_id does not match opaque id",
        ));
    }
    Ok(result)
}

fn majority_side(votes: [Option<Side>; 3]) -> Option<Side> {
    let mut baseline = 0u8;
    let mut candidate = 0u8;
    for vote in votes {
        match vote {
            Some(Side::Baseline) => baseline += 1,
            Some(Side::Candidate) => candidate += 1,
            None => {}
        }
    }
    if candidate >= 2 {
        Some(Side::Candidate)
    } else if baseline >= 2 {
        Some(Side::Baseline)
    } else {
        None
    }
}

fn oracle_name(verdict: OracleVerdict) -> String {
    match verdict {
        OracleVerdict::Pass => "pass".to_owned(),
        OracleVerdict::LiteralCorruption => "literal_corruption".to_owned(),
    }
}
