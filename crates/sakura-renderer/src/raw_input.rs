//! Raw Input ownership for the Pad shortcut.
//!
//! This boundary intentionally has a very small output vocabulary.  The
//! hidden renderer HWND receives keyboard packets with INPUTSINK and
//! DEVNOTIFY, but it never disables legacy keyboard processing and it never
//! forwards packet contents to the engine, a log, or the pad window.

use std::ffi::c_void;
use std::mem::{size_of, MaybeUninit};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_SHIFT,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICE_FLAGS,
    RAWINPUTHEADER, RAWKEYBOARD, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT,
    RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::RI_KEY_BREAK;

use crate::pad_gesture::{ControlSide, GestureInput, GestureResult, OtherKeyKind, PadGesture};

/// An individual RAWINPUT packet is small for a keyboard, but the API reports
/// the size before filling the buffer.  A fixed, aligned buffer lets the
/// message handler reject oversized/malformed data without allocating on the
/// UI thread.
pub const RAW_INPUT_BUFFER_SIZE: usize = 4096;
const RAW_KEYBOARD_PACKET_SIZE: usize = size_of::<RAWINPUTHEADER>() + size_of::<RAWKEYBOARD>();

fn valid_keyboard_packet_bounds(copied: u32, advertised: u32) -> bool {
    let copied = copied as usize;
    let advertised = advertised as usize;
    (RAW_KEYBOARD_PACKET_SIZE..=RAW_INPUT_BUFFER_SIZE).contains(&copied)
        && advertised >= RAW_KEYBOARD_PACKET_SIZE
        && advertised <= copied
}

#[repr(C, align(8))]
struct RawInputBuffer([MaybeUninit<u8>; RAW_INPUT_BUFFER_SIZE]);

/// The flags used for an enabled keyboard registration.  Keeping this
/// function public makes the no-NOLEGACY invariant directly testable.
pub const fn enabled_flags() -> RAWINPUTDEVICE_FLAGS {
    RAWINPUTDEVICE_FLAGS(RIDEV_INPUTSINK.0 | RIDEV_DEVNOTIFY.0)
}

/// Reduced packet independent from USER32.  `at_ms` is supplied by the
/// owner; this module does not call a clock from inside packet parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardPacket {
    pub device: u64,
    pub vkey: u16,
    pub flags: u16,
    pub at_ms: u64,
}

/// Convert a keyboard packet into the anonymous gesture vocabulary.  Unknown
/// flags, missing key information, and impossible virtual-key values all
/// become `Malformed`; no key data survives this function.
pub fn reduce_keyboard_packet(packet: KeyboardPacket, repeat: bool) -> GestureInput {
    const ALLOWED_FLAGS: u16 = RI_KEY_BREAK as u16 | 0x02 | 0x04;
    // USER32 reports a null device for synthetic/remote-session keyboard
    // input (for example RDP, Splashtop and SendInput). It is still a valid
    // local desktop input stream. Keep zero as an anonymous device identity
    // so the two taps must remain consistent, while malformed key data is
    // rejected independently below.
    if packet.vkey == 0 || packet.flags & !ALLOWED_FLAGS != 0 || packet.flags & 0x04 != 0 {
        return GestureInput::OtherKey {
            device: packet.device,
            at_ms: packet.at_ms,
            kind: OtherKeyKind::Malformed,
        };
    }

    let is_up = packet.flags & RI_KEY_BREAK as u16 != 0;
    let side = control_side(packet.vkey, packet.flags);
    match side {
        Some(side) if is_up => GestureInput::CtrlUp {
            side,
            device: packet.device,
            at_ms: packet.at_ms,
        },
        Some(side) => GestureInput::CtrlDown {
            side,
            device: packet.device,
            at_ms: packet.at_ms,
            repeat,
        },
        None => GestureInput::OtherKey {
            device: packet.device,
            at_ms: packet.at_ms,
            kind: if is_modifier(packet.vkey) {
                OtherKeyKind::Modifier
            } else {
                OtherKeyKind::Key
            },
        },
    }
}

