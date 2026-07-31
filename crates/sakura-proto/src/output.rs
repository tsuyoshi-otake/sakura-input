//! `OutputBuf`: a fixed-capacity, allocation-free builder for
//! `Response::Output` frames.
//!
//! The engine's hot path (kana input, conversion, prediction hand-off)
//! must not allocate (DESIGN.md §5.7, §10). Building a `types::Output`
//! the normal way allocates a `Vec<Segment>` and one `String` per segment
//! plus one for the commit text; `OutputBuf` instead accumulates preedit
//! and commit text into [`FixedStr`] buffers and segment spans into a
//! [`FixedVec`], then encodes straight to bytes with [`OutputBuf::encode_frame`]
//! — no intermediate owned `Output` is built on that path at all.
//! [`OutputBuf::to_output`] is provided for tests and the DLL side, where
//! an owned `Output` is convenient and allocation is not a concern.

use crate::fixed::{FixedStr, FixedVec, Overflow};
use crate::message::RES_OUTPUT;
use crate::types::{Mode, Output, Preedit, Segment, UnderlineKind};
use crate::wire::{Error, Sink, SliceSink};
use crate::{
    RequestId, FRAME_HEADER_LEN, MAX_COMMIT_BYTES, MAX_PAYLOAD, MAX_PREEDIT_BYTES, MAX_SEGMENTS,
    PROTOCOL_VERSION,
};

/// One segment's span within `OutputBuf`'s flat preedit text buffer.
///
/// `start`/`len` are **byte** offsets/lengths into
/// [`OutputBuf::preedit_text`] — segments are always pushed as whole
/// `&str` values, so a span's boundaries are always UTF-8 char
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegSpan {
    pub start: u32,
    pub len: u32,
    pub underline: UnderlineKind,
}

/// A fixed-capacity, allocation-free builder for a `Response::Output`
/// frame.
///
/// All text lives in [`FixedStr`] buffers and all segment spans in a
/// [`FixedVec`]; every field is stack-sized, so [`OutputBuf::new`] and
/// every mutator run without touching the heap.
#[derive(Clone)]
pub struct OutputBuf {
    pub consumed: bool,
    pub beep: bool,
    pub mode: Option<Mode>,
    has_preedit: bool,
    preedit: FixedStr<MAX_PREEDIT_BYTES>,
    segments: FixedVec<SegSpan, MAX_SEGMENTS>,
    cursor: u32,
    has_commit: bool,
    commit: FixedStr<MAX_COMMIT_BYTES>,
}

impl OutputBuf {
    /// Creates an empty `OutputBuf`. Does not allocate.
    pub fn new() -> Self {
        OutputBuf {
            consumed: false,
            beep: false,
            mode: None,
            has_preedit: false,
            preedit: FixedStr::new(),
            segments: FixedVec::new(),
            cursor: 0,
            has_commit: false,
            commit: FixedStr::new(),
        }
    }

    /// Resets every field to its default, ready for reuse. Does not
    /// allocate (and releases no capacity — the underlying buffers keep
    /// their fixed size).
    pub fn clear(&mut self) {
        self.consumed = false;
        self.beep = false;
        self.mode = None;
        self.has_preedit = false;
        self.preedit.clear();
        self.segments.clear();
        self.cursor = 0;
        self.has_commit = false;
        self.commit.clear();
    }

    /// Starts (or restarts) a preedit composition: marks a preedit as
    /// present and clears any previously accumulated segment text/spans
    /// and cursor.
    pub fn begin_preedit(&mut self) {
        self.has_preedit = true;
        self.preedit.clear();
        self.segments.clear();
        self.cursor = 0;
    }

    /// Appends one segment of preedit text.
    ///
    /// Atomic on overflow: if `text` would not fit in the remaining
    /// preedit capacity, or the segment table is already full, neither
    /// the preedit text nor the segment table is changed.
    pub fn push_segment(&mut self, text: &str, underline: UnderlineKind) -> Result<(), Overflow> {
        if self.segments.len() >= self.segments.capacity() {
            return Err(Overflow);
        }
        let start = self.preedit.len();
        let new_len = match start.checked_add(text.len()) {
            Some(n) if n <= self.preedit.capacity() => n,
            _ => return Err(Overflow),
        };
        // Capacity for both buffers was checked above, so these cannot
        // fail; `?` still routes any surprise failure through cleanly
        // instead of unwrapping.
        self.preedit.push_str(text)?;
        let span = SegSpan {
            start: start as u32,
            len: (new_len - start) as u32,
            underline,
        };
        self.segments.push(span)?;
        Ok(())
    }

