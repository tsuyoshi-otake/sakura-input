use super::codec::record_at;
use super::format::{HEADER_LEN, REPAIR_SUPPRESS_CONTEXT, REPAIR_SUPPRESS_SURFACE};

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
        let (next, record) = record_at(self.bytes, self.version, self.offset)?;
        self.offset = next;
        if record.left_context == REPAIR_SUPPRESS_CONTEXT
            && record.right_context == REPAIR_SUPPRESS_CONTEXT
            && record.surface == REPAIR_SUPPRESS_SURFACE
        {
            Some(ReplayEvent::RepairSuppress {
                day: record.day,
                reading: record.reading,
            })
        } else {
            Some(ReplayEvent::Commit {
                day: record.day,
                left_context: record.left_context,
                right_context: record.right_context,
                reading: record.reading,
                surface: record.surface,
            })
        }
    }
}
