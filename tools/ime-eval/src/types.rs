use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

pub fn err(message: impl Into<String>) -> Error {
    Error(message.into())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Context {
    #[serde(default)]
    pub left: String,
    #[serde(default)]
    pub right: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Input {
    #[serde(default)]
    pub input_mode: Option<String>,
    pub reading: String,
    /// The physical character sequence used by the real-engine capture.
    /// It is intentionally separate from `reading`: a corpus case may
    /// describe a kana reading while the host actually sends romaji keys.
    #[serde(default)]
    pub typing: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Constraints {
    #[serde(default)]
    pub literal_token: bool,
    #[serde(default)]
    pub forbid_unrelated_absorption: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SemanticCase {
    pub schema_version: u32,
    pub case_id: String,
    pub task: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub context: Context,
    pub input: Input,
    #[serde(default)]
    pub constraints: Constraints,
    /// Provenance for locally approved derived cases. This is intentionally
    /// excluded from the blinded Judge view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_provenance: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SystemOutput {
    pub candidates: Vec<String>,
}

impl SystemOutput {
    pub fn top1(&self) -> Option<&str> {
        self.candidates.first().map(String::as_str)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub git_sha: String,
    pub engine_sha256: String,
    pub dictionary_sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CapturePair {
    pub case_id: String,
    pub baseline: SystemOutput,
    pub candidate: SystemOutput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureControlPair {
    pub control_id: String,
    pub reading: String,
    pub baseline: SystemOutput,
    pub candidate: SystemOutput,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureRuntime {
    pub terminal: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_us: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureFile {
    pub schema_version: u32,
    pub baseline: ArtifactIdentity,
    pub candidate: ArtifactIdentity,
    pub pairs: Vec<CapturePair>,
    /// Optional deterministic negative-control captures. Semantic captures
    /// predate this field; quality scoring requires the IDs declared by its
    /// fixture whenever that fixture declares controls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_pairs: Vec<CaptureControlPair>,
    /// Capture metadata is optional for schema-v1 files produced before the
    /// quality-stage timing contract. Quality reports expose the absence
    /// explicitly instead of fabricating elapsed time or terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_capture: Option<CaptureRuntime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_capture: Option<CaptureRuntime>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct JudgeManifest {
    pub schema_version: u32,
    pub judge_version: String,
    pub aggregation_version: u32,
    pub model: String,
    pub reasoning: String,
    pub codex_cli_version: String,
    pub fail_closed_on_effort_mismatch: bool,
    pub no_effort_downgrade: bool,
    pub fresh_exec_only: bool,
    pub blind_ab: bool,
    pub swap_decisive_cases: bool,
    #[serde(default = "default_calibration_schema_file")]
    pub calibration_schema_file: String,
}

fn default_calibration_schema_file() -> String {
    "calibration.schema.json".to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[serde(rename = "A")]
    A,
    #[serde(rename = "B")]
    B,
    #[serde(rename = "tie")]
    Tie,
    #[serde(rename = "ungradable")]
    Ungradable,
}

impl Verdict {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "A" => Ok(Self::A),
            "B" => Ok(Self::B),
            "tie" => Ok(Self::Tie),
            "ungradable" => Ok(Self::Ungradable),
            other => Err(err(format!("unknown verdict {other}"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::Tie => "tie",
            Self::Ungradable => "ungradable",
        }
    }

    pub fn swapped(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
            other => other,
        }
    }

    pub fn is_decisive(self) -> bool {
        matches!(self, Self::A | Self::B)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Certainty {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeResult {
    pub case_id: String,
    pub verdict: Verdict,
    pub severity: u8,
    pub certainty: Certainty,
    pub reason_codes: Vec<String>,
    pub short_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Baseline,
    Candidate,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assignment {
    pub system_a: Side,
}

impl Assignment {
    pub fn swapped(self) -> Self {
        Self {
            system_a: match self.system_a {
                Side::Baseline => Side::Candidate,
                Side::Candidate => Side::Baseline,
            },
        }
    }

    pub fn side_for(self, label: Verdict) -> Option<Side> {
        match label {
            Verdict::A => Some(self.system_a),
            Verdict::B => Some(self.swapped().system_a),
            Verdict::Tie | Verdict::Ungradable => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleVerdict {
    Pass,
    LiteralCorruption,
}

impl OracleVerdict {
    pub fn is_hard_failure(self) -> bool {
        matches!(self, Self::LiteralCorruption)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticOutcome {
    CandidateBetter,
    BaselineBetter,
    Tie,
    Ungradable,
    Unstable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseRecord {
    pub case_id: String,
    pub opaque_id: String,
    pub oracle_candidate: String,
    pub oracle_baseline: String,
    pub semantic: Option<SemanticOutcome>,
    pub severity: u8,
    pub unstable: bool,
    pub hard_failure: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateProfile {
    Phase1,
    Release,
}

impl GateProfile {
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "phase1" => Ok(Self::Phase1),
            "release" => Ok(Self::Release),
            other => Err(err(format!("unknown gate profile {other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateCheck {
    pub id: String,
    pub passed: bool,
    pub detail: String,
}

pub const REQUIRED_MODEL: &str = "gpt-5.6-luna";
pub const REQUIRED_REASONING: &str = "max";
pub const REASON_CODES: [&str; 11] = [
    "semantic_fit",
    "literal_corruption",
    "meaning_change",
    "unnatural_japanese",
    "candidate_ranking",
    "overcorrection",
    "underconversion",
    "mixed_script_handling",
    "correction_burden",
    "equivalent",
    "insufficient_context",
];

pub fn default_eval_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        roots.push(PathBuf::from(manifest));
    }
    roots
}
