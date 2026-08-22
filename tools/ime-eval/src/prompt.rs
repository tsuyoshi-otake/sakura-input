use serde::Serialize;

use crate::types::{err, Assignment, Context, Error, SemanticCase, Side, SystemOutput};

#[derive(Serialize)]
struct JudgeView<'a> {
    case_id: &'a str,
    task: &'a str,
    context: &'a Context,
    input: JudgeInput<'a>,
    system_a: &'a SystemOutput,
    system_b: &'a SystemOutput,
}

#[derive(Serialize)]
struct JudgeInput<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    input_mode: Option<&'a str>,
    reading: &'a str,
}

#[derive(Debug)]
pub struct PreparedPrompt {
    pub opaque_id: String,
    pub assignment: Assignment,
    pub user_prompt: String,
    pub case_json: String,
}

pub fn prepare(
    case: &SemanticCase,
    baseline: &SystemOutput,
    candidate: &SystemOutput,
    seed: u64,
    swap: bool,
) -> Result<PreparedPrompt, Error> {
    let opaque_id = crate::blind::opaque_id(seed, &case.case_id);
    let mut assignment = crate::blind::assignment(seed, &case.case_id);
    if swap {
        assignment = assignment.swapped();
    }
    let (system_a, system_b) = match assignment.system_a {
        Side::Baseline => (baseline, candidate),
        Side::Candidate => (candidate, baseline),
    };
    let view = JudgeView {
        case_id: &opaque_id,
        task: &case.task,
        context: &case.context,
        input: JudgeInput {
            input_mode: case.input.input_mode.as_deref(),
            reading: &case.input.reading,
        },
        system_a,
        system_b,
    };
    let case_json = serde_json::to_string_pretty(&view)
        .map_err(|error| err(format!("serialize judge view: {error}")))?;
    let user_prompt =
        format!("Evaluate this anonymous Japanese IME comparison.\n\nCASE:\n\n{case_json}\n");
    reject_leaks(&user_prompt, case)?;
    Ok(PreparedPrompt {
        opaque_id,
        assignment,
        user_prompt,
        case_json,
    })
}

fn reject_leaks(prompt: &str, case: &SemanticCase) -> Result<(), Error> {
    if prompt.contains(&case.case_id) {
        return Err(err("judge prompt leaked the corpus case_id"));
    }
    for forbidden in ["literal_token", "forbid_unrelated_absorption"] {
        if prompt.contains(forbidden) {
            return Err(err(format!("judge prompt leaked {forbidden}")));
        }
    }
    Ok(())
}

pub fn developer_instructions(eval_root: &std::path::Path) -> Result<String, Error> {
    let path = eval_root
        .join("judge")
        .join("v1")
        .join("developer-instructions.md");
    std::fs::read_to_string(&path).map_err(|error| err(format!("read {}: {error}", path.display())))
}
