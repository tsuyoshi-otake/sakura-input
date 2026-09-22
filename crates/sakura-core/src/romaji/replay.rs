//! Replaying a raw romaji string with a per-event trace of which raw bytes
//! produced which output bytes, for repair and reconversion.

use sakura_values::{FixedStr, FixedVec};

use super::fsm::Emission;
use super::table::Step;
use super::{Sequence, Table, MAX_REPLAY_EVENTS, MAX_REPLAY_OUTPUT_BYTES, MAX_REPLAY_RAW_BYTES};

/// A single emission observed while replaying an append-only raw key span.
///
/// The source range is a byte range in the ASCII raw input.  The output range
/// is a character range in [`ReplayTrace::output`].  A carry can make source
/// ranges overlap an earlier emission (for example the second `t` in `tt`
/// is both the source of `っ` and the carried source of the following `つ`),
/// so consumers must treat these as provenance spans rather than a partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplayEvent {
    raw_start: u16,
    raw_end: u16,
    output_start: u16,
    output_end: u16,
    kind: ReplayEventKind,
}

impl ReplayEvent {
    /// Raw byte offset at which this emission starts.
    pub fn raw_start(&self) -> usize {
        usize::from(self.raw_start)
    }

    /// Raw byte offset immediately after this emission's source.
    pub fn raw_end(&self) -> usize {
        usize::from(self.raw_end)
    }

    /// Output character offset at which this emission starts.
    pub fn output_start(&self) -> usize {
        usize::from(self.output_start)
    }

    /// Output character offset immediately after this emission.
    pub fn output_end(&self) -> usize {
        usize::from(self.output_end)
    }

    /// Whether this emission was produced by a table entry or passed through
    /// as an unresolved raw ASCII character.
    pub fn kind(&self) -> ReplayEventKind {
        self.kind
    }
}

/// The provenance class of a replay emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplayEventKind {
    /// Output produced by a matching entry in the compiled Romaji table.
    #[default]
    Kana,
    /// A leading raw character for which the table had no matching entry.
    RawPassthrough,
}

/// A bounded, allocation-free replay of the actual compiled Romaji FSM.
///
/// [`Table::replay`] feeds every byte through the same `feed`/`drive` rules as
/// live input.  The output and source spans are retained only in fixed
/// buffers, making this suitable for a speculative local-completion probe.
/// It intentionally models live append-only input: a pending prefix is not
/// flushed at the end, because flushing would erase the distinction between
/// an unresolved key and a raw passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayTrace {
    pub(super) output: FixedStr<MAX_REPLAY_OUTPUT_BYTES>,
    events: FixedVec<ReplayEvent, MAX_REPLAY_EVENTS>,
    pending: Sequence,
    carry_overlap: usize,
}

impl ReplayTrace {
    fn new() -> Self {
        Self {
            output: FixedStr::new(),
            events: FixedVec::new(),
            pending: Sequence::new(),
            carry_overlap: 0,
        }
    }

    /// Kana and literal output emitted before the replay's final pending
    /// prefix.
    pub fn output(&self) -> &str {
        self.output.as_str()
    }

    /// All emitted source spans in deterministic FSM order.
    pub fn events(&self) -> &[ReplayEvent] {
        self.events.as_slice()
    }

    /// Raw passthrough emissions, in source order.
    pub fn raw_passthrough(&self) -> impl Iterator<Item = &ReplayEvent> {
        self.events
            .as_slice()
            .iter()
            .filter(|event| event.kind == ReplayEventKind::RawPassthrough)
    }

    /// Number of raw passthrough emissions.
    pub fn raw_passthrough_count(&self) -> usize {
        self.raw_passthrough().count()
    }

    /// The unresolved prefix left by live append-only replay.
    pub fn pending(&self) -> &str {
        self.pending.as_str()
    }

    /// Carry overlap associated with [`ReplayTrace::pending`].
    pub fn carry_overlap(&self) -> usize {
        self.carry_overlap.min(self.pending.len())
    }

    fn push_event(
        &mut self,
        output: &str,
        raw_start: usize,
        raw_end: usize,
        kind: ReplayEventKind,
    ) -> Result<(), ReplayError> {
        let output_start = self.output.as_str().chars().count();
        self.output
            .push_str(output)
            .map_err(|_| ReplayError::OutputOverflow)?;
        let output_end = output_start + output.chars().count();
        let event = ReplayEvent {
            raw_start: u16::try_from(raw_start).map_err(|_| ReplayError::TraceOverflow)?,
            raw_end: u16::try_from(raw_end).map_err(|_| ReplayError::TraceOverflow)?,
            output_start: u16::try_from(output_start).map_err(|_| ReplayError::TraceOverflow)?,
            output_end: u16::try_from(output_end).map_err(|_| ReplayError::TraceOverflow)?,
            kind,
        };
        self.events
            .push(event)
            .map_err(|_| ReplayError::TraceOverflow)
    }
}

