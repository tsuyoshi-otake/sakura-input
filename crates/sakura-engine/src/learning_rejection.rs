//! Bounded record of surfaces the user rejected by correcting them (#305).
//!
//! A tombstone says "in left context L, do not let learning lift surface X
//! of reading r". Each (r, L) also keeps one marker holding the sequence of
//! its newest tombstone: exact learning newer than the marker is the user's
//! choice after a correction and may pass the strength gate. The table is a
//! fixed four-way set-associative array, so memory and every probe stay O(1).
//! A full bucket overwrites its oldest slot; an overwritten tombstone or
//! marker simply stops applying.

use crate::session::text_hash;

const BUCKETS: usize = 512;
const WAYS: usize = 4;
pub(super) const REJECTION_SLOTS: usize = BUCKETS * WAYS;
/// Reserved surface length of the per-(reading, left context) marker. A
/// learned surface is bounded far below it by the commit size limit.
const MARKER_SURFACE_LEN: u16 = u16::MAX;

/// A hashed `(reading, left context, surface)` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RejectionKey {
    pub(super) reading_hash: u64,
    pub(super) reading_len: u16,
    pub(super) left_context: u16,
    pub(super) surface_hash: u64,
    pub(super) surface_len: u16,
}

impl RejectionKey {
    /// The key of `surface` of `reading` in `left_context`, or `None` when a
    /// length does not fit the learning index's key.
    pub(super) fn of(reading: &str, left_context: u16, surface: &str) -> Option<Self> {
        Some(Self {
            reading_hash: text_hash(reading),
            reading_len: u16::try_from(reading.len()).ok()?,
            left_context,
            surface_hash: text_hash(surface),
            surface_len: u16::try_from(surface.len()).ok()?,
        })
    }

    fn marker(self) -> Self {
        Self {
            surface_hash: 0,
            surface_len: MARKER_SURFACE_LEN,
            ..self
        }
    }

    fn bucket(self) -> usize {
        let mut hash = self.reading_hash.rotate_left(23) ^ self.surface_hash.rotate_right(7);
        hash ^= u64::from(self.left_context) << 40;
        hash ^= u64::from(self.reading_len) << 16 | u64::from(self.surface_len);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        hash ^= hash >> 33;
        (hash as usize) & (BUCKETS - 1)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    key: Option<RejectionKey>,
    sequence: u64,
}

pub(super) struct RejectionTable {
    slots: Box<[Slot]>,
}

/// The tombstones of one reading in one left context, reached through the
/// context's marker. Ranking asks for it once per conversion.
#[derive(Clone, Copy)]
pub(super) struct RejectionGate<'a> {
    table: &'a RejectionTable,
    marker_key: RejectionKey,
    marker: u64,
}

impl RejectionGate<'_> {
    /// Sequence of the context's newest tombstone.
    pub(super) fn marker(&self) -> u64 {
        self.marker
    }

    pub(super) fn rejects(&self, surface_hash: u64, surface_len: u16) -> bool {
        self.table.is_rejected(RejectionKey {
            surface_hash,
            surface_len,
            ..self.marker_key
        })
    }
}

impl core::fmt::Debug for RejectionTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let occupied = self.slots.iter().filter(|slot| slot.key.is_some()).count();
        f.debug_struct("RejectionTable")
            .field("occupied", &occupied)
            .finish()
    }
}

impl RejectionTable {
    pub(super) fn new() -> Self {
        Self {
            slots: vec![Slot::default(); REJECTION_SLOTS].into_boxed_slice(),
        }
    }

    /// Records the tombstone and moves its (reading, left context) marker to
    /// `sequence`.
    pub(super) fn reject(&mut self, key: RejectionKey, sequence: u64) {
        self.upsert(key, sequence);
        self.upsert(key.marker(), sequence);
    }

    /// The surface was chosen again in this context: its tombstone ends. The
    /// marker stays, so the new choice still outranks the strength gate.
    pub(super) fn reinstate(&mut self, key: RejectionKey) {
        if let Some(slot) = self.find_mut(key) {
            *slot = Slot::default();
        }
    }

    fn is_rejected(&self, key: RejectionKey) -> bool {
        self.find(key).is_some()
    }

