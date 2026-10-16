use super::codec::{record_at, record_meaning, RecordMeaning};
use super::format::HEADER_LEN;

/// One verified durable event borrowed from a learning-log replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayEvent<'a> {
    Commit {
        day: u32,
        left_context: u16,
        right_context: u16,
        reading: &'a str,
        surface: &'a str,
    },
    RepairSuppress {
        day: u32,
        reading: &'a str,
    },
    /// A correction tombstone (#305): `surface` of `reading` was rejected in
    /// `left_context`; `right_context` is the rejected surface's right ID.
    Reject {
        day: u32,
        left_context: u16,
        right_context: u16,
        reading: &'a str,
        surface: &'a str,
    },
}

/// A bounded, fully verified log prefix replayed without owned string copies.
#[derive(Debug, Clone, Copy)]
pub struct ReplayView<'a> {
    bytes: &'a [u8],
    version: u16,
    offset: usize,
}

impl<'a> ReplayView<'a> {
    /// Constructs a view only after the owning transaction has verified the
    /// complete slice with `scan_records` or constructed it from verified frames.
    pub(crate) fn from_verified(bytes: &'a [u8], version: u16) -> Self {
        Self {
            bytes,
            version,
            offset: HEADER_LEN,
        }
    }
}

impl<'a> Iterator for ReplayView<'a> {
    type Item = ReplayEvent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (next, record) = record_at(self.bytes, self.version, self.offset)?;
            self.offset = next;
            return Some(match record_meaning(&record) {
                RecordMeaning::Commit => ReplayEvent::Commit {
                    day: record.day,
                    left_context: record.left_context,
                    right_context: record.right_context,
                    reading: record.reading,
                    surface: record.surface,
                },
                RecordMeaning::RepairSuppress => ReplayEvent::RepairSuppress {
                    day: record.day,
                    reading: record.reading,
                },
                RecordMeaning::Rejection { reading, surface } => ReplayEvent::Reject {
                    day: record.day,
                    left_context: record.left_context,
                    right_context: record.right_context,
                    reading,
                    surface,
                },
                RecordMeaning::Ignored => continue,
            });
        }
    }
}
