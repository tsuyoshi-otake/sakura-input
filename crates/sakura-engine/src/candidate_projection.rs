//! The bounded, display-facing view of a conversion candidate list.
//!
//! The converter owns the order and identity of raw candidates.  The engine
//! may nevertheless render two different raw entries as the same string: the
//! width/punctuation normalizer can collapse (for example) `第3番` and
//! `第３番`.  Keeping that collapse at the display boundary is important: the
//! converter's ranking and learning keys remain untouched, while every
//! consumer of the visible list uses one explicit mapping back to the raw
//! representative.
//!
//! A projection is deliberately a small value object rather than a collection
//! of ad-hoc `position` calls.  It owns both directions of the mapping and
//! builds them with a fixed-capacity open-addressed table.  Hashes are only a
//! fast rejection: equal hashes still compare the complete normalized UTF-8
//! surface, so a collision cannot merge intentionally distinct candidates.
//! The probe budget is bounded as well; malformed or adversarial input then
//! fails closed instead of turning candidate rendering into an unbounded
//! search.

use sakura_proto::{FixedStr, Overflow, MAX_CANDIDATES, MAX_PREEDIT_BYTES};

/// One more than the largest representable raw/visible index.  `u16::MAX` is
/// reserved as the invalid mapping marker; the wire list itself is bounded by
/// `MAX_CANDIDATES`.
const INVALID_INDEX: u16 = u16::MAX;

/// Keep the load factor at or below 50%.  The table is deliberately fixed and
/// twice as large as the wire ceiling; indexing uses remainder rather than a
/// power-of-two mask so a future protocol ceiling need not preserve that
/// implementation detail.
const TABLE_CAPACITY: usize = MAX_CANDIDATES * 2;

/// A fixed probe budget is the fail-closed boundary for the hash table.  At a
/// 50% load factor ordinary input stays well below this value.  A pathological
/// set of hash collisions cannot make the keystroke path quadratic: it is
/// rejected with [`ProjectionError::ProbeLimit`].
const MAX_PROBES: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Slot {
    hash: u64,
    raw_index: u16,
    visible_index: u16,
    occupied: bool,
}

/// Why a visible projection could not be built.  Callers should treat every
/// variant as a recoverable, fail-closed rendering failure and keep the raw
/// converter order authoritative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionError {
    /// The source supplied more rows than the protocol can expose.
    TooManyCandidates,
    /// The callback could not materialize a normalized surface in the bounded
    /// temporary buffer.
    SurfaceOverflow,
    /// A candidate produced no visible surface.  Empty rows cannot be mapped
    /// safely to a renderer or commit target.
    EmptySurface,
    /// The bounded hash-table probe budget was exhausted.  This is an
    /// explicit fail-closed outcome for collision-heavy input.
    ProbeLimit,
}

impl From<Overflow> for ProjectionError {
    fn from(_: Overflow) -> Self {
        Self::SurfaceOverflow
    }
}

/// Stable mapping between a converter's raw candidate order and its visible
/// normalized order.
///
/// The first raw candidate for each normalized surface is the visible
/// representative.  `raw_to_visible` maps later duplicate raw rows to that
/// same visible row, while `visible_to_raw` always points at the first raw
/// row.  Both arrays are retained even though many callers need only one
/// direction; retaining the pair prevents a future consumer from silently
/// reintroducing a surface search with different semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateProjection {
    raw_count: usize,
    visible_count: usize,
    truncated: bool,
    raw_to_visible: [u16; MAX_CANDIDATES],
    visible_to_raw: [u16; MAX_CANDIDATES],
}

impl Default for CandidateProjection {
    fn default() -> Self {
        Self {
            raw_count: 0,
            visible_count: 0,
            truncated: false,
            raw_to_visible: [INVALID_INDEX; MAX_CANDIDATES],
            visible_to_raw: [INVALID_INDEX; MAX_CANDIDATES],
        }
    }
}

impl CandidateProjection {
    /// Builds a projection for `raw_count` rows.
    ///
    /// `render` must write exactly the surface that the renderer and commit
    /// path expose for a raw candidate.  It is called once for every raw row,
    /// and again only when a hash collision needs an exact UTF-8 comparison.
    /// The callback receives a reusable bounded sink owned by this module, so
    /// projection construction itself never allocates.
    #[cfg(test)]
    pub(crate) fn build(
        raw_count: usize,
        render: impl FnMut(usize, &mut FixedStr<MAX_PREEDIT_BYTES>) -> Result<(), Overflow>,
    ) -> Result<Self, ProjectionError> {
        Self::build_internal(raw_count, render, false)
    }