    /// The gate of a reading in a left context, or `None` when no tombstone
    /// of that context survives in the table.
    pub(super) fn gate(
        &self,
        reading_hash: u64,
        reading_len: u16,
        left_context: u16,
    ) -> Option<RejectionGate<'_>> {
        let marker_key = RejectionKey {
            reading_hash,
            reading_len,
            left_context,
            surface_hash: 0,
            surface_len: MARKER_SURFACE_LEN,
        };
        self.find(marker_key).map(|slot| RejectionGate {
            table: self,
            marker_key,
            marker: slot.sequence,
        })
    }

    fn ways(&self, key: RejectionKey) -> &[Slot] {
        let start = key.bucket() * WAYS;
        &self.slots[start..start + WAYS]
    }

    fn find(&self, key: RejectionKey) -> Option<&Slot> {
        self.ways(key).iter().find(|slot| slot.key == Some(key))
    }

    fn find_mut(&mut self, key: RejectionKey) -> Option<&mut Slot> {
        let start = key.bucket() * WAYS;
        self.slots[start..start + WAYS]
            .iter_mut()
            .find(|slot| slot.key == Some(key))
    }

    fn upsert(&mut self, key: RejectionKey, sequence: u64) {
        let start = key.bucket() * WAYS;
        let ways = &mut self.slots[start..start + WAYS];
        let index = ways
            .iter()
            .position(|slot| slot.key == Some(key))
            .or_else(|| ways.iter().position(|slot| slot.key.is_none()))
            .unwrap_or_else(|| {
                ways.iter()
                    .enumerate()
                    .min_by_key(|(_, slot)| slot.sequence)
                    .map(|(index, _)| index)
                    .expect("every bucket has at least one way")
            });
        ways[index] = Slot {
            key: Some(key),
            sequence,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(reading: u64, left: u16, surface: u64) -> RejectionKey {
        RejectionKey {
            reading_hash: reading,
            reading_len: 6,
            left_context: left,
            surface_hash: surface,
            surface_len: 6,
        }
    }

    fn marker(table: &RejectionTable, reading: u64, left: u16) -> Option<u64> {
        table.gate(reading, 6, left).map(|gate| gate.marker())
    }

    #[test]
    fn a_tombstone_applies_only_to_its_reading_context_and_surface() {
        let mut table = RejectionTable::new();
        table.reject(key(1, 10, 100), 5);
        assert!(table.is_rejected(key(1, 10, 100)));
        assert!(!table.is_rejected(key(1, 11, 100)), "other left context");
        assert!(!table.is_rejected(key(1, 10, 101)), "other surface");
        assert!(!table.is_rejected(key(2, 10, 100)), "other reading");
        assert_eq!(marker(&table, 1, 10), Some(5));
        assert_eq!(marker(&table, 1, 11), None);
        let gate = table.gate(1, 6, 10).expect("gate");
        assert!(gate.rejects(100, 6));
        assert!(!gate.rejects(101, 6));
    }

    #[test]
    fn the_marker_follows_the_newest_tombstone_and_survives_reinstatement() {
        let mut table = RejectionTable::new();
        table.reject(key(1, 10, 100), 5);
        table.reject(key(1, 10, 101), 9);
        assert_eq!(marker(&table, 1, 10), Some(9));
        table.reinstate(key(1, 10, 101));
        assert!(!table.is_rejected(key(1, 10, 101)));
        assert!(table.is_rejected(key(1, 10, 100)));
        assert_eq!(marker(&table, 1, 10), Some(9));
    }

    #[test]
    fn a_full_bucket_overwrites_its_oldest_slot_and_the_table_stays_fixed() {
        let mut table = RejectionTable::new();
        let first = key(1, 10, 100);
        let bucket = first.bucket();
        let mut same_bucket = Vec::new();
        let mut probe = 0u64;
        while same_bucket.len() < WAYS {
            probe += 1;
            let candidate = key(1_000 + probe, 10, 100);
            if candidate.bucket() == bucket && candidate.marker().bucket() != bucket {
                same_bucket.push(candidate);
            }
        }
        table.reject(first, 1);
        for (offset, candidate) in same_bucket.iter().enumerate() {
            table.reject(*candidate, 2 + offset as u64);
        }
        assert!(!table.is_rejected(first), "the oldest slot was overwritten");
        assert!(same_bucket
            .iter()
            .all(|candidate| table.is_rejected(*candidate)));
        assert_eq!(table.slots.len(), REJECTION_SLOTS);
    }
}
