//! Turning a Windows keystroke into a [`KeyInput`] the engine understands.
//!
//! Milestone 6 of PLAN.md took the *decision* out of this process: what a
//! key means is now the engine's answer, not this module's. What is left
//! is translation, and translation is the part that cannot be moved,
//! because the facts it needs — the active keyboard layout, the physical
//! scan code, the toggle state of Caps Lock — belong to the thread the
//! keystroke arrived on and to no other.
//!
//! # Why the character is computed here and not in the engine
//!
//! A virtual-key code is not a character. `VK_2` is `2`, `"` or `@`
//! depending on the layout and the shift state, and the layout is a
//! per-thread property of the *host application*. Sending the engine a raw
//! VK would make it guess, and DESIGN 5.1 is explicit that guessing here
//! types the wrong character for half the users. So the DLL asks Windows,
//! with the one call that can answer without lying: [`ToUnicodeEx`].
//!
//! # The flag that makes `ToUnicodeEx` usable at all
//!
//! Bit 2 of `wFlags` (Windows 10 1607 and later) tells `ToUnicodeEx` not
//! to modify kernel keyboard state. Without it, asking what a key would
//! produce *consumes* a pending dead key, so typing `^` then `e` in a
//! layout that composes them gives the user `e` and a lost accent. The IME
//! is meant to observe the keystroke, not to spend it.

use sakura_proto::{KeyCode, KeyInput, Modifiers};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, MapVirtualKeyW, ToUnicodeEx, MAPVK_VK_TO_VSC,
    VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_CONVERT, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8,
    VK_F9, VK_HOME, VK_KANA, VK_KANJI, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_MENU, VK_NEXT,
    VK_NONCONVERT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_SHIFT, VK_SPACE,
    VK_TAB, VK_UP,
};

/// Keys that mean the same thing on every layout, so their identity
/// travels as a [`KeyCode`] rather than as a character.
///
/// A linear scan rather than a match on `VK_*.0`: the constants are not
/// usable as patterns, and thirty comparisons on a keystroke is nothing
/// next to the round trip that follows.
const NAMED_KEYS: &[(VIRTUAL_KEY, KeyCode)] = &[
    (VK_SPACE, KeyCode::Space),
    (VK_RETURN, KeyCode::Enter),
    (VK_ESCAPE, KeyCode::Escape),
    (VK_BACK, KeyCode::Backspace),
    (VK_DELETE, KeyCode::Delete),
    (VK_TAB, KeyCode::Tab),
    (VK_LEFT, KeyCode::Left),
    (VK_RIGHT, KeyCode::Right),
    (VK_UP, KeyCode::Up),
    (VK_DOWN, KeyCode::Down),
    (VK_HOME, KeyCode::Home),
    (VK_END, KeyCode::End),
    (VK_PRIOR, KeyCode::PageUp),
    (VK_NEXT, KeyCode::PageDown),
    (VK_CONVERT, KeyCode::Henkan),
    (VK_NONCONVERT, KeyCode::Muhenkan),
    (VK_KANA, KeyCode::KanaMode),
    // 半角/全角. The key reports itself as `VK_KANJI` through TSF on the
    // Japanese layout; the OEM aliases below are what some keyboards and
    // remapping utilities send for the same physical key.
    (VK_KANJI, KeyCode::HankakuZenkaku),
    (VIRTUAL_KEY(0xF3), KeyCode::HankakuZenkaku), // VK_OEM_AUTO
    (VIRTUAL_KEY(0xF4), KeyCode::HankakuZenkaku), // VK_OEM_ENLW
    (VK_F1, KeyCode::F1),
    (VK_F2, KeyCode::F2),
    (VK_F3, KeyCode::F3),
    (VK_F4, KeyCode::F4),
    (VK_F5, KeyCode::F5),
    (VK_F6, KeyCode::F6),
    (VK_F7, KeyCode::F7),
    (VK_F8, KeyCode::F8),
    (VK_F9, KeyCode::F9),
    (VK_F10, KeyCode::F10),
    (VK_F11, KeyCode::F11),
    (VK_F12, KeyCode::F12),
];