fn control_side(vkey: u16, flags: u16) -> Option<ControlSide> {
    match vkey {
        value if value == VK_LCONTROL.0 => Some(ControlSide::Left),
        value if value == VK_RCONTROL.0 => Some(ControlSide::Right),
        value if value == VK_CONTROL.0 => {
            if flags & 0x02 != 0 {
                Some(ControlSide::Right)
            } else {
                Some(ControlSide::Left)
            }
        }
        _ => None,
    }
}

fn is_modifier(vkey: u16) -> bool {
    matches!(
        vkey,
        value if value == VK_SHIFT.0
            || value == VK_LSHIFT.0
            || value == VK_RSHIFT.0
            || value == VK_MENU.0
            || value == VK_LMENU.0
            || value == VK_RMENU.0
            || value == 0x5b // VK_LWIN
            || value == 0x5c // VK_RWIN
            || value == 0x14 // VK_CAPITAL
            || value == 0x15 // VK_KANA
    )
}

/// Main-thread owner of the keyboard registration and the gesture reducer.
#[derive(Debug)]
pub struct RawInputOwner {
    hwnd: HWND,
    registered: bool,
    gesture: PadGesture,
    active_control: Option<(ControlSide, u64)>,
}

impl RawInputOwner {
    pub const fn new(hwnd: HWND, generation: u64) -> Self {
        Self {
            hwnd,
            registered: false,
            gesture: PadGesture::new(generation),
            active_control: None,
        }
    }

