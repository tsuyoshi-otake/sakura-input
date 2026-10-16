//! Detects a commit the user erased and retyped with another spelling (#305).
//!
//! The trail is owned by one session and never allocates. The dispatcher
//! reports commits, idle single-character erasures and applied commit undos;
//! a commit answers with a [`Correction`] when it retypes the reading of the
//! erased commit next to the caret with a different surface. The trail never
//! touches learning itself: the correction rides on the retyped commit's
//! staged learning record (#275), so it takes effect only when that commit
//! survives the user's next action.
//!
//! The document model is deliberately small. Standing commits sit left of the
//! caret, newest last. Erasing a whole standing commit moves it to the pending
//! stack, whose top is the erased commit the next typed text replaces. Any
//! uncertainty about what the host deleted ends in "no match": a partial
//! erase, an erase reaching beyond the remembered commits, a commit too long
//! to track, or any key other than a plain erase clears the trail.

use sakura_proto::FixedStr;

/// Commits remembered across erasures, standing and pending together.
const MAX_TRAIL_ENTRIES: usize = 4;
/// Longest tracked reading or surface. A longer commit still occupies an
/// entry so its characters are counted off, but it never matches.
pub(crate) const MAX_TRAIL_TEXT_BYTES: usize = 96;
/// Rejected surfaces carried by one correction: the first and the latest.
pub(crate) const MAX_REJECTED_SURFACES: usize = 2;

pub(crate) type TrailText = FixedStr<MAX_TRAIL_TEXT_BYTES>;

/// One surface the user erased and replaced, with the right ID it was
/// committed with (the key of its exact-context learning).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RejectedSurface {
    surface: TrailText,
    right_context: u16,
}

impl RejectedSurface {
    pub(crate) fn surface(&self) -> &str {
        self.surface.as_str()
    }

    pub(crate) fn right_context(&self) -> u16 {
        self.right_context
    }
}

/// Up to [`MAX_REJECTED_SURFACES`] distinct surfaces, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RejectedList {
    items: [RejectedSurface; MAX_REJECTED_SURFACES],
    len: u8,
}

impl RejectedList {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &RejectedSurface> {
        self.items[..usize::from(self.len)].iter()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Drops every rejected copy of `surface`.
    pub(crate) fn remove(&mut self, surface: &str) {
        let mut kept = 0usize;
        for index in 0..usize::from(self.len) {
            if self.items[index].surface.as_str() != surface {
                self.items.swap(kept, index);
                kept += 1;
            }
        }
        for item in &mut self.items[kept..usize::from(self.len)] {
            *item = RejectedSurface::default();
        }
        self.len = kept as u8;
    }

    /// `carried`, then `latest`, without duplicates and without `chosen`,
    /// keeping the first and the latest when more remain.
    fn merged(carried: &Self, latest: Option<(&str, u16)>, chosen: &str) -> Self {
        let mut all: [Option<(&str, u16)>; MAX_REJECTED_SURFACES + 1] =
            [None; MAX_REJECTED_SURFACES + 1];
        let mut count = 0usize;
        let candidates = carried
            .iter()
            .map(|item| (item.surface.as_str(), item.right_context))
            .chain(latest);
        for (surface, right_context) in candidates {
            if surface == chosen
                || all[..count]
                    .iter()
                    .flatten()
                    .any(|(seen, _)| *seen == surface)
            {
                continue;
            }
            all[count] = Some((surface, right_context));
            count += 1;
        }
        let picked: [Option<(&str, u16)>; MAX_REJECTED_SURFACES] = match count {
            0 => [None, None],
            1 => [all[0], None],
            _ => [all[0], all[count - 1]],
        };
        let mut merged = Self::default();
        for (surface, right_context) in picked.into_iter().flatten() {
            let mut text = TrailText::new();
            if text.push_str(surface).is_ok() {
                let index = usize::from(merged.len);
                merged.items[index] = RejectedSurface {
                    surface: text,
                    right_context,
                };
                merged.len += 1;
            }
        }
        merged
    }
}

/// A retyped commit replaced these surfaces of the same reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Correction {
    origin_left: u16,
    rejected: RejectedList,
}

impl Correction {
    /// The left context the first erased commit was made in.
    pub(crate) fn origin_left(&self) -> u16 {
        self.origin_left
    }

    pub(crate) fn rejected(&self) -> &RejectedList {
        &self.rejected
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TrailEntry {
    /// Unicode scalar values of the committed surface, as the host erases them.
    chars: usize,
    tracked: bool,
    reading: TrailText,
    surface: TrailText,
    origin_left: u16,
    right_context: u16,
    rejected: RejectedList,
}

/// The session's recent commits and erasures (#305). See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CorrectionTrail {
    /// Standing commits first, oldest to newest, then the pending stack,
    /// bottom to top.
    entries: [TrailEntry; MAX_TRAIL_ENTRIES],
    standing: u8,
    len: u8,
    /// Erased characters not yet accounted for by whole standing commits.
    erase_overflow: usize,
}

impl CorrectionTrail {
    /// Forgets everything: the caret or the text left of it may have changed
    /// in a way the trail cannot follow.
    pub(crate) fn clear(&mut self) {
        for entry in &mut self.entries[..usize::from(self.len)] {
            *entry = TrailEntry::default();
        }
        self.standing = 0;
        self.len = 0;
        self.erase_overflow = 0;
    }

