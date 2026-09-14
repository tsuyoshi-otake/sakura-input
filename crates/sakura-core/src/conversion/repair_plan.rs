use sakura_values::MAX_PREEDIT_BYTES;

use super::{
    RepairTier, DEFAULT_MAX_RAW_REPAIR_CANDIDATES, DEFAULT_MAX_RAW_REPAIR_LATTICE_NODES,
    DEFAULT_MAX_RAW_REPAIR_PASSES, DEFAULT_MAX_RAW_REPAIR_SEARCH_STATES, MAX_CORRECTION_RUNS,
};

/// Aggregate limits across all corrected readings in one raw-repair request.
/// Per-pass lattice/search limits are reset by the ordinary converter, so the
/// raw API applies these counters across the whole sequence as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawRepairBudget {
    pub max_corrected_passes: usize,
    pub max_repair_candidates: usize,
    pub max_lattice_nodes: usize,
    pub max_search_states: usize,
}

impl Default for RawRepairBudget {
    fn default() -> Self {
        Self {
            max_corrected_passes: DEFAULT_MAX_RAW_REPAIR_PASSES,
            max_repair_candidates: DEFAULT_MAX_RAW_REPAIR_CANDIDATES,
            max_lattice_nodes: DEFAULT_MAX_RAW_REPAIR_LATTICE_NODES,
            max_search_states: DEFAULT_MAX_RAW_REPAIR_SEARCH_STATES,
        }
    }
}

/// A single forward edit run.  No reverse/inferred mapping is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionRunKind {
    Equal,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrectionRun {
    pub corrected_start: u16,
    pub corrected_end: u16,
    pub original_start: u16,
    pub original_end: u16,
    pub kind: CorrectionRunKind,
}

impl CorrectionRun {
    pub const fn equal(
        corrected_start: u16,
        corrected_end: u16,
        original_start: u16,
        original_end: u16,
    ) -> Self {
        Self {
            corrected_start,
            corrected_end,
            original_start,
            original_end,
            kind: CorrectionRunKind::Equal,
        }
    }

    pub const fn replace(
        corrected_start: u16,
        corrected_end: u16,
        original_start: u16,
        original_end: u16,
    ) -> Self {
        Self {
            corrected_start,
            corrected_end,
            original_start,
            original_end,
            kind: CorrectionRunKind::Replace,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionMapError {
    TooManyRuns,
    EmptyRun,
    NonContiguous,
    InvalidEndpoint,
    InvalidUtf8Boundary,
    EqualRunMismatch,
    ReplaceRunUnchanged,
    SnapshotMismatch,
    LengthOverflow,
}

impl core::fmt::Display for CorrectionMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::TooManyRuns => "correction map has too many runs",
            Self::EmptyRun => "correction map contains an empty run",
            Self::NonContiguous => "correction map runs are not contiguous",
            Self::InvalidEndpoint => "correction map endpoint is out of range",
            Self::InvalidUtf8Boundary => "correction map endpoint is not a UTF-8 boundary",
            Self::EqualRunMismatch => "equal correction run does not contain equal text",
            Self::ReplaceRunUnchanged => "replace correction run contains unchanged text",
            Self::SnapshotMismatch => "correction map snapshot does not match the request",
            Self::LengthOverflow => "correction map length exceeds the u16 boundary type",
        };
        f.write_str(text)
    }
}

impl std::error::Error for CorrectionMapError {}

/// A bounded, heap-backed forward map from corrected-reading ranges to the
/// original raw-reading ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionMap {
    runs: Vec<CorrectionRun>,
    original_snapshot: String,
    corrected_snapshot: String,
    original_len: u16,
    corrected_len: u16,
}

impl CorrectionMap {
    pub fn new(
        original: &str,
        corrected: &str,
        runs: &[CorrectionRun],
    ) -> Result<Self, CorrectionMapError> {
        if original.len() > MAX_PREEDIT_BYTES || corrected.len() > MAX_PREEDIT_BYTES {
            return Err(CorrectionMapError::LengthOverflow);
        }
        let original_len =
            u16::try_from(original.len()).map_err(|_| CorrectionMapError::LengthOverflow)?;
        let corrected_len =
            u16::try_from(corrected.len()).map_err(|_| CorrectionMapError::LengthOverflow)?;
        if runs.len() > MAX_CORRECTION_RUNS {
            return Err(CorrectionMapError::TooManyRuns);
        }
        let mut previous_corrected = 0u16;
        let mut previous_original = 0u16;
        for run in runs {
            if run.corrected_start != previous_corrected || run.original_start != previous_original
            {
                return Err(CorrectionMapError::NonContiguous);
            }
            if run.corrected_end > corrected_len || run.original_end > original_len {
                return Err(CorrectionMapError::InvalidEndpoint);
            }
            if run.corrected_start >= run.corrected_end || run.original_start >= run.original_end {
                return Err(CorrectionMapError::EmptyRun);
            }
            let corrected_start = usize::from(run.corrected_start);
            let corrected_end = usize::from(run.corrected_end);
            let original_start = usize::from(run.original_start);
            let original_end = usize::from(run.original_end);
            if !corrected.is_char_boundary(corrected_start)
                || !corrected.is_char_boundary(corrected_end)
                || !original.is_char_boundary(original_start)
                || !original.is_char_boundary(original_end)
            {
                return Err(CorrectionMapError::InvalidUtf8Boundary);
            }
            match run.kind {
                CorrectionRunKind::Equal => {
                    if original[original_start..original_end]
                        != corrected[corrected_start..corrected_end]
                    {
                        return Err(CorrectionMapError::EqualRunMismatch);
                    }
                }
                CorrectionRunKind::Replace => {
                    if original[original_start..original_end]
                        == corrected[corrected_start..corrected_end]
                    {
                        return Err(CorrectionMapError::ReplaceRunUnchanged);
                    }
                }
            }
            previous_corrected = run.corrected_end;
            previous_original = run.original_end;
        }
        if previous_corrected != corrected_len || previous_original != original_len {
            return Err(CorrectionMapError::NonContiguous);
        }
        Ok(Self {
            runs: runs.to_vec(),
            original_snapshot: original.to_owned(),
            corrected_snapshot: corrected.to_owned(),
            original_len,
            corrected_len,
        })
    }

