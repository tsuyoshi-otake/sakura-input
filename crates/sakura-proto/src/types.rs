//! Domain value types shared by both directions of the protocol.
//!
//! These are the pieces `Request`/`Response` (see [`crate::message`]) are
//! built from: key events, editor mode, preedit/segment shapes, and error
//! codes. Two decode disciplines are used deliberately (DESIGN.md §7):
//!
//! - [`KeyCode`] decodes **leniently** — an unrecognised value becomes
//!   [`KeyCode::Unknown`], because keyboards and layouts vary and the
//!   engine simply won't consume a code it doesn't recognise.
//! - [`Mode`], [`InputScope`], [`UnderlineKind`], and [`ErrorCode`] decode
//!   **strictly** — an unrecognised value is [`Error::BadEnum`]. Protocol
//!   evolution for these is handled by the version negotiation in
//!   [`crate::message::Header`], not by silently guessing a value.

use crate::wire::{Error, Reader, Sink};

/// A logical key, independent of the host platform's virtual-key codes.
///
/// The DLL maps Win32 virtual keys to `KeyCode` at the TSF boundary so the
/// engine and every other consumer of this protocol never see a
/// platform-specific key constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum KeyCode {
    /// A key the sender did not map to a known `KeyCode`. Also the decode
    /// fallback for any value this enum does not list.
    Unknown = 0,
    /// A printable character key; the character itself travels in
    /// [`KeyInput::ch`].
    Char = 1,
    Space = 2,
    Enter = 3,
    Escape = 4,
    Backspace = 5,
    Delete = 6,
    Tab = 7,
    Left = 8,
    Right = 9,
    Up = 10,
    Down = 11,
    Home = 12,
    End = 13,
    PageUp = 14,
    PageDown = 15,
    /// IME conversion key (変換).
    Henkan = 16,
    /// IME non-conversion key (無変換).
    Muhenkan = 17,
    /// Kana lock / kana-mode toggle key.
    KanaMode = 18,
    /// 半角/全角 toggle key.
    HankakuZenkaku = 19,
    F1 = 32,
    F2 = 33,
    F3 = 34,
    F4 = 35,
    F5 = 36,
    F6 = 37,
    F7 = 38,
    F8 = 39,
    F9 = 40,
    F10 = 41,
    F11 = 42,
    F12 = 43,
}

impl KeyCode {
    /// Encodes as a little-endian `u16`.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u16(self as u16)
    }

    /// Decodes a `u16`, mapping any unrecognised value to
    /// [`KeyCode::Unknown`] rather than failing (see module docs).
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let v = r.read_u16()?;
        Ok(Self::from_u16(v))
    }

    /// Maps a raw wire value to a `KeyCode`, defaulting to `Unknown`.
    fn from_u16(v: u16) -> Self {
        use KeyCode::*;
        match v {
            0 => Unknown,
            1 => Char,
            2 => Space,
            3 => Enter,
            4 => Escape,
            5 => Backspace,
            6 => Delete,
            7 => Tab,
            8 => Left,
            9 => Right,
            10 => Up,
            11 => Down,
            12 => Home,
            13 => End,
            14 => PageUp,
            15 => PageDown,
            16 => Henkan,
            17 => Muhenkan,
            18 => KanaMode,
            19 => HankakuZenkaku,
            32 => F1,
            33 => F2,
            34 => F3,
            35 => F4,
            36 => F5,
            37 => F6,
            38 => F7,
            39 => F8,
            40 => F9,
            41 => F10,
            42 => F11,
            43 => F12,
            _ => Unknown,
        }
    }

    /// All `KeyCode` variants, in declaration order. Used by tests that
    /// need to exercise every value.
    pub const ALL: [KeyCode; 32] = [
        KeyCode::Unknown,
        KeyCode::Char,
        KeyCode::Space,
        KeyCode::Enter,
        KeyCode::Escape,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Tab,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Henkan,
        KeyCode::Muhenkan,
        KeyCode::KanaMode,
        KeyCode::HankakuZenkaku,
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
    ];
}

/// A bitmask of held modifier keys.
///
/// Deliberately a thin `u8` wrapper rather than a `bitflags`-generated
/// type (this crate has zero external dependencies); the bit layout is
/// part of the wire format, so it is specified explicitly here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(pub u8);

