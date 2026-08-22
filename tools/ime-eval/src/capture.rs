use std::fs;
use std::path::Path;

use crate::identity::artifact_identity_known;
use crate::types::{err, CaptureFile, Error};

/// Generic semantic captures retain their historical bounded-file limit so
/// loading an older capture does not change behavior outside Stage 1. The
/// quality lane applies its stricter 18-candidate production contract during
/// capture and scoring (`QUALITY_CANDIDATE_LIMIT`).
pub const MAX_CANDIDATES_PER_SYSTEM: usize = 64;
const MAX_CANDIDATE_BYTES: usize = 4096;
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

pub fn load_capture(path: &Path) -> Result<CaptureFile, Error> {
    let bytes = fs::read(path).map_err(|error| err(format!("read {}: {error}", path.display())))?;
    if bytes.len() > MAX_CAPTURE_BYTES {
        return Err(err(format!("capture exceeds {} bytes", MAX_CAPTURE_BYTES)));
    }
    let text = String::from_utf8(bytes).map_err(|_| err("capture is not UTF-8"))?;
    let capture: CaptureFile = serde_json::from_str(&text)
        .map_err(|error| err(format!("parse {}: {error}", path.display())))?;
    if capture.schema_version != 1 {
        return Err(err(format!(
            "unsupported capture schema_version {}",
            capture.schema_version
        )));
    }
    if !artifact_identity_known(&capture.baseline) || !artifact_identity_known(&capture.candidate) {
        return Err(err("capture artifact identity is incomplete or malformed"));
    }
    if capture.pairs.is_empty() {
        return Err(err("capture contains no pairs"));
    }
    validate_runtime("baseline", capture.baseline_capture.as_ref())?;
    validate_runtime("candidate", capture.candidate_capture.as_ref())?;
    for pair in &capture.pairs {
        if pair.case_id.is_empty() {
            return Err(err("capture contains an empty case_id"));
        }
        validate_candidates(&pair.baseline.candidates)?;
        validate_candidates(&pair.candidate.candidates)?;
    }
    let mut control_ids = std::collections::BTreeSet::new();
    for control in &capture.control_pairs {
        if control.control_id.is_empty() || !control_ids.insert(control.control_id.as_str()) {
            return Err(err("capture contains a duplicate or empty control_id"));
        }
        if control.reading.is_empty() || control.reading.len() > 1024 {
            return Err(err("capture control reading is empty or too long"));
        }
        validate_candidates(&control.baseline.candidates)?;
        validate_candidates(&control.candidate.candidates)?;
    }
    Ok(capture)
}

fn validate_runtime(
    name: &str,
    runtime: Option<&crate::types::CaptureRuntime>,
) -> Result<(), Error> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    if runtime.terminal.is_empty() || runtime.terminal.len() > 64 {
        return Err(err(format!(
            "{name} capture terminal state must be non-empty and at most 64 bytes"
        )));
    }
    Ok(())
}

fn validate_candidates(candidates: &[String]) -> Result<(), Error> {
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES_PER_SYSTEM {
        return Err(err(format!(
            "capture candidate count must be 1..={MAX_CANDIDATES_PER_SYSTEM}"
        )));
    }
    if candidates
        .iter()
        .any(|candidate| candidate.is_empty() || candidate.len() > MAX_CANDIDATE_BYTES)
    {
        return Err(err(format!(
            "capture candidate must be non-empty and at most {MAX_CANDIDATE_BYTES} bytes"
        )));
    }
    Ok(())
}