    pub const fn original_len(&self) -> u16 {
        self.original_len
    }

    pub const fn corrected_len(&self) -> u16 {
        self.corrected_len
    }

    pub fn runs(&self) -> &[CorrectionRun] {
        &self.runs
    }

    /// Re-validates the map against the exact snapshot used by a conversion
    /// pass.  Length equality alone is insufficient: an equal run created for
    /// a different same-length reading must fail closed.
    pub fn validate_for_readings(
        &self,
        original: &str,
        corrected: &str,
    ) -> Result<(), CorrectionMapError> {
        if self.original_len as usize != original.len()
            || self.corrected_len as usize != corrected.len()
        {
            return Err(CorrectionMapError::InvalidEndpoint);
        }
        let mut previous_corrected = 0u16;
        let mut previous_original = 0u16;
        for run in &self.runs {
            if run.corrected_start != previous_corrected || run.original_start != previous_original
            {
                return Err(CorrectionMapError::NonContiguous);
            }
            let corrected_start = usize::from(run.corrected_start);
            let corrected_end = usize::from(run.corrected_end);
            let original_start = usize::from(run.original_start);
            let original_end = usize::from(run.original_end);
            if run.corrected_start >= run.corrected_end || run.original_start >= run.original_end {
                return Err(CorrectionMapError::EmptyRun);
            }
            if !corrected.is_char_boundary(corrected_start)
                || !corrected.is_char_boundary(corrected_end)
                || !original.is_char_boundary(original_start)
                || !original.is_char_boundary(original_end)
            {
                return Err(CorrectionMapError::InvalidUtf8Boundary);
            }
            match run.kind {
                CorrectionRunKind::Equal
                    if original[original_start..original_end]
                        != corrected[corrected_start..corrected_end] =>
                {
                    return Err(CorrectionMapError::EqualRunMismatch);
                }
                CorrectionRunKind::Replace
                    if original[original_start..original_end]
                        == corrected[corrected_start..corrected_end] =>
                {
                    return Err(CorrectionMapError::ReplaceRunUnchanged);
                }
                _ => {}
            }
            previous_corrected = run.corrected_end;
            previous_original = run.original_end;
        }
        if previous_corrected != self.corrected_len || previous_original != self.original_len {
            return Err(CorrectionMapError::NonContiguous);
        }
        if original != self.original_snapshot || corrected != self.corrected_snapshot {
            return Err(CorrectionMapError::SnapshotMismatch);
        }
        Ok(())
    }

    /// Projects a corrected range only when both endpoints have an exact
    /// forward boundary.  A boundary inside a replacement run is rejected.
    pub fn project_corrected_range(&self, start: u16, end: u16) -> Option<(u16, u16)> {
        if start > end || end > self.corrected_len {
            return None;
        }
        if !self.corrected_snapshot.is_char_boundary(usize::from(start))
            || !self.corrected_snapshot.is_char_boundary(usize::from(end))
        {
            return None;
        }
        let original_start = self.project_boundary(start)?;
        let original_end = self.project_boundary(end)?;
        (original_start <= original_end).then_some((original_start, original_end))
    }

    fn project_boundary(&self, boundary: u16) -> Option<u16> {
        for run in &self.runs {
            if boundary == run.corrected_start {
                return Some(run.original_start);
            }
            if boundary == run.corrected_end {
                return Some(run.original_end);
            }
            if run.kind == CorrectionRunKind::Equal
                && boundary > run.corrected_start
                && boundary < run.corrected_end
            {
                let offset = boundary - run.corrected_start;
                let original_end = run.original_end - run.original_start;
                if offset <= original_end {
                    return Some(run.original_start + offset);
                }
            }
        }
        None
    }
}

/// One corrected-reading pass.  The map is validated before a plan is
/// accepted, and the plan owns only bounded heap data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRepairPlan {
    plan_id: u8,
    corrected_reading: String,
    pub(super) map: CorrectionMap,
    tier: RepairTier,
}

impl RawRepairPlan {
    pub fn new(
        plan_id: u8,
        corrected_reading: &str,
        map: CorrectionMap,
        tier: RepairTier,
    ) -> Result<Self, CorrectionMapError> {
        if corrected_reading.len() > MAX_PREEDIT_BYTES
            || usize::from(map.corrected_len()) != corrected_reading.len()
            || map.corrected_snapshot != corrected_reading
        {
            return Err(CorrectionMapError::InvalidEndpoint);
        }
        Ok(Self {
            plan_id,
            corrected_reading: corrected_reading.to_owned(),
            map,
            tier,
        })
    }

    pub const fn plan_id(&self) -> u8 {
        self.plan_id
    }

    pub fn corrected_reading(&self) -> &str {
        &self.corrected_reading
    }

    pub const fn map(&self) -> &CorrectionMap {
        &self.map
    }

    pub const fn tier(&self) -> RepairTier {
        self.tier
    }

    pub(super) fn is_valid_for(&self, original_reading: &str) -> bool {
        self.map.original_len() as usize == original_reading.len()
            && self.map.corrected_len() as usize == self.corrected_reading.len()
    }
}