impl Modifiers {
    /// No modifiers held.
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: Modifiers = Modifiers(0x01);
    pub const CTRL: Modifiers = Modifiers(0x02);
    pub const ALT: Modifiers = Modifiers(0x04);
    pub const CAPS_LOCK: Modifiers = Modifiers(0x08);
    pub const KANA_LOCK: Modifiers = Modifiers(0x10);

    /// Returns `true` if the Shift bit is set.
    pub fn shift(self) -> bool {
        self.contains(Self::SHIFT)
    }

    /// Returns `true` if the Ctrl bit is set.
    pub fn ctrl(self) -> bool {
        self.contains(Self::CTRL)
    }

    /// Returns `true` if the Alt bit is set.
    pub fn alt(self) -> bool {
        self.contains(Self::ALT)
    }

    /// Returns `true` if the Caps Lock bit is set.
    pub fn caps_lock(self) -> bool {
        self.contains(Self::CAPS_LOCK)
    }

    /// Returns `true` if the Kana Lock bit is set.
    pub fn kana_lock(self) -> bool {
        self.contains(Self::KANA_LOCK)
    }

    /// Returns `true` if every bit set in `other` is also set in `self`.
    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns a copy with the lock bits (Caps Lock, Kana Lock) cleared,
    /// leaving only the "held while typing" modifiers (Shift/Ctrl/Alt).
    pub fn without_locks(self) -> Modifiers {
        Modifiers(self.0 & !(Self::CAPS_LOCK.0 | Self::KANA_LOCK.0))
    }

    /// Encodes as one byte.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self.0)
    }

    /// Decodes one byte. Every `u8` value is a valid (if unusual) bitmask,
    /// so this never fails.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Modifiers(r.read_u8()?))
    }
}

/// A single key event delivered to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub code: KeyCode,
    /// The character the active keyboard layout would produce, if any.
    pub ch: Option<char>,
    pub modifiers: Modifiers,
    /// `true` if this event is an auto-repeat of a held key.
    pub repeat: bool,
    /// `true` for `ITfKeyEventSink::OnTestKeyDown`: the engine answers
    /// "would I consume this?" without mutating session state.
    pub test_only: bool,
}

impl KeyInput {
    /// Encodes all fields in declaration order.
    pub fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        self.code.encode(w)?;
        w.write_option(&self.ch, |w, c| w.write_char(*c))?;
        self.modifiers.encode(w)?;
        w.write_bool(self.repeat)?;
        w.write_bool(self.test_only)
    }

    /// Decodes all fields in declaration order.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let code = KeyCode::decode(r)?;
        let ch = r.read_option(Reader::read_char)?;
        let modifiers = Modifiers::decode(r)?;
        let repeat = r.read_bool()?;
        let test_only = r.read_bool()?;
        Ok(KeyInput {
            code,
            ch,
            modifiers,
            repeat,
            test_only,
        })
    }
}

/// The IME's current input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Passthrough: keys are not intercepted.
    Direct = 0,
    Hiragana = 1,
    Katakana = 2,
    /// Half-width katakana.
    HalfKatakana = 3,
    /// Full-width alphanumeric.
    FullAlnum = 4,
    /// Half-width alphanumeric (same as Direct's character set, but still
    /// an IME-owned mode so mode-change UI has something to show).
    HalfAlnum = 5,
}

impl Mode {
    /// All `Mode` variants, in declaration order.
    pub const ALL: [Mode; 6] = [
        Mode::Direct,
        Mode::Hiragana,
        Mode::Katakana,
        Mode::HalfKatakana,
        Mode::FullAlnum,
        Mode::HalfAlnum,
    ];

    /// Encodes as one byte.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`] (see module docs).
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(Mode::Direct),
            1 => Ok(Mode::Hiragana),
            2 => Ok(Mode::Katakana),
            3 => Ok(Mode::HalfKatakana),
            4 => Ok(Mode::FullAlnum),
            5 => Ok(Mode::HalfAlnum),
            _ => Err(Error::BadEnum),
        }
    }
}

/// The input scope of the focused text field (DESIGN.md §9): password and
/// similar sensitive scopes disable learning and the commit-cache history
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InputScope {
    Normal = 0,
    Password = 1,
    Url = 2,
    Email = 3,
    Digits = 4,
}