    /// Builds a display projection while retaining the already-renderable
    /// prefix when a later row exceeds the shared preedit arena.  This mirrors
    /// the output builder's existing bounded-list behavior: the selected row
    /// must still be representable, while an unselected tail may be omitted
    /// explicitly rather than making an otherwise valid conversion vanish.
    /// Any row that cannot be represented before the first visible row remains
    /// a hard error, as do empty rows and probe exhaustion.
    pub(crate) fn build_prefix(
        raw_count: usize,
        render: impl FnMut(usize, &mut FixedStr<MAX_PREEDIT_BYTES>) -> Result<(), Overflow>,
    ) -> Result<Self, ProjectionError> {
        Self::build_internal(raw_count, render, true)
    }

    fn build_internal(
        raw_count: usize,
        mut render: impl FnMut(usize, &mut FixedStr<MAX_PREEDIT_BYTES>) -> Result<(), Overflow>,
        allow_tail_overflow: bool,
    ) -> Result<Self, ProjectionError> {
        if raw_count > MAX_CANDIDATES {
            return Err(ProjectionError::TooManyCandidates);
        }

        let mut projection = Self {
            raw_count,
            ..Self::default()
        };
        // Deduplication scratch is only needed during construction. Returning
        // it through every projection consumer multiplied the worker stack
        // footprint as display capacity grew. The result needs just mappings.
        let mut slots = [Slot::default(); TABLE_CAPACITY];

        for raw_index in 0..raw_count {
            let mut surface = FixedStr::<MAX_PREEDIT_BYTES>::new();
            if let Err(overflow) = render(raw_index, &mut surface) {
                if allow_tail_overflow && projection.visible_count > 0 {
                    projection.truncated = true;
                    break;
                }
                return Err(overflow.into());
            }
            if surface.is_empty() {
                return Err(ProjectionError::EmptySurface);
            }
            let hash = stable_hash(surface.as_bytes());
            let mut slot_index = (hash as usize) % TABLE_CAPACITY;
            let mut mapped = false;

            for _ in 0..MAX_PROBES {
                let slot = slots[slot_index];
                if !slot.occupied {
                    let visible_index = projection.visible_count;
                    let raw_index =
                        u16::try_from(raw_index).map_err(|_| ProjectionError::TooManyCandidates)?;
                    let visible_index = u16::try_from(visible_index)
                        .map_err(|_| ProjectionError::TooManyCandidates)?;
                    slots[slot_index] = Slot {
                        hash,
                        raw_index,
                        visible_index,
                        occupied: true,
                    };
                    projection.raw_to_visible[usize::from(raw_index)] = visible_index;
                    projection.visible_to_raw[usize::from(visible_index)] = raw_index;
                    projection.visible_count += 1;
                    mapped = true;
                    break;
                }

                if slot.hash == hash {
                    // Hash equality is not key equality.  Re-materialize the
                    // first raw representative and compare complete surfaces
                    // before merging this row, preserving distinct surfaces
                    // even under a deliberate hash collision.
                    let mut representative = FixedStr::<MAX_PREEDIT_BYTES>::new();
                    render(usize::from(slot.raw_index), &mut representative)?;
                    if representative.as_str() == surface.as_str() {
                        projection.raw_to_visible[raw_index] = slot.visible_index;
                        mapped = true;
                        break;
                    }
                }

                slot_index = (slot_index + 1) % TABLE_CAPACITY;
            }

            if !mapped {
                return Err(ProjectionError::ProbeLimit);
            }
        }

        Ok(projection)
    }

    #[cfg(test)]
    pub(crate) const fn raw_count(&self) -> usize {
        self.raw_count
    }

    #[cfg(test)]
    pub(crate) const fn visible_count(&self) -> usize {
        self.visible_count
    }

    pub(crate) const fn is_complete(&self) -> bool {
        !self.truncated
    }

    /// Converts a visible index to the stable first raw representative.
    pub(crate) fn raw_index(&self, visible_index: usize) -> Option<usize> {
        if visible_index >= self.visible_count {
            return None;
        }
        let raw_index = self.visible_to_raw[visible_index];
        (raw_index != INVALID_INDEX).then_some(usize::from(raw_index))
    }

    /// Converts a raw index to its visible row, merging normalized duplicates
    /// into the first raw representative.
    pub(crate) fn visible_index(&self, raw_index: usize) -> Option<usize> {
        if raw_index >= self.raw_count {
            return None;
        }
        let visible_index = self.raw_to_visible[raw_index];
        (visible_index != INVALID_INDEX).then_some(usize::from(visible_index))
    }

