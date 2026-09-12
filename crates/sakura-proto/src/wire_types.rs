use sakura_values::{
    AiTextOperation, AiTextStatus, AppearanceTheme, InputScope, KeyCode, KeyInput, Mode, Modifiers,
    PadShortcut,
};

use crate::wire::{Error, Reader, Sink};

pub trait Wire: Sized {
    fn encode<S: Sink>(&self, sink: &mut S) -> Result<(), Error>;
    fn decode(reader: &mut Reader<'_>) -> Result<Self, Error>;
}

impl Wire for KeyCode {
    /// Encodes as a little-endian `u16`.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u16(*self as u16)
    }

    /// Decodes a `u16`, mapping any unrecognised value to
    /// [`KeyCode::Unknown`] rather than failing.
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let v = r.read_u16()?;
        Ok(key_code_from_u16(v))
    }
}

/// Maps a raw wire value to a `KeyCode`, defaulting to `Unknown`.
fn key_code_from_u16(v: u16) -> KeyCode {
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
        20 => CapsLock,
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

impl Wire for Modifiers {
    /// Encodes as one byte.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(self.0)
    }

    /// Decodes one byte. Every `u8` value is a valid (if unusual) bitmask,
    /// so this never fails.
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Modifiers(r.read_u8()?))
    }
}

impl Wire for KeyInput {
    /// Encodes all fields in declaration order.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        self.code.encode(w)?;
        w.write_option(&self.ch, |w, c| w.write_char(*c))?;
        self.modifiers.encode(w)?;
        w.write_bool(self.repeat)?;
        w.write_bool(self.test_only)
    }

    /// Decodes all fields in declaration order.
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
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

impl Wire for Mode {
    /// Encodes as one byte.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`].
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
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

impl Wire for AppearanceTheme {
    /// Encodes as one byte.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`], never a guessed palette.
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Light),
            2 => Ok(Self::Dark),
            _ => Err(Error::BadEnum),
        }
    }
}

impl Wire for PadShortcut {
    /// Encodes as one byte.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    /// Decodes one byte strictly. Protocol version negotiation, rather than
    /// an implicit fallback, handles future wire values.
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(Self::Disabled),
            1 => Ok(Self::DoubleCtrl),
            _ => Err(Error::BadEnum),
        }
    }
}

impl Wire for InputScope {
    /// Encodes as one byte.
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    /// Decodes one byte strictly: an unrecognised value is
    /// [`Error::BadEnum`].
    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            0 => Ok(InputScope::Normal),
            1 => Ok(InputScope::Password),
            2 => Ok(InputScope::Url),
            3 => Ok(InputScope::Email),
            4 => Ok(InputScope::Digits),
            5 => Ok(InputScope::Unclassified),
            _ => Err(Error::BadEnum),
        }
    }
}

impl Wire for AiTextOperation {
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            1 => Ok(Self::Transform),
            2 => Ok(Self::Proofread),
            _ => Err(Error::BadEnum),
        }
    }
}

impl Wire for AiTextStatus {
    fn encode<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        w.write_u8(*self as u8)
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        match r.read_u8()? {
            1 => Ok(Self::Applied),
            2 => Ok(Self::Cancelled),
            3 => Ok(Self::Timeout),
            4 => Ok(Self::MissingKey),
            5 => Ok(Self::WorkerError),
            6 => Ok(Self::ApiError),
            7 => Ok(Self::Rejected),
            _ => Err(Error::BadEnum),
        }
    }
}