impl InputScope {
    /// All `InputScope` variants, in declaration order.
    pub const ALL: [InputScope; 5] = [
        InputScope::Normal,
        InputScope::Password,
        InputScope::Url,
        InputScope::Email,
        InputScope::Digits,
    ];

    /// Encodes as one byte.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`] (see module docs).
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(InputScope::Normal),
            1 => Ok(InputScope::Password),
            2 => Ok(InputScope::Url),
            3 => Ok(InputScope::Email),
            4 => Ok(InputScope::Digits),
            _ => Err(Error::BadEnum),
        }
    }
}

/// How a preedit segment should be underlined by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum UnderlineKind {
    /// Unconverted reading text (thin underline).
    #[default]
    Raw = 0,
    /// Converted, non-focused segment (thick underline).
    Converted = 1,
    /// The segment currently being edited/resized (highlighted).
    Focused = 2,
}

impl UnderlineKind {
    /// All `UnderlineKind` variants, in declaration order.
    pub const ALL: [UnderlineKind; 3] = [
        UnderlineKind::Raw,
        UnderlineKind::Converted,
        UnderlineKind::Focused,
    ];

    /// Encodes as one byte.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`] (see module docs).
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(UnderlineKind::Raw),
            1 => Ok(UnderlineKind::Converted),
            2 => Ok(UnderlineKind::Focused),
            _ => Err(Error::BadEnum),
        }
    }
}

/// One run of preedit text and how it should be underlined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub underline: UnderlineKind,
}

impl Segment {
    /// Encodes the text then the underline kind.
    pub fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_str(&self.text)?;
        self.underline.encode(w)
    }

    /// Decodes the text then the underline kind.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let text = r.read_str()?.to_string();
        let underline = UnderlineKind::decode(r)?;
        Ok(Segment { text, underline })
    }
}

/// The full composition string: an ordered list of segments plus a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    pub segments: Vec<Segment>,
    /// A **character** offset (not byte offset) into the concatenation of
    /// all segment texts.
    pub cursor: u32,
}

impl Preedit {
    /// Encodes the segment count, then each segment, then the cursor.
    pub fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_count(self.segments.len())?;
        for seg in &self.segments {
            seg.encode(w)?;
        }
        w.write_u32(self.cursor)
    }

    /// Decodes the segment count, then each segment, then the cursor.
    /// Rejects more than [`crate::MAX_SEGMENTS`] segments with
    /// [`Error::TooLarge`].
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let count = r.read_count()? as usize;
        if count > crate::MAX_SEGMENTS {
            return Err(Error::TooLarge);
        }
        let mut segments = Vec::with_capacity(count);
        for _ in 0..count {
            segments.push(Segment::decode(r)?);
        }
        let cursor = r.read_u32()?;
        Ok(Preedit { segments, cursor })
    }
}

/// The engine's response to a key event, conversion command, or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// `true` if the engine consumed the key (the host must not process it
    /// further).
    pub consumed: bool,
    /// `true` if the engine wants an audible/visual "invalid input" beep.
    pub beep: bool,
    /// Present when the mode changed as a result of this event.
    pub mode: Option<Mode>,
    /// Present when there is an active composition to display.
    pub preedit: Option<Preedit>,
    /// Present when text was committed to the host application.
    pub commit: Option<String>,
}

impl Output {
    /// Encodes all fields in declaration order.
    pub fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_bool(self.consumed)?;
        w.write_bool(self.beep)?;
        w.write_option(&self.mode, |w, m| m.encode(w))?;
        w.write_option(&self.preedit, |w, p| p.encode(w))?;
        w.write_option(&self.commit, |w, c| w.write_str(c))
    }

    /// Decodes all fields in declaration order.
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let consumed = r.read_bool()?;
        let beep = r.read_bool()?;
        let mode = r.read_option(Mode::decode)?;
        let preedit = r.read_option(Preedit::decode)?;
        let commit = r.read_option(|r| Ok(r.read_str()?.to_string()))?;
        Ok(Output {
            consumed,
            beep,
            mode,
            preedit,
            commit,
        })
    }
}

/// A machine-readable reason a request could not be fulfilled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    /// The request's protocol version is not supported by this engine.
    UnsupportedVersion = 1,
    /// `session` did not name a live session (expired, deleted, or never
    /// created).
    UnknownSession = 2,
    /// The request payload did not decode.
    Malformed = 3,
    /// The engine is temporarily unable to service the request.
    Busy = 4,
    /// An internal engine error occurred.
    Internal = 5,
    /// The request or a field within it exceeded a size limit.
    TooLarge = 6,
}

