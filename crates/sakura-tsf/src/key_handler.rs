//! Deciding what a keystroke means.
//!
//! Milestone 3 of PLAN.md deliberately hardcodes this: type letters, watch them
//! appear underlined, press Enter to commit. Milestone 4 moves the real decision
//! — the romaji FSM and the configurable keymap — into `sakura-core`, and
//! milestone 6 moves it out of this process entirely.
//!
//! What survives that move is the shape: classification is a pure function of
//! (key, modifiers, whether a preedit exists), so it can be tested exhaustively
//! without a document, a thread manager, or a running IME.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_RETURN, VK_SHIFT,
    VK_SPACE,
};

/// Virtual-key codes for the ASCII rows, from the Win32 documentation: `VK_A`
/// through `VK_Z` and `VK_0` through `VK_9` have no named constants because they
/// are defined to equal their ASCII characters.
const VK_LETTER_FIRST: u16 = b'A' as u16;
const VK_LETTER_LAST: u16 = b'Z' as u16;
const VK_DIGIT_FIRST: u16 = b'0' as u16;
const VK_DIGIT_LAST: u16 = b'9' as u16;

/// The modifier keys held when a keystroke arrived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

impl Modifiers {
    /// Reads the current keyboard state.
    ///
    /// TSF delivers the virtual key but not the modifiers, so they have to be
    /// sampled. `GetKeyState` is the right call rather than `GetAsyncKeyState`:
    /// it reports the state as of the message being processed, not as of now,
    /// which matters when the user types faster than the IME is scheduled.
    pub fn current() -> Self {
        Self {
            shift: is_down(VK_SHIFT),
            control: is_down(VK_CONTROL),
            alt: is_down(VK_MENU),
        }
    }
}

fn is_down(key: VIRTUAL_KEY) -> bool {
    // SAFETY: `GetKeyState` reads thread-local input state and takes no pointers.
    let state = unsafe { GetKeyState(key.0 as i32) };
    // The high bit means "currently down"; the low bit is the toggle state, which
    // is why Caps Lock cannot be tested the same way.
    state < 0
}

/// What the text service should do with a keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Not ours. The application sees the key exactly as if no IME were loaded.
    PassThrough,
    /// Append to the preedit.
    Insert(char),
    /// Remove the last character of the preedit.
    Erase,
    /// Hand the preedit to the application as ordinary text.
    Commit,
    /// Throw the preedit away.
    Cancel,
}

impl KeyAction {
    /// Whether TSF should be told the key was consumed.
    pub fn consumes_key(self) -> bool {
        !matches!(self, KeyAction::PassThrough)
    }
}

