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
    /// Caps Lock / 英数 mode key.
    CapsLock = 20,
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
    /// All `KeyCode` variants, in declaration order. Used by tests that
    /// need to exercise every value.
    pub const ALL: [KeyCode; 33] = [
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
        KeyCode::CapsLock,
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
}