    /// Sets the preedit cursor, a **character** offset into the
    /// concatenation of all segment texts (matching
    /// [`crate::types::Preedit::cursor`]).
    pub fn set_cursor(&mut self, chars: u32) {
        self.cursor = chars;
    }

    /// Sets the commit text, replacing any previous value.
    ///
    /// Atomic on overflow: if `text` does not fit, the previous commit
    /// text (if any) is left unchanged.
    pub fn set_commit(&mut self, text: &str) -> Result<(), Overflow> {
        if text.len() > self.commit.capacity() {
            return Err(Overflow);
        }
        self.commit.clear();
        self.commit.push_str(text)?;
        self.has_commit = true;
        Ok(())
    }

    /// Returns the accumulated preedit text (the concatenation of every
    /// pushed segment).
    pub fn preedit_text(&self) -> &str {
        self.preedit.as_str()
    }

    /// Returns the commit text, if `set_commit` has been called since the
    /// last `clear`.
    pub fn commit_text(&self) -> Option<&str> {
        if self.has_commit {
            Some(self.commit.as_str())
        } else {
            None
        }
    }

    /// Returns the pushed segment spans, in push order.
    pub fn segments(&self) -> &[SegSpan] {
        self.segments.as_slice()
    }

    fn segment_text(&self, span: SegSpan) -> &str {
        let start = span.start as usize;
        let end = start + span.len as usize;
        // `start`/`end` were produced by `push_segment` as byte offsets
        // into `self.preedit` at whole-`&str`-append boundaries, so this
        // slice is always within bounds and on a char boundary.
        self.preedit.as_str().get(start..end).unwrap_or("")
    }

    fn encode_body<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_bool(self.consumed)?;
        w.write_bool(self.beep)?;
        w.write_option(&self.mode, |w, m| m.encode(w))?;
        if self.has_preedit {
            w.write_u8(1)?;
            w.write_count(self.segments.len())?;
            for span in self.segments.as_slice() {
                w.write_str(self.segment_text(*span))?;
                span.underline.encode(w)?;
            }
            w.write_u32(self.cursor)?;
        } else {
            w.write_u8(0)?;
        }
        if self.has_commit {
            w.write_u8(1)?;
            w.write_str(self.commit.as_str())?;
        } else {
            w.write_u8(0)?;
        }
        Ok(())
    }

    /// Encodes a complete `Response::Output` frame (4-byte length prefix
    /// included) into `dst`, without allocating.
    ///
    /// Returns the number of bytes written. Fails with [`Error::Overflow`]
    /// if `dst` is too small to hold the frame.
    pub fn encode_frame(&self, id: RequestId, dst: &mut [u8]) -> Result<usize, Error> {
        if dst.len() < FRAME_HEADER_LEN {
            return Err(Error::Overflow);
        }
        let body_len = {
            let mut w = SliceSink::new(&mut dst[FRAME_HEADER_LEN..]);
            w.write_u16(PROTOCOL_VERSION)?;
            w.write_u64(id)?;
            w.write_u16(RES_OUTPUT)?;
            self.encode_body(&mut w)?;
            w.len()
        };
        if body_len > MAX_PAYLOAD {
            return Err(Error::TooLarge);
        }
        let len_bytes = (body_len as u32).to_le_bytes();
        dst[..FRAME_HEADER_LEN].copy_from_slice(&len_bytes);
        Ok(FRAME_HEADER_LEN + body_len)
    }

    /// Builds an owned [`Output`] equivalent to this buffer's contents.
    ///
    /// This allocates (a `Vec<Segment>` plus one `String` per segment and
    /// one for the commit text) and is meant for tests and the DLL side,
    /// not the engine's hot path.
    pub fn to_output(&self) -> Output {
        let preedit = if self.has_preedit {
            let mut segments = Vec::with_capacity(self.segments.len());
            for span in self.segments.as_slice() {
                segments.push(Segment {
                    text: self.segment_text(*span).to_string(),
                    underline: span.underline,
                });
            }
            Some(Preedit {
                segments,
                cursor: self.cursor,
            })
        } else {
            None
        };
        let commit = self.commit_text().map(|s| s.to_string());
        Output {
            consumed: self.consumed,
            beep: self.beep,
            mode: self.mode,
            preedit,
            commit,
        }
    }
}