    pub(crate) fn visible_indices(&self) -> core::ops::Range<usize> {
        0..self.visible_count
    }

    /// Maps an arbitrary signed selection into visible list space.  Keeping
    /// the signed input matches the session's wrap-friendly selection state.
    pub(crate) fn normalize_selection(&self, selection: i16) -> Option<usize> {
        let count = i16::try_from(self.visible_count).ok()?;
        if self.truncated && selection >= count {
            // A positive index beyond a truncated prefix names a candidate
            // whose visible surface was not materialized. Wrapping it into
            // the prefix would commit or render a different candidate.
            return None;
        }
        (count > 0).then(|| selection.rem_euclid(count) as usize)
    }
}

/// Stable, process-independent FNV-1a over the normalized UTF-8 bytes.
fn stable_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::width::Normalizer;
    use sakura_proto::Mode;

    fn normalized_projection(values: &[&str]) -> CandidateProjection {
        let normalizer = Normalizer::default();
        CandidateProjection::build(values.len(), |index, output| {
            normalizer.normalize_into(values[index], Mode::Hiragana, output)
        })
        .expect("fixture projection")
    }

    #[test]
    fn first_raw_surface_is_stable_representative_for_normalized_duplicates() {
        let projection = normalized_projection(&["第3番", "第３番", "第4番"]);

        assert_eq!(projection.raw_count(), 3);
        assert_eq!(projection.visible_count(), 2);
        assert_eq!(projection.raw_index(0), Some(0));
        assert_eq!(projection.raw_index(1), Some(2));
        assert_eq!(projection.visible_index(0), Some(0));
        assert_eq!(projection.visible_index(1), Some(0));
        assert_eq!(projection.visible_index(2), Some(1));
    }

    #[test]
    fn distinct_normalized_surfaces_are_not_merged() {
        let projection = normalized_projection(&["第3番", "第３番"]);
        // Default policy deliberately collapses ASCII/full-width digits.
        assert_eq!(projection.visible_count(), 1);

        let projection =
            CandidateProjection::build(2, |index, output| output.push_str(["a", "A"][index]))
                .expect("case-sensitive projection");
        assert_eq!(projection.visible_count(), 2);
        assert_eq!(projection.raw_index(0), Some(0));
        assert_eq!(projection.raw_index(1), Some(1));
    }

    #[test]
    fn oversized_source_fails_closed() {
        assert_eq!(
            CandidateProjection::build(MAX_CANDIDATES + 1, |_, _| Ok(())),
            Err(ProjectionError::TooManyCandidates)
        );

        let oversized = "あ".repeat(MAX_PREEDIT_BYTES / 3 + 1);
        assert_eq!(
            CandidateProjection::build(1, |_, output| output.push_str(&oversized)),
            Err(ProjectionError::SurfaceOverflow)
        );
    }

    #[test]
    fn prefix_projection_truncates_only_an_unrenderable_tail() {
        let oversized = "あ".repeat(MAX_PREEDIT_BYTES / 3 + 1);
        let projection = CandidateProjection::build_prefix(2, |index, output| {
            if index == 0 {
                output.push_str("first")
            } else {
                output.push_str(&oversized)
            }
        })
        .expect("renderable prefix");

        assert!(!projection.is_complete());
        assert_eq!(projection.visible_count(), 1);
        assert_eq!(projection.raw_index(0), Some(0));
        assert_eq!(projection.visible_index(0), Some(0));
        assert_eq!(projection.visible_index(1), None);
    }

    #[test]
    fn mcc_mcdc_projection_tail_overflow() {
        let oversized = "あ".repeat(MAX_PREEDIT_BYTES / 3 + 1);
        for (allow_tail, has_prefix, truncated) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            let overflow_index = usize::from(has_prefix);
            let result = CandidateProjection::build_internal(
                overflow_index + 1,
                |index, output| {
                    output.push_str(if index == overflow_index {
                        &oversized
                    } else {
                        "first"
                    })
                },
                allow_tail,
            );
            if truncated {
                let projection = result.unwrap();
                assert!(projection.truncated);
                assert_eq!(projection.visible_count(), 1);
                assert_eq!(projection.raw_index(0), Some(0));
            } else {
                assert_eq!(result, Err(ProjectionError::SurfaceOverflow));
            }
            println!(
                "decision-evidence projection.tail_overflow {}{} {}",
                u8::from(allow_tail),
                u8::from(has_prefix),
                u8::from(truncated)
            );
        }
    }
}