    /// Records one persisted commit and reports whether it corrected the
    /// erased commit next to the caret.
    pub(crate) fn commit(
        &mut self,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
    ) -> Option<Correction> {
        if self.erase_overflow > 0 {
            // The host erased part of a commit, or more than the trail knows:
            // nothing left of the caret is described any more.
            self.clear();
        }
        let matched = self.take_pending_top(reading);
        if matched.is_none() {
            // The new text replaced the erased commits instead of retyping
            // the one next to the caret.
            self.drop_pending();
        }

        let mut entry = TrailEntry {
            chars: surface.chars().count(),
            tracked: false,
            reading: TrailText::new(),
            surface: TrailText::new(),
            origin_left: left_context,
            right_context,
            rejected: RejectedList::default(),
        };
        entry.tracked = entry.reading.push_str(reading).is_ok()
            && entry.surface.push_str(surface).is_ok()
            && !reading.is_empty()
            && !surface.is_empty();
        if !entry.tracked {
            entry.reading.clear();
            entry.surface.clear();
        }

        let correction = matched.filter(|_| entry.tracked).and_then(|matched| {
            entry.origin_left = matched.origin_left;
            let latest = (matched.surface.as_str() != surface)
                .then_some((matched.surface.as_str(), matched.right_context));
            entry.rejected = RejectedList::merged(&matched.rejected, latest, surface);
            (!entry.rejected.is_empty()).then(|| Correction {
                origin_left: matched.origin_left,
                rejected: entry.rejected.clone(),
            })
        });
        self.push_standing(entry);
        correction
    }

    /// Whether a commit of `reading` now would retype the erased commit next
    /// to the caret, with the same surface or another.
    #[cfg(feature = "replay-eval")]
    pub(crate) fn would_retype(&self, reading: &str) -> bool {
        self.erase_overflow == 0
            && self.len > self.standing
            && self.entries[usize::from(self.len) - 1].tracked
            && self.entries[usize::from(self.len) - 1].reading.as_str() == reading
    }

    /// One idle Backspace the host handled: one character left of the caret
    /// is gone.
    pub(crate) fn erase(&mut self) {
        self.erase_overflow = self.erase_overflow.saturating_add(1);
        while self.standing > 0 {
            let newest = usize::from(self.standing) - 1;
            let chars = self.entries[newest].chars;
            if self.erase_overflow < chars {
                break;
            }
            self.erase_overflow -= chars;
            self.move_newest_standing_to_pending();
        }
    }

    /// The host applied a commit undo that deleted `surface` whole.
    pub(crate) fn undo_applied(&mut self, surface: &str) {
        let newest_matches = self.erase_overflow == 0
            && self.standing > 0
            && self.entries[usize::from(self.standing) - 1].tracked
            && self.entries[usize::from(self.standing) - 1]
                .surface
                .as_str()
                == surface;
        if newest_matches {
            self.move_newest_standing_to_pending();
        } else {
            self.clear();
        }
    }

    /// Strips `surface` of `reading` from every carried rejection, so a
    /// forgotten pair is not rejected again by a later correction.
    pub(crate) fn forget(&mut self, reading: &str, surface: &str) {
        for entry in &mut self.entries[..usize::from(self.len)] {
            if entry.reading.as_str() == reading {
                entry.rejected.remove(surface);
            }
        }
    }

    fn take_pending_top(&mut self, reading: &str) -> Option<TrailEntry> {
        if self.len == self.standing {
            return None;
        }
        let top = usize::from(self.len) - 1;
        let entry = &self.entries[top];
        if !entry.tracked || entry.reading.as_str() != reading {
            return None;
        }
        self.len -= 1;
        Some(std::mem::take(&mut self.entries[top]))
    }

    fn drop_pending(&mut self) {
        for entry in &mut self.entries[usize::from(self.standing)..usize::from(self.len)] {
            *entry = TrailEntry::default();
        }
        self.len = self.standing;
    }

    /// Appends the newest standing commit below the remaining pending ones,
    /// dropping the oldest entry (standing first, else the deepest pending)
    /// when the trail is full.
    fn push_standing(&mut self, entry: TrailEntry) {
        if usize::from(self.len) == MAX_TRAIL_ENTRIES {
            self.entries.rotate_left(1);
            self.entries[MAX_TRAIL_ENTRIES - 1] = TrailEntry::default();
            self.standing = self.standing.saturating_sub(1);
            self.len -= 1;
        }
        let len = usize::from(self.len);
        self.entries[len] = entry;
        self.entries[usize::from(self.standing)..=len].rotate_right(1);
        self.standing += 1;
        self.len += 1;
    }

    fn move_newest_standing_to_pending(&mut self) {
        let newest = usize::from(self.standing) - 1;
        // The newest standing commit is left of every pending one, so it
        // becomes the top of the stack: the next text replaces it first.
        self.entries[newest..usize::from(self.len)].rotate_left(1);
        self.standing -= 1;
    }
}

#[cfg(test)]
#[path = "correction_tests.rs"]
mod tests;