/// Classifies a keystroke.
///
/// `composing` is what makes this safe to leave installed: with no preedit open,
/// Backspace, Enter and Escape all pass through, so the IME is invisible until
/// the user actually starts typing into it.
pub fn classify(vk: u16, modifiers: Modifiers, composing: bool) -> KeyAction {
    // Ctrl and Alt combinations belong to the application. An IME that swallows
    // Ctrl+S has broken every editor it is installed in, and no amount of
    // conversion quality makes up for that.
    if modifiers.control || modifiers.alt {
        return KeyAction::PassThrough;
    }

    let while_composing = |action: KeyAction| {
        if composing {
            action
        } else {
            KeyAction::PassThrough
        }
    };

    match vk {
        v if v == VK_BACK.0 => while_composing(KeyAction::Erase),
        v if v == VK_RETURN.0 => while_composing(KeyAction::Commit),
        v if v == VK_ESCAPE.0 => while_composing(KeyAction::Cancel),

        // Microsoft IME converts on Space. There is nothing to convert yet, so
        // this milestone treats it as "finish here" — the same key ends the
        // preedit either way, which keeps the muscle memory correct while the
        // behaviour behind it is still being built.
        v if v == VK_SPACE.0 => while_composing(KeyAction::Commit),

        v @ VK_LETTER_FIRST..=VK_LETTER_LAST => {
            let offset = (v - VK_LETTER_FIRST) as u8;
            let letter = if modifiers.shift {
                b'A' + offset
            } else {
                b'a' + offset
            };
            KeyAction::Insert(letter as char)
        }

        // Unshifted digits are the same on every layout this IME targets.
        // Shifted ones are not — Shift+2 is `"` on a Japanese keyboard and `@`
        // on a US one — so guessing here would type the wrong character for half
        // the users. Milestone 4 reads the real layout instead.
        v @ VK_DIGIT_FIRST..=VK_DIGIT_LAST if !modifiers.shift => {
            KeyAction::Insert((b'0' + (v - VK_DIGIT_FIRST) as u8) as char)
        }

        _ => KeyAction::PassThrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        control: false,
        alt: false,
    };
    const CONTROL: Modifiers = Modifiers {
        shift: false,
        control: true,
        alt: false,
    };
    const ALT: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: true,
    };

    #[test]
    fn every_letter_maps_to_itself() {
        for (index, expected) in ('a'..='z').enumerate() {
            let vk = VK_LETTER_FIRST + index as u16;
            assert_eq!(classify(vk, NONE, false), KeyAction::Insert(expected));
        }
    }

    #[test]
    fn shift_gives_capitals() {
        for (index, expected) in ('A'..='Z').enumerate() {
            let vk = VK_LETTER_FIRST + index as u16;
            assert_eq!(classify(vk, SHIFT, false), KeyAction::Insert(expected));
        }
    }

    #[test]
    fn every_digit_maps_to_itself() {
        for (index, expected) in ('0'..='9').enumerate() {
            let vk = VK_DIGIT_FIRST + index as u16;
            assert_eq!(classify(vk, NONE, false), KeyAction::Insert(expected));
        }
    }

    #[test]
    fn shifted_digits_are_left_to_the_application() {
        for vk in VK_DIGIT_FIRST..=VK_DIGIT_LAST {
            assert_eq!(classify(vk, SHIFT, true), KeyAction::PassThrough);
        }
    }

    /// The guarantee that makes this milestone safe to install: with no preedit
    /// open, every editing key belongs to the application.
    #[test]
    fn editing_keys_pass_through_when_idle() {
        for vk in [VK_BACK.0, VK_RETURN.0, VK_ESCAPE.0, VK_SPACE.0] {
            assert_eq!(classify(vk, NONE, false), KeyAction::PassThrough);
        }
    }

    #[test]
    fn editing_keys_act_while_composing() {
        assert_eq!(classify(VK_BACK.0, NONE, true), KeyAction::Erase);
        assert_eq!(classify(VK_RETURN.0, NONE, true), KeyAction::Commit);
        assert_eq!(classify(VK_ESCAPE.0, NONE, true), KeyAction::Cancel);
        assert_eq!(classify(VK_SPACE.0, NONE, true), KeyAction::Commit);
    }

    /// Application shortcuts must survive an active preedit, which is the case
    /// most likely to be got wrong.
    #[test]
    fn shortcuts_are_never_consumed() {
        for modifiers in [CONTROL, ALT] {
            for vk in 0u16..=0xFF {
                assert_eq!(
                    classify(vk, modifiers, true),
                    KeyAction::PassThrough,
                    "vk {vk:#04x} was consumed with {modifiers:?}"
                );
            }
        }
    }

    #[test]
    fn function_and_navigation_keys_pass_through() {
        // VK_F1..VK_F12, arrows, Home/End/PageUp/PageDown, Tab.
        for vk in (0x70..=0x7B).chain(0x21..=0x28).chain([0x09]) {
            assert_eq!(classify(vk, NONE, true), KeyAction::PassThrough);
        }
    }

    #[test]
    fn only_pass_through_declines_the_key() {
        assert!(!KeyAction::PassThrough.consumes_key());
        for action in [
            KeyAction::Insert('a'),
            KeyAction::Erase,
            KeyAction::Commit,
            KeyAction::Cancel,
        ] {
            assert!(action.consumes_key());
        }
    }
}
