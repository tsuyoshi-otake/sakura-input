use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::codex;
use crate::isolation::IsolationDir;
use crate::prompt::{self, PreparedPrompt};
use crate::schema;
use crate::types::{err, Error, JudgeResult, SemanticCase, Side, SystemOutput, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    PreferLiteral,
    AlwaysA,
    Codex,
}

impl BackendKind {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "prefer-literal" => Ok(Self::PreferLiteral),
            "always-a" => Ok(Self::AlwaysA),
            "codex" => Ok(Self::Codex),
            other => Err(err(format!("unknown backend {other}"))),
        }
    }
}

pub trait JudgeBackend {
    fn evaluate(
        &mut self,
        case: &SemanticCase,
        prepared: &PreparedPrompt,
        system_a: &SystemOutput,
        system_b: &SystemOutput,
    ) -> Result<JudgeResult, Error>;
}

#[derive(Debug)]
pub struct PreferLiteral;

impl JudgeBackend for PreferLiteral {
    fn evaluate(
        &mut self,
        case: &SemanticCase,
        prepared: &PreparedPrompt,
        system_a: &SystemOutput,
        system_b: &SystemOutput,
    ) -> Result<JudgeResult, Error> {
        let reading = case.input.reading.as_str();
        let a_ok = system_a.top1() == Some(reading);
        let b_ok = system_b.top1() == Some(reading);
        let (verdict, severity, code, reason) = match (a_ok, b_ok) {
            (true, false) => (
                Verdict::A,
                4,
                "literal_corruption",
                "SYSTEM_A preserved the literal token; SYSTEM_B did not",
            ),
            (false, true) => (
                Verdict::B,
                4,
                "literal_corruption",
                "SYSTEM_B preserved the literal token; SYSTEM_A did not",
            ),
            _ => (
                Verdict::Tie,
                0,
                "equivalent",
                "neither anonymous system uniquely preserved the literal token",
            ),
        };
        Ok(JudgeResult {
            case_id: prepared.opaque_id.clone(),
            verdict,
            severity,
            certainty: crate::types::Certainty::High,
            reason_codes: vec![code.to_owned()],
            short_reason: reason.to_owned(),
        })
    }
}

#[derive(Debug)]
pub struct AlwaysA;

impl JudgeBackend for AlwaysA {
    fn evaluate(
        &mut self,
        _case: &SemanticCase,
        prepared: &PreparedPrompt,
        _system_a: &SystemOutput,
        _system_b: &SystemOutput,
    ) -> Result<JudgeResult, Error> {
        Ok(JudgeResult {
            case_id: prepared.opaque_id.clone(),
            verdict: Verdict::A,
            severity: 3,
            certainty: crate::types::Certainty::High,
            reason_codes: vec!["semantic_fit".to_owned()],
            short_reason: "positional stub always selects SYSTEM_A".to_owned(),
        })
    }
}

#[derive(Debug)]
pub struct CodexBackend {
    temp_root: PathBuf,
    developer_instructions: String,
    schema_bytes: Vec<u8>,
    timeout: Duration,
}

impl CodexBackend {
    pub fn new(eval_root: &Path, temp_root: PathBuf, timeout: Duration) -> Result<Self, Error> {
        let developer_instructions = prompt::developer_instructions(eval_root)?;
        let schema_path = eval_root
            .join("judge")
            .join("v1")
            .join("result.schema.json");
        let schema_bytes = std::fs::read(&schema_path)
            .map_err(|error| err(format!("read {}: {error}", schema_path.display())))?;
        std::fs::create_dir_all(&temp_root)
            .map_err(|error| err(format!("create {}: {error}", temp_root.display())))?;
        Ok(Self {
            temp_root,
            developer_instructions,
            schema_bytes,
            timeout,
        })
    }
}

impl JudgeBackend for CodexBackend {
    fn evaluate(
        &mut self,
        _case: &SemanticCase,
        prepared: &PreparedPrompt,
        _system_a: &SystemOutput,
        _system_b: &SystemOutput,
    ) -> Result<JudgeResult, Error> {
        let isolation =
            IsolationDir::create(&self.temp_root, &prepared.case_json, &self.schema_bytes)?;
        let result = (|| {
            let plan = codex::plan_exec(
                &isolation.path,
                &self.developer_instructions,
                crate::types::REQUIRED_MODEL,
                crate::types::REQUIRED_REASONING,
            )?;
            let outcome =
                codex::run_exec(&plan, &isolation.path, &prepared.user_prompt, self.timeout)?;
            let judge_result = schema::parse_result(&outcome.result_json)?;
            if judge_result.case_id != prepared.opaque_id {
                return Err(err("Codex Judge returned a non-opaque or stale case_id"));
            }
            Ok(judge_result)
        })();
        let cleanup = isolation.remove();
        match (result, cleanup) {
            (Ok(judge_result), Ok(())) => Ok(judge_result),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(_cleanup_error)) => Err(error),
        }
    }
}

pub fn systems_for_assignment<'a>(
    assignment: crate::types::Assignment,
    baseline: &'a SystemOutput,
    candidate: &'a SystemOutput,
) -> (&'a SystemOutput, &'a SystemOutput) {
    match assignment.system_a {
        Side::Baseline => (baseline, candidate),
        Side::Candidate => (candidate, baseline),
    }
}
