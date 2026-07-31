//! Primitive, cursor-based binary reader/writer for the wire format.
//!
//! The protocol (DESIGN.md §7) is a hand-rolled fixed-layout binary codec:
//! no reflection, no codegen, explicit little-endian integers. This module
//! provides the small set of primitive read/write operations that every
//! higher-level type in [`crate::types`] and [`crate::message`] is built
//! from, plus the shared [`Error`] type returned by every decode path in
//! the crate.
//!
//! Writing is abstracted behind the [`Sink`] trait so the exact same
//! encoding logic serves two destinations: a growable [`VecSink`] (used by
//! [`crate::message::encode_request`]/`encode_response`, where reusing one
//! `Vec<u8>` per connection keeps steady-state allocation at zero) and a
//! fixed-capacity [`SliceSink`] over caller-owned memory (used by
//! [`crate::output::OutputBuf::encode_frame`], which must not allocate at
//! all).

use crate::fixed::Overflow;
use crate::MAX_STRING_BYTES;
use core::fmt;

/// Errors produced while decoding (or, for [`Error::Overflow`] /
/// [`Error::TooLarge`], while encoding into a fixed-size destination).
///
/// Every payload byte on the wire is untrusted input (DESIGN.md §7: "the
/// engine treats the pipe as a hostile boundary"), so every decode path in
/// this crate returns `Result<_, Error>` instead of panicking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The buffer ended before a value could be fully read.
    Truncated,
    /// Bytes remained after a complete message was decoded.
    TrailingBytes,
    /// A declared length exceeded a protocol limit (e.g. `MAX_PAYLOAD`,
    /// `MAX_STRING_BYTES`) or a fixed-capacity destination was too small.
    TooLarge,
    /// A string field was not valid UTF-8.
    BadUtf8,
    /// A `char` field's `u32` scalar value was not a valid Unicode scalar.
    BadChar,
    /// A strictly-decoded enum field carried an unrecognised value.
    BadEnum,
    /// A message type byte did not match any known request/response.
    BadMsgType(u16),
    /// An `Option<T>` tag byte was neither 0 nor 1.
    BadTag,
    /// The payload's protocol version did not match [`crate::PROTOCOL_VERSION`].
    UnsupportedVersion(u16),
    /// A fixed-capacity buffer ([`crate::fixed::FixedStr`],
    /// [`crate::fixed::FixedVec`], or a [`SliceSink`]) ran out of room.
    Overflow,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("buffer ended before value was fully read"),
            Error::TrailingBytes => f.write_str("trailing bytes after complete message"),
            Error::TooLarge => f.write_str("declared length exceeds protocol limit"),
            Error::BadUtf8 => f.write_str("string field was not valid UTF-8"),
            Error::BadChar => f.write_str("char field was not a valid Unicode scalar value"),
            Error::BadEnum => f.write_str("enum field carried an unrecognised value"),
            Error::BadMsgType(t) => write!(f, "unrecognised message type: 0x{t:04x}"),
            Error::BadTag => f.write_str("option tag byte was neither 0 nor 1"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported protocol version: {v}"),
            Error::Overflow => f.write_str("fixed-capacity destination ran out of room"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Overflow> for Error {
    fn from(_: Overflow) -> Self {
        Error::Overflow
    }
}

/// A cursor over an in-memory byte slice, reading little-endian primitives.
///
/// `Reader` never panics: every read that would run past the end of the
/// buffer returns `Err(Error::Truncated)` instead.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader positioned at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// Returns the number of unread bytes remaining.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < n {
            return Err(Error::Truncated);
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.buf[start..self.pos])
    }

    /// Reads one byte.
    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let b = self.take(1)?;
        Ok(b[0])
    }

    /// Reads a little-endian `u16`.
    pub fn read_u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Reads a little-endian `u32`.
    pub fn read_u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a little-endian `u64`.
    pub fn read_u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(b);
        Ok(u64::from_le_bytes(arr))
    }

    /// Reads a `bool` encoded as one byte: `0` is `false`, anything else is
    /// `true`.
    pub fn read_bool(&mut self) -> Result<bool, Error> {
        Ok(self.read_u8()? != 0)
    }

    /// Reads a `char` encoded as a little-endian `u32` Unicode scalar
    /// value. Fails with [`Error::BadChar`] on values that are not valid
    /// scalar values (e.g. lone surrogates) — the DLL converts UTF-16 at
    /// the TSF boundary, so this codec never has to guess at surrogates.
    pub fn read_char(&mut self) -> Result<char, Error> {
        let v = self.read_u32()?;
        char::from_u32(v).ok_or(Error::BadChar)
    }

    /// Reads a length-prefixed UTF-8 string: a `u16 LE` byte length
    /// (capped at [`MAX_STRING_BYTES`]) followed by that many bytes.
    pub fn read_str(&mut self) -> Result<&'a str, Error> {
        let len = self.read_u16()? as usize;
        if len > MAX_STRING_BYTES {
            return Err(Error::TooLarge);
        }
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes).map_err(|_| Error::BadUtf8)
    }

    /// Reads a length-prefixed sequence count (`u16 LE`).
    pub fn read_count(&mut self) -> Result<u16, Error> {
        self.read_u16()
    }

    /// Reads an `Option<T>` tag (`u8`: 0 = None, 1 = Some) and, if present,
    /// decodes the payload with `f`. Any tag other than 0/1 is
    /// [`Error::BadTag`].
    pub fn read_option<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            _ => Err(Error::BadTag),
        }
    }

    /// Consumes the reader, asserting no bytes remain. Used at the end of
    /// every top-level message decode to reject trailing bytes
    /// ([`Error::TrailingBytes`]) — a strict boundary that keeps fuzzing
    /// honest (a decoder that silently ignores extra bytes hides bugs).
    pub fn finish(self) -> Result<(), Error> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(Error::TrailingBytes)
        }
    }
}

