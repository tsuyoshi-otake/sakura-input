//! The one interface the core writes text through.
//!
//! Everything the engine produces — kana from the romaji FSM, normalized
//! output from the width policy, conversion results — is written into a sink
//! supplied by the caller rather than returned as a `String`. That is what
//! makes the hot path allocation-free (DESIGN 5.7): the engine reuses one
//! preallocated buffer for the lifetime of a session, and only tests and
//! tools bother with a growable `String`.
//!
//! Sinks are fallible because a fixed-capacity buffer can fill up. Refusing a
//! write is always better than the alternatives — truncating silently loses
//! the user's text, and growing turns a bounded engine into an unbounded one.

use sakura_proto::{FixedStr, Overflow};

/// Somewhere UTF-8 text can be appended.
pub trait TextSink {
    /// Appends `s`.
    ///
    /// On overflow the sink is left exactly as it was: a partial write would
    /// leave the caller with text it never asked for and no way to tell how
    /// much of it landed.
    fn push_str(&mut self, s: &str) -> Result<(), Overflow>;

    /// Appends one character.
    fn push(&mut self, c: char) -> Result<(), Overflow> {
        let mut buf = [0u8; 4];
        self.push_str(c.encode_utf8(&mut buf))
    }
}

/// For tests, tools, and anywhere the allocation does not matter.
impl TextSink for String {
    fn push_str(&mut self, s: &str) -> Result<(), Overflow> {
        String::push_str(self, s);
        Ok(())
    }

    fn push(&mut self, c: char) -> Result<(), Overflow> {
        String::push(self, c);
        Ok(())
    }
}

/// The shipping sink: bounded, reusable, and never allocates.
impl<const N: usize> TextSink for FixedStr<N> {
    fn push_str(&mut self, s: &str) -> Result<(), Overflow> {
        FixedStr::push_str(self, s)
    }

    fn push(&mut self, c: char) -> Result<(), Overflow> {
        FixedStr::push(self, c)
    }
}

/// Forwards to the sink behind the reference, so `&mut dyn TextSink` and
/// `&mut impl TextSink` are both usable where a sink is expected.
impl<T: TextSink + ?Sized> TextSink for &mut T {
    fn push_str(&mut self, s: &str) -> Result<(), Overflow> {
        (**self).push_str(s)
    }

    fn push(&mut self, c: char) -> Result<(), Overflow> {
        (**self).push(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spelled out through the trait because `String`'s inherent `push_str`
    /// shadows the trait method and returns `()`.
    #[test]
    fn string_accepts_everything() {
        let mut sink = String::new();
        TextSink::push_str(&mut sink, "どっかー").expect("String never overflows");
        TextSink::push(&mut sink, '🍣').expect("String never overflows");
        assert_eq!(sink, "どっかー🍣");
    }

    #[test]
    fn fixed_sink_reports_overflow_without_partial_writes() {
        let mut sink = FixedStr::<8>::new();
        sink.push_str("abcdefgh").expect("exactly fits");
        assert_eq!(sink.push_str("i"), Err(Overflow));
        assert_eq!(sink.as_str(), "abcdefgh");
    }

    #[test]
    fn multibyte_characters_count_as_bytes_not_characters() {
        // 3 bytes each: two fit in 8 bytes, the third does not.
        let mut sink = FixedStr::<8>::new();
        sink.push('あ').expect("fits");
        sink.push('い').expect("fits");
        assert_eq!(sink.push('う'), Err(Overflow));
        assert_eq!(sink.as_str(), "あい");
    }

    #[test]
    fn mutable_references_forward_to_the_sink() {
        fn write_into(sink: &mut impl TextSink) -> Result<(), Overflow> {
            sink.push_str("ok")
        }

        let mut sink = FixedStr::<8>::new();
        write_into(&mut &mut sink).expect("forwards through the reference");
        assert_eq!(sink.as_str(), "ok");
    }
}