    /// Register the keyboard only when the configured shortcut is enabled.
    /// `RIDEV_NOLEGACY` is intentionally absent: the Pad observes Ctrl but
    /// must not change normal Ctrl key behaviour in the foreground app.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        if enabled == self.registered {
            return Ok(());
        }
        if enabled {
            let device = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: enabled_flags(),
                hwndTarget: self.hwnd,
            };
            // SAFETY: `device` is a valid keyboard registration and the
            // structure size is the ABI size required by USER32.
            unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32)? };
            self.registered = true;
        } else {
            let _ = self
                .gesture
                .cancel(crate::pad_gesture::TerminalReason::ConfigGenerationChanged);
            self.active_control = None;
            self.unregister()?;
        }
        Ok(())
    }

    /// Explicitly unregister before the hidden host is destroyed.  Windows
    /// requires a null target with RIDEV_REMOVE.
    pub fn unregister(&mut self) -> Result<()> {
        if !self.registered {
            self.active_control = None;
            return Ok(());
        }
        // Mark the owner disabled before making the OS call.  If USER32
        // reports an error, packets already queued for this HWND still take
        // the fail-closed path below instead of being able to complete a
        // gesture after configuration disabled it.
        self.registered = false;
        self.active_control = None;
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01,
            usUsage: 0x06,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        };
        // SAFETY: this is the exact remove form documented for a registered
        // top-level keyboard device.
        unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32)? };
        Ok(())
    }

    pub fn set_generation(&mut self, generation: u64, at_ms: u64) -> GestureResult {
        self.active_control = None;
        self.gesture
            .handle(GestureInput::ConfigGeneration { generation, at_ms })
    }

    pub fn shutdown(&mut self, at_ms: u64) -> GestureResult {
        self.active_control = None;
        self.gesture.handle(GestureInput::Shutdown { at_ms })
    }

    pub fn device_removed(&mut self, device: u64, at_ms: u64) -> GestureResult {
        if self
            .active_control
            .is_some_and(|(_, active)| active == device)
        {
            self.active_control = None;
        }
        self.gesture
            .handle(GestureInput::DeviceRemoved { device, at_ms })
    }

    pub fn timeout(&mut self, at_ms: u64) -> GestureResult {
        if !self.registered {
            self.active_control = None;
            return self
                .gesture
                .cancel(crate::pad_gesture::TerminalReason::ConfigGenerationChanged);
        }
        self.gesture.handle(GestureInput::Timeout { at_ms })
    }

    /// Parse one WM_INPUT payload into a gesture event.  This performs a
    /// bounded two-call `GetRawInputData` and never logs or forwards the raw
    /// packet.  The caller should post the deferred trigger message when the
    /// returned result is `GestureResult::Trigger`.
    pub fn handle_wm_input(&mut self, lparam: LPARAM, at_ms: u64) -> GestureResult {
        if !self.registered {
            self.active_control = None;
            return self
                .gesture
                .cancel(crate::pad_gesture::TerminalReason::ConfigGenerationChanged);
        }
        let handle = HRAWINPUT(lparam.0 as *mut c_void);
        if handle.is_invalid() {
            return self.gesture.handle(GestureInput::OtherKey {
                device: 0,
                at_ms,
                kind: OtherKeyKind::Malformed,
            });
        }
        let mut required = 0u32;
        // SAFETY: querying the size does not write through a data pointer.
        let size = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                None,
                &mut required,
                size_of::<windows::Win32::UI::Input::RAWINPUTHEADER>() as u32,
            )
        };
        if size == u32::MAX
            || required == 0
            || required as usize > RAW_INPUT_BUFFER_SIZE
            || size as usize > RAW_INPUT_BUFFER_SIZE
        {
            return self.gesture.handle(GestureInput::OtherKey {
                device: 0,
                at_ms,
                kind: OtherKeyKind::Malformed,
            });
        }

        let mut buffer = RawInputBuffer([MaybeUninit::uninit(); RAW_INPUT_BUFFER_SIZE]);
        let mut capacity = RAW_INPUT_BUFFER_SIZE as u32;
        // SAFETY: the aligned fixed buffer is writable for the advertised
        // capacity and the API writes at most that many bytes.
        let copied = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                Some(buffer.0.as_mut_ptr().cast()),
                &mut capacity,
                size_of::<windows::Win32::UI::Input::RAWINPUTHEADER>() as u32,
            )
        };
        if copied == u32::MAX
            || copied < RAW_KEYBOARD_PACKET_SIZE as u32
            || copied as usize > RAW_INPUT_BUFFER_SIZE
        {
            return self.gesture.handle(GestureInput::OtherKey {
                device: 0,
                at_ms,
                kind: OtherKeyKind::Malformed,
            });
        }

        // Do not cast this keyboard packet to `RAWINPUT`: that structure's
        // union is sized for the larger mouse payload, so `size_of::<RAWINPUT>`
        // can exceed a perfectly valid header + RAWKEYBOARD packet. Read the
        // two ABI fields at their documented offsets instead.
        // SAFETY: the aligned buffer contains at least RAW_KEYBOARD_PACKET_SIZE
        // initialized bytes after the checks above.
        let header = unsafe { &*buffer.0.as_ptr().cast::<RAWINPUTHEADER>() };
        if header.dwType != RIM_TYPEKEYBOARD.0
            || !valid_keyboard_packet_bounds(copied, header.dwSize)
        {
            return self.gesture.handle(GestureInput::OtherKey {
                device: 0,
                at_ms,
                kind: OtherKeyKind::Malformed,
            });
        }
        // SAFETY: RAWINPUT places its payload immediately after the header;
        // the header identifies RAWKEYBOARD and the bounded size check above
        // covers the complete payload.
        let keyboard = unsafe {
            *buffer
                .0
                .as_ptr()
                .cast::<u8>()
                .add(size_of::<RAWINPUTHEADER>())
                .cast::<RAWKEYBOARD>()
        };
        let device = header.hDevice.0 as usize as u64;
        let is_down = keyboard.Flags & RI_KEY_BREAK as u16 == 0;
        let repeat = is_down
            && control_side(keyboard.VKey, keyboard.Flags)
                .is_some_and(|side| self.active_control == Some((side, device)));
        let reduced = reduce_keyboard_packet(
            KeyboardPacket {
                device,
                vkey: keyboard.VKey,
                flags: keyboard.Flags,
                at_ms,
            },
            repeat,
        );
        match reduced {
            GestureInput::CtrlDown { side, device, .. } => {
                self.active_control = Some((side, device));
            }
            GestureInput::CtrlUp { side, device, .. }
                if self.active_control == Some((side, device)) =>
            {
                self.active_control = None;
            }
            _ => {}
        }
        self.gesture.handle(reduced)
    }
}