impl Default for OutputBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for OutputBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OutputBuf")
            .field("consumed", &self.consumed)
            .field("beep", &self.beep)
            .field("mode", &self.mode)
            .field("preedit_text", &self.preedit_text())
            .field("segments", &self.segments())
            .field("cursor", &self.cursor)
            .field("commit_text", &self.commit_text())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::decode_response;
    use crate::message::Response;

    #[test]
    fn new_is_empty() {
        let buf = OutputBuf::new();
        assert!(!buf.consumed);
        assert!(!buf.beep);
        assert_eq!(buf.mode, None);
        assert_eq!(buf.preedit_text(), "");
        assert_eq!(buf.commit_text(), None);
        assert!(buf.segments().is_empty());
    }

    #[test]
    fn push_segment_accumulates_text_and_spans() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        buf.push_segment("かん", UnderlineKind::Raw).expect("push");
        buf.push_segment("字", UnderlineKind::Converted)
            .expect("push");
        buf.set_cursor(3);
        assert_eq!(buf.preedit_text(), "かん字");
        assert_eq!(buf.segments().len(), 2);
        assert_eq!(buf.segments()[0].underline, UnderlineKind::Raw);
        assert_eq!(buf.segments()[1].underline, UnderlineKind::Converted);
    }

    #[test]
    fn push_segment_overflow_is_atomic() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        let long = "a".repeat(MAX_PREEDIT_BYTES);
        buf.push_segment(&long, UnderlineKind::Raw)
            .expect("fits exactly");
        let before = buf.preedit_text().len();
        let result = buf.push_segment("x", UnderlineKind::Raw);
        assert_eq!(result, Err(Overflow));
        assert_eq!(buf.preedit_text().len(), before);
        assert_eq!(buf.segments().len(), 1);
    }

    #[test]
    fn push_segment_rejects_beyond_max_segments() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        for _ in 0..MAX_SEGMENTS {
            buf.push_segment("a", UnderlineKind::Raw).expect("push");
        }
        assert_eq!(buf.push_segment("a", UnderlineKind::Raw), Err(Overflow));
        assert_eq!(buf.segments().len(), MAX_SEGMENTS);
    }

    #[test]
    fn set_commit_overflow_is_atomic() {
        let mut buf = OutputBuf::new();
        buf.set_commit("hello").expect("fits");
        let too_long = "a".repeat(MAX_COMMIT_BYTES + 1);
        assert_eq!(buf.set_commit(&too_long), Err(Overflow));
        // Previous commit text is unchanged.
        assert_eq!(buf.commit_text(), Some("hello"));
    }

    #[test]
    fn clear_resets_everything() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.beep = true;
        buf.mode = Some(Mode::Hiragana);
        buf.begin_preedit();
        buf.push_segment("a", UnderlineKind::Raw).expect("push");
        buf.set_commit("b").expect("push");
        buf.clear();
        assert!(!buf.consumed);
        assert!(!buf.beep);
        assert_eq!(buf.mode, None);
        assert_eq!(buf.preedit_text(), "");
        assert_eq!(buf.commit_text(), None);
        assert!(buf.segments().is_empty());
    }

    #[test]
    fn encode_frame_matches_to_output_through_decode() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.mode = Some(Mode::Katakana);
        buf.begin_preedit();
        buf.push_segment("ドッカー", UnderlineKind::Converted)
            .expect("push");
        buf.set_cursor(4);
        buf.set_commit("🍣").expect("push");

        let mut frame = [0u8; 256];
        let n = buf.encode_frame(99, &mut frame).expect("encode");
        let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode");
        assert_eq!(id, 99);
        assert_eq!(response, Response::Output(buf.to_output()));
    }

    #[test]
    fn encode_frame_reports_overflow_for_small_dst() {
        let buf = OutputBuf::new();
        let mut tiny = [0u8; 2];
        assert_eq!(buf.encode_frame(1, &mut tiny), Err(Error::Overflow));
    }

    #[test]
    fn default_matches_new() {
        let a = OutputBuf::default();
        let b = OutputBuf::new();
        assert_eq!(a.to_output(), b.to_output());
    }

    #[test]
    fn debug_impl_does_not_panic() {
        let buf = OutputBuf::new();
        let s = format!("{buf:?}");
        assert!(s.contains("OutputBuf"));
    }
}