impl ErrorCode {
    /// All `ErrorCode` variants, in declaration order.
    pub const ALL: [ErrorCode; 6] = [
        ErrorCode::UnsupportedVersion,
        ErrorCode::UnknownSession,
        ErrorCode::Malformed,
        ErrorCode::Busy,
        ErrorCode::Internal,
        ErrorCode::TooLarge,
    ];

    /// Encodes as a little-endian `u16`.
    pub fn encode<S: Sink>(self, w: &mut S) -> Result<(), Error> {
        w.write_u16(self as u16)
    }

    /// Decodes a `u16` strictly: an unrecognised value is
    /// [`Error::BadEnum`].
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u16()? {
            1 => Ok(ErrorCode::UnsupportedVersion),
            2 => Ok(ErrorCode::UnknownSession),
            3 => Ok(ErrorCode::Malformed),
            4 => Ok(ErrorCode::Busy),
            5 => Ok(ErrorCode::Internal),
            6 => Ok(ErrorCode::TooLarge),
            _ => Err(Error::BadEnum),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::VecSink;

    #[test]
    fn key_code_decode_is_lenient() {
        let mut buf = Vec::new();
        VecSink::new(&mut buf).write_u16(999).expect("write");
        let mut r = Reader::new(&buf);
        assert_eq!(KeyCode::decode(&mut r), Ok(KeyCode::Unknown));
    }

    #[test]
    fn mode_decode_is_strict() {
        let mut buf = Vec::new();
        VecSink::new(&mut buf).write_u8(200).expect("write");
        let mut r = Reader::new(&buf);
        assert_eq!(Mode::decode(&mut r), Err(Error::BadEnum));
    }

    #[test]
    fn input_scope_decode_is_strict() {
        let mut buf = Vec::new();
        VecSink::new(&mut buf).write_u8(200).expect("write");
        let mut r = Reader::new(&buf);
        assert_eq!(InputScope::decode(&mut r), Err(Error::BadEnum));
    }

    #[test]
    fn underline_kind_decode_is_strict_and_defaults_to_raw() {
        let mut buf = Vec::new();
        VecSink::new(&mut buf).write_u8(200).expect("write");
        let mut r = Reader::new(&buf);
        assert_eq!(UnderlineKind::decode(&mut r), Err(Error::BadEnum));
        assert_eq!(UnderlineKind::default(), UnderlineKind::Raw);
    }

    #[test]
    fn error_code_decode_is_strict() {
        let mut buf = Vec::new();
        VecSink::new(&mut buf).write_u16(999).expect("write");
        let mut r = Reader::new(&buf);
        assert_eq!(ErrorCode::decode(&mut r), Err(Error::BadEnum));
    }

    #[test]
    fn modifiers_helpers() {
        let m = Modifiers(Modifiers::SHIFT.0 | Modifiers::CAPS_LOCK.0);
        assert!(m.shift());
        assert!(!m.ctrl());
        assert!(m.caps_lock());
        assert!(m.contains(Modifiers::SHIFT));
        assert!(!m.contains(Modifiers::CTRL));
        let cleared = m.without_locks();
        assert!(cleared.shift());
        assert!(!cleared.caps_lock());
        assert!(!cleared.kana_lock());
    }

    #[test]
    fn preedit_decode_rejects_too_many_segments() {
        let mut buf = Vec::new();
        {
            let mut w = VecSink::new(&mut buf);
            w.write_count(crate::MAX_SEGMENTS + 1).expect("write");
        }
        let mut r = Reader::new(&buf);
        assert_eq!(Preedit::decode(&mut r), Err(Error::TooLarge));
    }

    #[test]
    fn key_input_roundtrip() {
        let input = KeyInput {
            code: KeyCode::Char,
            ch: Some('あ'),
            modifiers: Modifiers::SHIFT,
            repeat: true,
            test_only: false,
        };
        let mut buf = Vec::new();
        input.encode(&mut VecSink::new(&mut buf)).expect("encode");
        let mut r = Reader::new(&buf);
        let decoded = KeyInput::decode(&mut r).expect("decode");
        r.finish().expect("no trailing bytes");
        assert_eq!(decoded, input);
    }
}