/// Why a bounded raw replay could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayError {
    /// Replay input contained a non-ASCII character.  Physical raw key
    /// provenance is ASCII by contract; direct Kana must be suppressed.
    NonAsciiRaw,
    /// The caller supplied more raw bytes than the local probe can inspect.
    RawTooLong,
    /// The fixed event buffer could not retain the trace.
    TraceOverflow,
    /// The fixed output buffer could not retain the trace.
    OutputOverflow,
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NonAsciiRaw => f.write_str("raw replay requires ASCII input"),
            Self::RawTooLong => write!(f, "raw replay exceeds {MAX_REPLAY_RAW_BYTES} bytes"),
            Self::TraceOverflow => f.write_str("raw replay trace buffer overflow"),
            Self::OutputOverflow => f.write_str("raw replay output buffer overflow"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl Table {
    /// Replays an append-only ASCII raw-key span through this exact compiled
    /// table and retains bounded provenance events.
    ///
    /// Unlike [`Table::flush`], this leaves a final prefix pending.  That is
    /// important to a caller deciding whether a raw span is a structural
    /// anomaly: `n`, `k`, and a custom-table prefix are not equivalent to a
    /// raw passthrough merely because a commit would eventually flush them.
    pub fn replay(&self, raw: &str) -> Result<ReplayTrace, ReplayError> {
        if !raw.is_ascii() {
            return Err(ReplayError::NonAsciiRaw);
        }
        if raw.len() > MAX_REPLAY_RAW_BYTES {
            return Err(ReplayError::RawTooLong);
        }

        let mut trace = ReplayTrace::new();
        let mut candidate = Sequence::new();
        let mut overlap = 0usize;
        for (cursor, byte) in raw.bytes().enumerate() {
            let pending_start = cursor.saturating_sub(candidate.len());
            let key = (byte as char).to_ascii_lowercase();
            if candidate.push(key).is_err() {
                // A valid compiled table cannot normally reach this branch:
                // pending is always a proper prefix of an entry.  Keep the
                // defensive behavior in lock-step with `feed` for a custom
                // table or a future change to the sequence bound.
                let mut source_start = pending_start;
                self.drive_trace(
                    &mut candidate,
                    &mut overlap,
                    &mut trace,
                    &mut source_start,
                    cursor,
                    false,
                )?;
                candidate.clear();
                overlap = 0;
                candidate
                    .push(key)
                    .map_err(|_| ReplayError::TraceOverflow)?;
            }
            let mut source_start = if candidate.len() == 1 {
                cursor
            } else {
                pending_start
            };
            self.drive_trace(
                &mut candidate,
                &mut overlap,
                &mut trace,
                &mut source_start,
                cursor + 1,
                true,
            )?;
        }

        trace.pending = candidate;
        trace.carry_overlap = overlap;
        Ok(trace)
    }

    /// Replays and commits an append-only ASCII raw-key span.
    ///
    /// This convenience method is useful for tests and offline consumers
    /// that need the same terminal output as a real commit.  For structural
    /// anomaly admission use [`Table::replay`] so an ordinary unresolved
    /// `n`/`k` prefix is not mistaken for a raw passthrough.
    pub fn replay_committed(&self, raw: &str) -> Result<ReplayTrace, ReplayError> {
        let mut trace = self.replay(raw)?;
        let mut candidate = trace.pending.clone();
        if candidate.is_empty() {
            return Ok(trace);
        }
        let mut overlap = trace.carry_overlap;
        let pending_start = raw.len().saturating_sub(candidate.len());
        let mut source_start = pending_start;
        self.drive_trace(
            &mut candidate,
            &mut overlap,
            &mut trace,
            &mut source_start,
            raw.len(),
            false,
        )?;
        trace.pending = candidate;
        trace.carry_overlap = overlap;
        Ok(trace)
    }

    /// Trace-aware twin of [`Table::drive`] used only by the pure bounded
    /// replay helper.  Keeping the matching, longest-prefix, carry, and wait
    /// rules in this function tied to the same [`Step`] lookup prevents a
    /// second approximation of the Romaji FSM from becoming a repair oracle.
    fn drive_trace(
        &self,
        candidate: &mut Sequence,
        overlap: &mut usize,
        trace: &mut ReplayTrace,
        source_start: &mut usize,
        source_end: usize,
        may_wait: bool,
    ) -> Result<(), ReplayError> {
        loop {
            if candidate.is_empty() {
                *overlap = 0;
                *source_start = source_end;
                return Ok(());
            }
            if may_wait && self.extends(candidate.as_str()) {
                return Ok(());
            }

            let (emitted, consumed, carry) = match self.step_for(candidate.as_str()) {
                Step::Entry { index, consumed } => match self.entries.get(index) {
                    Some(entry) => (
                        Emission::Kana(&entry.output),
                        consumed,
                        entry.carry.as_str(),
                    ),
                    None => return Ok(()),
                },
                Step::Raw => match candidate.as_str().chars().next() {
                    Some(c) => (Emission::Raw(c), c.len_utf8(), ""),
                    None => return Ok(()),
                },
            };

            let mut next = Sequence::new();
            next.push_str(carry)
                .map_err(|_| ReplayError::TraceOverflow)?;
            if let Some(rest) = candidate.as_str().get(consumed..) {
                next.push_str(rest)
                    .map_err(|_| ReplayError::TraceOverflow)?;
            }

            let step_start = *source_start;
            // `candidate` is ASCII and is built from exactly the source range
            // tracked here.  Clamp defensively rather than allowing malformed
            // custom state to produce a backwards provenance span.
            let step_end = step_start
                .saturating_add(consumed)
                .min(source_end)
                .max(step_start);
            match emitted {
                Emission::Kana(kana) => {
                    trace.push_event(kana, step_start, step_end, ReplayEventKind::Kana)?
                }
                Emission::Raw(c) => {
                    let mut raw = [0u8; 4];
                    let text = c.encode_utf8(&mut raw);
                    trace.push_event(
                        text,
                        step_start,
                        step_end,
                        ReplayEventKind::RawPassthrough,
                    )?;
                }
            }
            *candidate = next;
            *overlap = carry.len() + overlap.saturating_sub(consumed);
            *source_start = if candidate.is_empty() {
                source_end
            } else {
                step_start
                    .saturating_add(consumed)
                    .saturating_sub(carry.len())
            };
        }
    }
}
