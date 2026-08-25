//! The Sakura Pad keyboard gesture.
//!
//! The raw-input window deliberately does not hand keyboard text to the pad.
//! It reduces each packet to one of the small events in this module and this
//! state machine decides whether a gesture was completed.  Keeping this part
//! independent from USER32 makes the timing, device identity, and lifecycle
//! rules executable on every build (and, importantly, keeps malformed input
//! from becoming an accidental trigger).

use core::fmt;

/// Maximum duration of either a Ctrl tap or the gap between the two taps.
pub const TAP_HOLD_MS: u64 = 500;
pub const TAP_GAP_MS: u64 = 500;

/// The physical side of the Ctrl key.  A left and right Ctrl from different
/// devices must never be combined into one gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSide {
    Left,
    Right,
}

/// The only events the raw-input boundary is allowed to pass to this state
/// machine.  No scan-code payload, text, or device name is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureInput {
    CtrlDown {
        side: ControlSide,
        device: u64,
        at_ms: u64,
        /// Raw input can report a repeated make while the key is held.
        repeat: bool,
    },
    CtrlUp {
        side: ControlSide,
        device: u64,
        at_ms: u64,
    },
    OtherKey {
        device: u64,
        at_ms: u64,
        kind: OtherKeyKind,
    },
    DeviceRemoved {
        device: u64,
        at_ms: u64,
    },
    /// Configuration revisions are part of the gesture identity.  A stale
    /// release after a settings change is a rejection, never a trigger.
    ConfigGeneration {
        generation: u64,
        at_ms: u64,
    },
    Timeout {
        at_ms: u64,
    },
    Shutdown {
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherKeyKind {
    Key,
    Modifier,
    Malformed,
}

/// Every non-waiting branch has an explicit terminal reason.  This is useful
/// for diagnostics and prevents a dropped branch from silently retaining
/// half of a gesture until a future, unrelated key event arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReason {
    Repeat,
    OtherKey,
    Modifier,
    Malformed,
    Timeout,
    DeviceRemoved,
    ConfigGenerationChanged,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureResult {
    Waiting,
    Trigger,
    Terminated(TerminalReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    FirstDown {
        side: ControlSide,
        device: u64,
        at_ms: u64,
    },
    Gap {
        side: ControlSide,
        device: u64,
        at_ms: u64,
    },
    SecondDown {
        side: ControlSide,
        device: u64,
        at_ms: u64,
    },
}

/// A deterministic two-tap Ctrl recognizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadGesture {
    state: State,
    generation: u64,
}

impl Default for PadGesture {
    fn default() -> Self {
        Self::new(0)
    }
}

impl PadGesture {
    pub const fn new(generation: u64) -> Self {
        Self {
            state: State::Idle,
            generation,
        }
    }

    #[cfg(test)]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub const fn is_idle(self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// Cancel a pending gesture for an owner lifecycle change (for example,
    /// disabling the shortcut) without consuming a new keyboard packet.
    pub fn cancel(&mut self, reason: TerminalReason) -> GestureResult {
        self.terminate(reason)
    }

    /// Consume one reduced event.  The state is reset after every terminal
    /// outcome, including a successful trigger.
    pub fn handle(&mut self, input: GestureInput) -> GestureResult {
        match input {
            GestureInput::ConfigGeneration {
                generation,
                at_ms: _,
            } => {
                if generation == self.generation {
                    GestureResult::Waiting
                } else {
                    self.generation = generation;
                    self.terminate(TerminalReason::ConfigGenerationChanged)
                }
            }
            GestureInput::Shutdown { at_ms: _ } => self.terminate(TerminalReason::Shutdown),
            GestureInput::Timeout { at_ms } => match self.timed_out(at_ms) {
                Some(true) => self.terminate(TerminalReason::Timeout),
                Some(false) => GestureResult::Waiting,
                None => self.terminate(TerminalReason::Malformed),
            },
            GestureInput::DeviceRemoved { device, at_ms: _ } => {
                if self.device_is_active(device) {
                    self.terminate(TerminalReason::DeviceRemoved)
                } else {
                    GestureResult::Waiting
                }
            }
            GestureInput::OtherKey {
                device: _,
                at_ms: _,
                kind,
            } => match kind {
                OtherKeyKind::Key => self.terminate(TerminalReason::OtherKey),
                OtherKeyKind::Modifier => self.terminate(TerminalReason::Modifier),
                OtherKeyKind::Malformed => self.terminate(TerminalReason::Malformed),
            },
            GestureInput::CtrlDown {
                side,
                device,
                at_ms,
                repeat,
            } => {
                if repeat {
                    return self.terminate(TerminalReason::Repeat);
                }
                match self.state {
                    State::Idle => {
                        self.state = State::FirstDown {
                            side,
                            device,
                            at_ms,
                        };
                        GestureResult::Waiting
                    }
                    State::FirstDown { .. } | State::SecondDown { .. } => {
                        self.terminate(TerminalReason::Repeat)
                    }
                    State::Gap {
                        side: first_side,
                        device: first_device,
                        at_ms: released_at,
                    } => {
                        if first_side != side || first_device != device {
                            self.terminate(TerminalReason::DeviceRemoved)
                        } else if elapsed(at_ms, released_at).is_some_and(|gap| gap <= TAP_GAP_MS) {
                            self.state = State::SecondDown {
                                side,
                                device,
                                at_ms,
                            };
                            GestureResult::Waiting
                        } else {
                            self.terminate(TerminalReason::Timeout)
                        }
                    }
                }
            }
            GestureInput::CtrlUp {
                side,
                device,
                at_ms,
            } => match self.state {
                State::FirstDown {
                    side: first_side,
                    device: first_device,
                    at_ms: pressed_at,
                } => {
                    if first_side != side || first_device != device {
                        self.terminate(TerminalReason::DeviceRemoved)
                    } else if elapsed(at_ms, pressed_at).is_some_and(|hold| hold <= TAP_HOLD_MS) {
                        self.state = State::Gap {
                            side,
                            device,
                            at_ms,
                        };
                        GestureResult::Waiting
                    } else {
                        self.terminate(TerminalReason::Timeout)
                    }
                }
                State::SecondDown {
                    side: second_side,
                    device: second_device,
                    at_ms: pressed_at,
                } => {
                    if second_side != side || second_device != device {
                        self.terminate(TerminalReason::DeviceRemoved)
                    } else if elapsed(at_ms, pressed_at).is_some_and(|hold| hold <= TAP_HOLD_MS) {
                        self.state = State::Idle;
                        GestureResult::Trigger
                    } else {
                        self.terminate(TerminalReason::Timeout)
                    }
                }
                State::Gap { .. } => self.terminate(TerminalReason::Malformed),
                State::Idle => GestureResult::Waiting,
            },
        }
    }

    fn terminate(&mut self, reason: TerminalReason) -> GestureResult {
        self.state = State::Idle;
        GestureResult::Terminated(reason)
    }

    fn device_is_active(self, device: u64) -> bool {
        match self.state {
            State::FirstDown { device: active, .. }
            | State::Gap { device: active, .. }
            | State::SecondDown { device: active, .. } => active == device,
            State::Idle => false,
        }
    }

    fn timed_out(self, now: u64) -> Option<bool> {
        match self.state {
            State::FirstDown { at_ms, .. } | State::SecondDown { at_ms, .. } => {
                elapsed(now, at_ms).map(|duration| duration > TAP_HOLD_MS)
            }
            State::Gap { at_ms, .. } => elapsed(now, at_ms).map(|duration| duration > TAP_GAP_MS),
            State::Idle => Some(false),
        }
    }
}

fn elapsed(now: u64, then: u64) -> Option<u64> {
    now.checked_sub(then)
}

impl fmt::Display for TerminalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Repeat => "repeat",
            Self::OtherKey => "other-key",
            Self::Modifier => "modifier",
            Self::Malformed => "malformed",
            Self::Timeout => "timeout",
            Self::DeviceRemoved => "device-removed",
            Self::ConfigGenerationChanged => "config-generation-changed",
            Self::Shutdown => "shutdown",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: u64 = 7;
    const SIDE: ControlSide = ControlSide::Left;

    fn down(gesture: &mut PadGesture, at_ms: u64) -> GestureResult {
        gesture.handle(GestureInput::CtrlDown {
            side: SIDE,
            device: DEVICE,
            at_ms,
            repeat: false,
        })
    }

    fn up(gesture: &mut PadGesture, at_ms: u64) -> GestureResult {
        gesture.handle(GestureInput::CtrlUp {
            side: SIDE,
            device: DEVICE,
            at_ms,
        })
    }

    #[test]
    fn second_release_within_two_500_ms_windows_triggers() {
        let mut gesture = PadGesture::new(1);
        assert_eq!(down(&mut gesture, 10), GestureResult::Waiting);
        assert_eq!(up(&mut gesture, 510), GestureResult::Waiting);
        assert_eq!(down(&mut gesture, 1_010), GestureResult::Waiting);
        assert_eq!(up(&mut gesture, 1_510), GestureResult::Trigger);
        assert!(gesture.is_idle());
    }

    #[test]
    fn each_boundary_is_inclusive_but_one_tick_late_is_timeout() {
        let mut gesture = PadGesture::new(0);
        assert_eq!(down(&mut gesture, 0), GestureResult::Waiting);
        assert_eq!(up(&mut gesture, TAP_HOLD_MS), GestureResult::Waiting);
        assert_eq!(
            down(&mut gesture, TAP_HOLD_MS + TAP_GAP_MS),
            GestureResult::Waiting
        );
        assert_eq!(
            up(&mut gesture, TAP_HOLD_MS + TAP_GAP_MS + TAP_HOLD_MS),
            GestureResult::Trigger
        );

        assert_eq!(down(&mut gesture, 0), GestureResult::Waiting);
        assert_eq!(
            up(&mut gesture, TAP_HOLD_MS + 1),
            GestureResult::Terminated(TerminalReason::Timeout)
        );
        assert!(gesture.is_idle());
    }

    #[test]
    fn repeat_other_key_modifier_malformed_and_bad_order_terminate() {
        let mut gesture = PadGesture::new(0);
        assert_eq!(down(&mut gesture, 1), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::CtrlDown {
                side: SIDE,
                device: DEVICE,
                at_ms: 2,
                repeat: true,
            }),
            GestureResult::Terminated(TerminalReason::Repeat)
        );

        for kind in [
            OtherKeyKind::Key,
            OtherKeyKind::Modifier,
            OtherKeyKind::Malformed,
        ] {
            assert_eq!(down(&mut gesture, 10), GestureResult::Waiting);
            assert_eq!(
                gesture.handle(GestureInput::OtherKey {
                    device: DEVICE,
                    at_ms: 11,
                    kind,
                }),
                GestureResult::Terminated(match kind {
                    OtherKeyKind::Key => TerminalReason::OtherKey,
                    OtherKeyKind::Modifier => TerminalReason::Modifier,
                    OtherKeyKind::Malformed => TerminalReason::Malformed,
                })
            );
        }

        assert_eq!(
            gesture.handle(GestureInput::CtrlUp {
                side: SIDE,
                device: DEVICE,
                at_ms: 20,
            }),
            GestureResult::Waiting
        );
        assert_eq!(down(&mut gesture, 21), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::CtrlUp {
                side: SIDE,
                device: DEVICE,
                at_ms: 21,
            }),
            GestureResult::Waiting
        );
        assert_eq!(
            gesture.handle(GestureInput::CtrlUp {
                side: SIDE,
                device: DEVICE,
                at_ms: 22,
            }),
            GestureResult::Terminated(TerminalReason::Malformed)
        );
    }

    #[test]
    fn side_or_device_mismatch_is_rejected_and_never_mixed() {
        let mut gesture = PadGesture::new(0);
        assert_eq!(down(&mut gesture, 100), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::CtrlUp {
                side: ControlSide::Right,
                device: DEVICE,
                at_ms: 101,
            }),
            GestureResult::Terminated(TerminalReason::DeviceRemoved)
        );
        assert_eq!(down(&mut gesture, 200), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::DeviceRemoved {
                device: DEVICE,
                at_ms: 201,
            }),
            GestureResult::Terminated(TerminalReason::DeviceRemoved)
        );
        assert_eq!(down(&mut gesture, 300), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::ConfigGeneration {
                generation: 1,
                at_ms: 301,
            }),
            GestureResult::Terminated(TerminalReason::ConfigGenerationChanged)
        );
        assert_eq!(gesture.generation(), 1);
        assert_eq!(
            gesture.handle(GestureInput::Shutdown { at_ms: 302 }),
            GestureResult::Terminated(TerminalReason::Shutdown)
        );
    }

    #[test]
    fn timeout_event_has_no_effect_in_idle_and_stale_timestamp_cannot_trigger() {
        let mut gesture = PadGesture::new(0);
        assert_eq!(
            gesture.handle(GestureInput::Timeout { at_ms: 10 }),
            GestureResult::Waiting
        );
        assert_eq!(down(&mut gesture, 100), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::Timeout { at_ms: 99 }),
            GestureResult::Terminated(TerminalReason::Malformed),
            "a backwards clock sample is malformed and cannot trigger"
        );
        assert_eq!(down(&mut gesture, 100), GestureResult::Waiting);
        assert_eq!(
            gesture.handle(GestureInput::Timeout { at_ms: 601 }),
            GestureResult::Terminated(TerminalReason::Timeout)
        );
    }
}
