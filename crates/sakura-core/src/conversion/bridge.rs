/// Typed sides of one connection-matrix lookup. Keeping them distinct at the
/// bridge boundary prevents a previous terminal right ID from being mistaken
/// for the left ID of a word that actually starts before the commit boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeftContextId(u16);

impl LeftContextId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RightContextId(u16);

impl RightContextId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

/// A bounded, volatile tail of the immediately preceding committed
/// conversion. `prefix_cost` is the selected tail path from
/// `prefix_right_id` through its final lexical edge, excluding its old EOS
/// connection. A combined tail+current path can therefore be normalized back
/// into the same cost domain as an ordinary current-only candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossCommitBridge<'a> {
    pub tail_reading: &'a str,
    pub tail_surface: &'a str,
    pub prefix_right_id: RightContextId,
    pub prefix_cost: i64,
}

/// Exact final system edge retained from the selected raw lattice path.
/// Ranges end at the selected candidate's reading/surface end, so only their
/// starts need to be carried. The engine slices them while it still owns the
/// exact commit strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitBridgeTail {
    pub reading_start: u16,
    pub text_start: u16,
    pub prefix_right_id: RightContextId,
    pub prefix_cost: i64,
}

pub(in crate::conversion) const NO_COMMIT_BRIDGE_ENTRY: u32 = u32::MAX;
pub(in crate::conversion) const NO_SYSTEM_ENTRY_INDEX: u32 = u32::MAX;

/// Compact identity for the final raw system edge. Keeping only eight bytes
/// in each candidate preserves the 128 KiB conversion-worker stack contract;
/// the mapped dictionary materializes its surface and cost on commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) struct CommitBridgeTailStorage {
    pub(in crate::conversion) entry_index: u32,
    pub(in crate::conversion) reading_start: u16,
    pub(in crate::conversion) prefix_right_id: u16,
}

impl CommitBridgeTailStorage {
    pub(in crate::conversion) const fn new(
        entry_index: u32,
        reading_start: u16,
        prefix_right_id: u16,
    ) -> Self {
        Self {
            entry_index,
            reading_start,
            prefix_right_id,
        }
    }
}

impl Default for CommitBridgeTailStorage {
    fn default() -> Self {
        Self {
            entry_index: NO_COMMIT_BRIDGE_ENTRY,
            reading_start: 0,
            prefix_right_id: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) enum BridgeBoundaryKind {
    SpanningEdge,
    TypedFrontier,
}