impl Drop for RawInputOwner {
    fn drop(&mut self) {
        // Drop cannot report an error.  We still make a best effort to remove
        // the global registration before the HWND disappears.
        let _ = self.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pad_gesture::{GestureResult, PadGesture, TerminalReason};

    #[test]
    fn enabled_registration_is_input_sink_and_device_notify_without_nolegacy() {
        let flags = enabled_flags();
        assert!(flags.contains(RIDEV_INPUTSINK));
        assert!(flags.contains(RIDEV_DEVNOTIFY));
        assert!(!flags.contains(windows::Win32::UI::Input::RIDEV_NOLEGACY));
    }

    #[test]
    fn keyboard_packet_bounds_use_header_plus_keyboard_not_the_larger_union() {
        let exact = RAW_KEYBOARD_PACKET_SIZE as u32;
        assert!(valid_keyboard_packet_bounds(exact, exact));
        assert!(!valid_keyboard_packet_bounds(exact - 1, exact - 1));
        assert!(!valid_keyboard_packet_bounds(exact, exact + 1));
        assert!(!valid_keyboard_packet_bounds(
            RAW_INPUT_BUFFER_SIZE as u32 + 1,
            exact
        ));
        assert!(size_of::<windows::Win32::UI::Input::RAWINPUT>() >= RAW_KEYBOARD_PACKET_SIZE);
    }

    #[test]
    fn keyboard_reduction_keeps_only_control_side_and_device_identity() {
        let packet = KeyboardPacket {
            device: 42,
            vkey: VK_LCONTROL.0,
            flags: 0,
            at_ms: 10,
        };
        assert_eq!(
            reduce_keyboard_packet(packet, false),
            GestureInput::CtrlDown {
                side: ControlSide::Left,
                device: 42,
                at_ms: 10,
                repeat: false,
            }
        );
        assert_eq!(
            reduce_keyboard_packet(
                KeyboardPacket {
                    vkey: VK_RCONTROL.0,
                    flags: 0x02 | RI_KEY_BREAK as u16,
                    ..packet
                },
                false,
            ),
            GestureInput::CtrlUp {
                side: ControlSide::Right,
                device: 42,
                at_ms: 10,
            }
        );
    }

    #[test]
    fn anonymous_remote_keyboard_can_complete_the_same_bounded_gesture() {
        let mut gesture = PadGesture::new(1);
        for (flags, at_ms) in [(0, 10), (RI_KEY_BREAK as u16, 20), (0, 30)] {
            let input = reduce_keyboard_packet(
                KeyboardPacket {
                    device: 0,
                    vkey: VK_LCONTROL.0,
                    flags,
                    at_ms,
                },
                false,
            );
            assert_eq!(gesture.handle(input), GestureResult::Waiting);
        }
        let release = reduce_keyboard_packet(
            KeyboardPacket {
                device: 0,
                vkey: VK_LCONTROL.0,
                flags: RI_KEY_BREAK as u16,
                at_ms: 40,
            },
            false,
        );
        assert_eq!(gesture.handle(release), GestureResult::Trigger);
    }

    #[test]
    fn malformed_and_other_modifier_packets_are_terminal() {
        let malformed = reduce_keyboard_packet(
            KeyboardPacket {
                device: 1,
                vkey: 0,
                flags: 0,
                at_ms: 1,
            },
            false,
        );
        assert!(matches!(
            malformed,
            GestureInput::OtherKey {
                kind: OtherKeyKind::Malformed,
                ..
            }
        ));
        let mut gesture = PadGesture::default();
        assert_eq!(
            gesture.handle(reduce_keyboard_packet(
                KeyboardPacket {
                    device: 1,
                    vkey: VK_SHIFT.0,
                    flags: 0,
                    at_ms: 1,
                },
                false,
            )),
            GestureResult::Terminated(TerminalReason::Modifier)
        );
    }

    #[test]
    fn disabled_owner_rejects_queued_input_and_timeout() {
        let mut owner = RawInputOwner::new(HWND::default(), 0);
        assert_eq!(
            owner.handle_wm_input(LPARAM(0), 1),
            GestureResult::Terminated(TerminalReason::ConfigGenerationChanged)
        );
        assert_eq!(
            owner.timeout(2),
            GestureResult::Terminated(TerminalReason::ConfigGenerationChanged)
        );
    }
}
