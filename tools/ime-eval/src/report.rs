use std::fmt::Write as _;

use crate::aggregate::Aggregate;
use crate::identity::RunIdentity;
use crate::types::{err, Error, GateCheck};

pub fn render(
    identity: &RunIdentity,
    aggregate: &Aggregate,
    checks: &[GateCheck],
    release_pass: bool,
) -> Result<String, Error> {
    let mut out = String::new();
    writeln!(out, "Sakura Input Quality Gate").map_err(fmt_err)?;
    writeln!(out, "=========================").map_err(fmt_err)?;
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "Candidate:").map_err(fmt_err)?;
    writeln!(out, "git: {}", identity.candidate.git_sha).map_err(fmt_err)?;
    writeln!(out, "engine SHA256: {}", identity.candidate.engine_sha256).map_err(fmt_err)?;
    writeln!(
        out,
        "dictionary SHA256: {}",
        identity.candidate.dictionary_sha256
    )
    .map_err(fmt_err)?;
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "Baseline:").map_err(fmt_err)?;
    writeln!(out, "git: {}", identity.baseline.git_sha).map_err(fmt_err)?;
    writeln!(out, "engine SHA256: {}", identity.baseline.engine_sha256).map_err(fmt_err)?;
    writeln!(
        out,
        "dictionary SHA256: {}",
        identity.baseline.dictionary_sha256
    )
    .map_err(fmt_err)?;
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "DETERMINISTIC").map_err(fmt_err)?;
    writeln!(out, "-------------").map_err(fmt_err)?;
    writeln!(out, "Pairs:              {}", aggregate.pair_count).map_err(fmt_err)?;
    writeln!(out, "Hard failures:      {}", aggregate.hard_failures).map_err(fmt_err)?;
    writeln!(out, "Literal corruption: {}", aggregate.literal_corruption).map_err(fmt_err)?;
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "SEMANTIC / LUNA MAX").map_err(fmt_err)?;
    writeln!(out, "-------------------").map_err(fmt_err)?;
    writeln!(out, "Gradable:        {}", aggregate.gradable).map_err(fmt_err)?;
    writeln!(out, "Candidate win:   {}", aggregate.candidate_better).map_err(fmt_err)?;
    writeln!(out, "Baseline win:    {}", aggregate.baseline_better).map_err(fmt_err)?;
    writeln!(out, "Tie:             {}", aggregate.tie).map_err(fmt_err)?;
    writeln!(out, "Unstable:        {}", aggregate.unstable).map_err(fmt_err)?;
    writeln!(out, "Ungradable:      {}", aggregate.ungradable).map_err(fmt_err)?;
    if let Some(rate) = aggregate.material_regression_rate() {
        writeln!(out, "Material regression: {:.2}%", rate * 100.0).map_err(fmt_err)?;
    }
    if let Some(rate) = aggregate.decisive_win_rate() {
        writeln!(out, "Decisive win rate:   {:.1}%", rate * 100.0).map_err(fmt_err)?;
    }
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "JUDGE").map_err(fmt_err)?;
    writeln!(out, "-----").map_err(fmt_err)?;
    writeln!(out, "model: {}", identity.model).map_err(fmt_err)?;
    writeln!(out, "reasoning: {}", identity.reasoning).map_err(fmt_err)?;
    writeln!(out, "prompt: {}", identity.prompt_sha256).map_err(fmt_err)?;
    writeln!(out, "schema: {}", identity.schema_sha256).map_err(fmt_err)?;
    writeln!(out, "Codex CLI: {}", identity.codex_cli_version).map_err(fmt_err)?;
    writeln!(out).map_err(fmt_err)?;
    writeln!(out, "RESULT").map_err(fmt_err)?;
    writeln!(out, "------").map_err(fmt_err)?;
    writeln!(
        out,
        "RELEASE: {}",
        if release_pass { "PASS" } else { "FAIL" }
    )
    .map_err(fmt_err)?;
    for check in checks {
        writeln!(
            out,
            "{}: {} ({})",
            check.id,
            if check.passed { "PASS" } else { "FAIL" },
            check.detail
        )
        .map_err(fmt_err)?;
    }
    Ok(out)
}

fn fmt_err(error: std::fmt::Error) -> Error {
    err(format!("format report: {error}"))
}