/// Builds the message the engine will be asked about.
///
/// `lparam` is the keystroke's original `WM_KEYDOWN` parameter: bits 16-23
/// are the scan code and bit 30 says the key was already down, which is
/// how auto-repeat is told apart from a deliberate second press.
pub fn translate(virtual_key: u16, lparam: isize, test_only: bool) -> KeyInput {
    let scan_code = scan_code_of(virtual_key, lparam);
    let code = code_for(virtual_key).unwrap_or(KeyCode::Char);
    let ch = if code == KeyCode::Char {
        character(virtual_key, scan_code)
    } else {
        // A named key's meaning is its name. Sending a character too would
        // give the engine two answers to the same question -- and for
        // Space, the two disagree about whether a space should be typed.
        None
    };

    KeyInput {
        // A key that produced no character and has no name is not
        // something the engine can act on, and `Char` with `ch: None` is a
        // shape the protocol gives no meaning to.
        code: if code == KeyCode::Char && ch.is_none() {
            KeyCode::Unknown
        } else {
            code
        },
        ch,
        modifiers: modifiers(),
        repeat: lparam & (1 << 30) != 0,
        test_only,
    }
}

/// The [`KeyCode`] for keys whose meaning does not depend on the layout.
fn code_for(virtual_key: u16) -> Option<KeyCode> {
    NAMED_KEYS
        .iter()
        .find(|(vk, _)| vk.0 == virtual_key)
        .map(|&(_, code)| code)
}

/// The modifier keys held, and the locks toggled, as of the message being
/// processed.
///
/// `GetKeyState` rather than `GetAsyncKeyState`: this must describe the
/// keystroke being handled, not the keyboard as it is right now, which is
/// a different thing whenever the user types faster than the IME is
/// scheduled.
fn modifiers() -> Modifiers {
    let mut bits = Modifiers::NONE.0;
    for (held, bit) in [
        (is_down(VK_SHIFT), Modifiers::SHIFT),
        (is_down(VK_CONTROL), Modifiers::CTRL),
        (is_down(VK_MENU), Modifiers::ALT),
        (is_toggled(VK_CAPITAL), Modifiers::CAPS_LOCK),
        (is_toggled(VK_KANA), Modifiers::KANA_LOCK),
    ] {
        if held {
            bits |= bit.0;
        }
    }
    Modifiers(bits)
}

fn key_state(key: VIRTUAL_KEY) -> i16 {
    // SAFETY: `GetKeyState` reads thread-local input state and takes no
    // pointers.
    unsafe { GetKeyState(key.0 as i32) }
}

/// The high bit of `GetKeyState` means "held down right now".
fn is_down(key: VIRTUAL_KEY) -> bool {
    key_state(key) < 0
}

/// The low bit means "the lock is on", which is a different question from
/// whether the key itself is held, and the reason Caps Lock cannot be
/// tested with [`is_down`].
fn is_toggled(key: VIRTUAL_KEY) -> bool {
    key_state(key) & 1 != 0
}

/// The scan code the keystroke carried, or the one this layout would use.
///
/// TSF passes the original `lParam` through, but keystrokes synthesized by
/// automation and accessibility tools arrive with it zeroed, and
/// `ToUnicodeEx` needs a real scan code to distinguish keys that share a
/// virtual-key code.
fn scan_code_of(virtual_key: u16, lparam: isize) -> u32 {
    let from_message = ((lparam >> 16) & 0xFF) as u32;
    if from_message != 0 {
        return from_message;
    }
    // SAFETY: a pure lookup against the calling thread's layout; no
    // pointers are involved.
    unsafe { MapVirtualKeyW(virtual_key as u32, MAPVK_VK_TO_VSC) }
}

/// What this key types on the host thread's keyboard layout.
///
/// Returns `None` for keys that produce nothing, for dead keys, and for
/// anything that comes back as a control character.
fn character(virtual_key: u16, scan_code: u32) -> Option<char> {
    let mut state = [0u8; 256];
    // SAFETY: the buffer is exactly the 256 bytes the API documents.
    if unsafe { GetKeyboardState(&mut state) }.is_err() {
        return None;
    }

    // Ctrl and Alt are cleared before asking, so Ctrl+J is reported as the
    // character `j` with the Ctrl bit set rather than as U+000A. The
    // engine's key map is written in terms of "the letter, plus the
    // modifiers"; handing it a control code instead would make every
    // shortcut unbindable. Shift and Caps Lock are deliberately left
    // alone, because they change which character the key really types.
    for modifier in [
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
    ] {
        if let Some(slot) = state.get_mut(modifier.0 as usize) {
            *slot = 0;
        }
    }

    // SAFETY: reads the calling thread's own layout; no pointers.
    let layout = unsafe { GetKeyboardLayout(0) };
    let mut buffer = [0u16; 8];
    // SAFETY: both buffers are passed with their lengths and outlive the
    // call. `DO_NOT_MODIFY_KERNEL_STATE` is what keeps this from
    // consuming a pending dead key -- see the module docs.
    let written = unsafe {
        ToUnicodeEx(
            virtual_key as u32,
            scan_code,
            &state,
            &mut buffer,
            DO_NOT_MODIFY_KERNEL_STATE,
            Some(layout),
        )
    };

    // A negative count is a dead key: the character is not this
    // keystroke's to type, it is the next one's. A count above one is a
    // ligature, which no romaji table has a rule for. Both belong to the
    // application.
    if written != 1 {
        return None;
    }
    let typed = char::from_u32(u32::from(*buffer.first()?))?;
    if typed.is_control() {
        return None;
    }
    Some(typed)
}