/// A destination for encoded bytes.
///
/// [`VecSink`] and [`SliceSink`] both implement this; every other encode
/// helper in the crate is written once, generically, against `Sink`.
pub trait Sink {
    /// Appends `bytes` to the destination, or fails if there is no room.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error>;

    /// Writes one byte.
    fn write_u8(&mut self, v: u8) -> Result<(), Error> {
        self.write_bytes(&[v])
    }

    /// Writes a little-endian `u16`.
    fn write_u16(&mut self, v: u16) -> Result<(), Error> {
        self.write_bytes(&v.to_le_bytes())
    }

    /// Writes a little-endian `u32`.
    fn write_u32(&mut self, v: u32) -> Result<(), Error> {
        self.write_bytes(&v.to_le_bytes())
    }

    /// Writes a little-endian `u64`.
    fn write_u64(&mut self, v: u64) -> Result<(), Error> {
        self.write_bytes(&v.to_le_bytes())
    }

    /// Writes a `bool` as one byte (0 or 1).
    fn write_bool(&mut self, v: bool) -> Result<(), Error> {
        self.write_u8(u8::from(v))
    }

    /// Writes a `char` as a little-endian `u32` Unicode scalar value.
    fn write_char(&mut self, c: char) -> Result<(), Error> {
        self.write_u32(c as u32)
    }

    /// Writes a length-prefixed UTF-8 string: a `u16 LE` byte length
    /// followed by the bytes. Fails with [`Error::TooLarge`] if `s` is
    /// longer than [`MAX_STRING_BYTES`] or than `u16::MAX` bytes.
    fn write_str(&mut self, s: &str) -> Result<(), Error> {
        let bytes = s.as_bytes();
        if bytes.len() > MAX_STRING_BYTES || bytes.len() > u16::MAX as usize {
            return Err(Error::TooLarge);
        }
        self.write_u16(bytes.len() as u16)?;
        self.write_bytes(bytes)
    }

    /// Writes a sequence count (`u16 LE`). Fails with [`Error::TooLarge`]
    /// if `count` does not fit in a `u16`.
    fn write_count(&mut self, count: usize) -> Result<(), Error> {
        if count > u16::MAX as usize {
            return Err(Error::TooLarge);
        }
        self.write_u16(count as u16)
    }

    /// Writes an `Option<T>` as a tag byte (0/1) followed by the payload
    /// (via `f`) when present.
    fn write_option<T>(
        &mut self,
        opt: &Option<T>,
        f: impl FnOnce(&mut Self, &T) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match opt {
            None => self.write_u8(0),
            Some(v) => {
                self.write_u8(1)?;
                f(self, v)
            }
        }
    }
}

