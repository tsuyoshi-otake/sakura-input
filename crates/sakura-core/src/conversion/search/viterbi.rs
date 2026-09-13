use core::cmp::Ordering;

use super::super::{ConversionSearchTerminal, Surface};

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct SearchState {
    pub(in crate::conversion) cost: i64,
    pub(in crate::conversion) node: u32,
    pub(in crate::conversion) parent: u32,
    pub(in crate::conversion) class: PathClass,
    pub(in crate::conversion) depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) struct HeapItem {
    pub(in crate::conversion) estimate: i64,
    pub(in crate::conversion) sequence: u64,
    pub(in crate::conversion) state: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::conversion) enum PathClass {
    Neutral,
    Lexical,
    Reading,
    Katakana,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct SearchRun {
    pub(in crate::conversion) terminal: ConversionSearchTerminal,
    pub(in crate::conversion) states_pushed: usize,
    pub(in crate::conversion) incoherent_prefixes_pruned: usize,
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .estimate
            .cmp(&self.estimate)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PathClass {
    pub(in crate::conversion) fn extend(self, surface: Surface) -> Option<Self> {
        let next = match surface {
            Surface::Dictionary { .. } | Surface::User(_) => Self::Lexical,
            Surface::Reading => Self::Reading,
            Surface::Katakana => Self::Katakana,
            Surface::Literal(_) | Surface::Generated(_) => Self::Neutral,
        };
        match (self, next) {
            (current, Self::Neutral) => Some(current),
            (Self::Neutral, next) => Some(next),
            (current, next) if current == next => Some(current),
            _ => None,
        }
    }
}