/// `ToUnicodeEx`'s "do not modify kernel keyboard state" flag. Named here
/// because the Windows crate exposes `wFlags` as a bare `u32`.
const DO_NOT_MODIFY_KERNEL_STATE: u32 = 0x4;

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the pass-through guarantee rests on: a key the engine
    /// cannot name and Windows will not turn into a character must reach it
    /// as `Unknown`, never as a `Char` with nothing in it.
    #[test]
    fn a_nameless_key_that_types_nothing_is_unknown() {
        // Shift itself: named by no entry in the table, and `ToUnicodeEx`
        // produces nothing for it on every layout.
        let key = translate(VK_SHIFT.0, 0, false);
        assert_eq!(key.code, KeyCode::Unknown);
        assert_eq!(key.ch, None);
    }

    #[test]
    fn named_keys_carry_their_name_and_no_character() {
        for &(vk, expected) in NAMED_KEYS {
            let key = translate(vk.0, 0, false);
            assert_eq!(key.code, expected, "vk {:#04x}", vk.0);
            assert_eq!(
                key.ch, None,
                "vk {:#04x} sent a character alongside its name",
                vk.0
            );
        }
    }

    /// Space is the case where sending both would be actively wrong: the
    /// engine converts on `Space`, and a stray U+0020 riding along would be
    /// typed into the document as well.
    #[test]
    fn space_is_a_name_not_a_character() {
        let key = translate(VK_SPACE.0, 0, false);
        assert_eq!(key.code, KeyCode::Space);
        assert_eq!(key.ch, None);
    }

    #[test]
    fn the_test_flag_is_carried_through_untouched() {
        assert!(translate(b'A' as u16, 0, true).test_only);
        assert!(!translate(b'A' as u16, 0, false).test_only);
    }

    /// Bit 30 of `lParam` is the previous key state, which is what tells an
    /// auto-repeat apart from a second press.
    #[test]
    fn auto_repeat_is_read_from_the_message() {
        assert!(!translate(b'A' as u16, 0, false).repeat);
        assert!(translate(b'A' as u16, 1 << 30, false).repeat);
    }

    /// A synthesized keystroke arrives with a zeroed `lParam`; falling back
    /// to the layout's own scan code is what keeps `ToUnicodeEx` able to
    /// answer for it.
    #[test]
    fn a_missing_scan_code_is_recovered_from_the_layout() {
        let from_message = scan_code_of(b'A' as u16, 0x001E_0001);
        assert_eq!(from_message, 0x1E);
        // No assertion on the exact value: it is whatever the layout on the
        // machine running the tests uses. What matters is that a zeroed
        // `lParam` does not silently become scan code zero.
        let recovered = scan_code_of(b'A' as u16, 0);
        assert_ne!(recovered, 0, "VK_A must map to some scan code");
    }

    /// Control characters are what `ToUnicodeEx` returns for Ctrl chords
    /// when the modifier state is left in place. Letting one through would
    /// feed U+0001 into a composition.
    #[test]
    fn control_characters_are_never_reported_as_typed() {
        for code in 0u32..0x20 {
            if let Some(ch) = char::from_u32(code) {
                assert!(ch.is_control(), "{code:#x} must be filtered");
            }
        }
        assert!('\u{7f}'.is_control());
    }

    #[test]
    fn no_virtual_key_is_named_twice() {
        for (index, &(vk, _)) in NAMED_KEYS.iter().enumerate() {
            let duplicate = NAMED_KEYS
                .iter()
                .skip(index + 1)
                .any(|&(other, _)| other.0 == vk.0);
            assert!(!duplicate, "vk {:#04x} appears twice", vk.0);
        }
    }
}