/// A [`Sink`] that appends to a growable `Vec<u8>`.
///
/// Used by [`crate::message::encode_request`] and `encode_response`. A
/// caller that owns one `Vec<u8>` per connection and reuses it across
/// calls (the buffer's capacity survives `Vec::clear`) pays no allocation
/// once the buffer has grown to its steady-state size.
#[derive(Debug)]
pub struct VecSink<'a> {
    dst: &'a mut Vec<u8>,
}

impl<'a> VecSink<'a> {
    /// Creates a sink appending to `dst`.
    pub fn new(dst: &'a mut Vec<u8>) -> Self {
        VecSink { dst }
    }
}

impl Sink for VecSink<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.dst.extend_from_slice(bytes);
        Ok(())
    }
}

/// A [`Sink`] that writes into a fixed-capacity, caller-owned `&mut [u8]`.
///
/// Used by [`crate::output::OutputBuf::encode_frame`], which must not
/// allocate at all (DESIGN.md §5.7). Writing past the end of the slice
/// fails with [`Error::Overflow`] instead of growing.
#[derive(Debug)]
pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceSink<'a> {
    /// Creates a sink writing into `buf`, starting at offset 0.
    pub fn new(buf: &'a mut [u8]) -> Self {
        SliceSink { buf, pos: 0 }
    }

    /// Returns the number of bytes written so far.
    pub fn len(&self) -> usize {
        self.pos
    }

    /// Returns `true` if nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.pos == 0
    }
}

impl Sink for SliceSink<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.pos.checked_add(bytes.len()).ok_or(Error::Overflow)?;
        if end > self.buf.len() {
            return Err(Error::Overflow);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_reads_le_primitives() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u8.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&4u64.to_le_bytes());
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u8(), Ok(1));
        assert_eq!(r.read_u16(), Ok(2));
        assert_eq!(r.read_u32(), Ok(3));
        assert_eq!(r.read_u64(), Ok(4));
        assert_eq!(r.finish(), Ok(()));
    }

    #[test]
    fn reader_truncated_never_panics() {
        let buf = [0u8; 1];
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u32(), Err(Error::Truncated));
    }

    #[test]
    fn reader_trailing_bytes() {
        let buf = [0u8; 4];
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_u8(), Ok(0));
        assert_eq!(r.finish(), Err(Error::TrailingBytes));
    }

    #[test]
    fn reader_bad_char() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0xD800u32.to_le_bytes()); // lone surrogate
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_char(), Err(Error::BadChar));
    }

    #[test]
    fn reader_bad_utf8() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&[0xFF, 0xFE]);
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_str(), Err(Error::BadUtf8));
    }

    #[test]
    fn reader_string_too_large() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&((MAX_STRING_BYTES + 1) as u16).to_le_bytes());
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_str(), Err(Error::TooLarge));
    }

    #[test]
    fn reader_option_bad_tag() {
        let buf = [2u8];
        let mut r = Reader::new(&buf);
        let result: Result<Option<u8>, Error> = r.read_option(Reader::read_u8);
        assert_eq!(result, Err(Error::BadTag));
    }

    #[test]
    fn vec_sink_roundtrips_with_reader() {
        let mut dst = Vec::new();
        {
            let mut w = VecSink::new(&mut dst);
            w.write_u8(7).expect("write");
            w.write_str("あいう").expect("write");
            w.write_bool(true).expect("write");
        }
        let mut r = Reader::new(&dst);
        assert_eq!(r.read_u8(), Ok(7));
        assert_eq!(r.read_str(), Ok("あいう"));
        assert_eq!(r.read_bool(), Ok(true));
        assert_eq!(r.finish(), Ok(()));
    }

    #[test]
    fn slice_sink_overflow_is_reported() {
        let mut buf = [0u8; 2];
        let mut w = SliceSink::new(&mut buf);
        assert_eq!(w.write_u8(1), Ok(()));
        assert_eq!(w.write_u16(2), Err(Error::Overflow));
        // First write is still intact.
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn error_display_and_from_overflow() {
        assert_eq!(
            Error::from(Overflow).to_string(),
            Error::Overflow.to_string()
        );
        assert_eq!(
            Error::BadMsgType(0x1234).to_string(),
            "unrecognised message type: 0x1234"
        );
    }
}
