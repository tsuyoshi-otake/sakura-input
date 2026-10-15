//! The scheduling class the engine and its helper processes run at,
//! whoever started them.
//!
//! The text service waits for the engine synchronously, on the host
//! application's UI thread, and gives each keystroke `KEY_BUDGET` (50 ms in
//! `sakura-tsf`). A reply that misses that budget is a dropped keystroke and
//! a finalized composition (#148). The engine is therefore the server half
//! of a foreground interaction, not a background job.
//!
//! Nothing about how the engine is started says so. The logon task is
//! registered with the Task Scheduler's default priority, 7, which is
//! `BELOW_NORMAL_PRIORITY_CLASS`; Setup runs that same task; and
//! `CreateProcess` hands a below-normal parent's class down to a child that
//! does not ask for one. Every installed engine and renderer therefore ran
//! below normal (#322), and a build or any other normal-priority work that
//! filled every core kept the engine waiting for a turn while the user's
//! keystroke waited for the engine.
//!
//! So the engine sets its own class at startup instead of trusting its
//! launcher, and every helper it starts is given an explicit class rather
//! than whatever the engine's class would make the default. Raising the
//! engine changes the default for its children too: below a below-normal
//! parent a child inherits that class, below any other parent it gets
//! normal.

use windows::Win32::System::Threading::{
    GetCurrentProcess, GetPriorityClass, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS,
    NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS, REALTIME_PRIORITY_CLASS,
};

/// A Win32 process priority class, named for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
    /// `GetPriorityClass` failed (it returns zero) or returned a value this
    /// build does not know.
    Unknown,
}

impl PriorityClass {
    /// Classifies a raw `GetPriorityClass` result.
    pub fn from_raw(raw: u32) -> Self {
        match PROCESS_CREATION_FLAGS(raw) {
            IDLE_PRIORITY_CLASS => Self::Idle,
            BELOW_NORMAL_PRIORITY_CLASS => Self::BelowNormal,
            NORMAL_PRIORITY_CLASS => Self::Normal,
            ABOVE_NORMAL_PRIORITY_CLASS => Self::AboveNormal,
            HIGH_PRIORITY_CLASS => Self::High,
            REALTIME_PRIORITY_CLASS => Self::Realtime,
            _ => Self::Unknown,
        }
    }

    /// The `CreateProcess` / `SetPriorityClass` flag for this class.
    /// `Unknown` has none.
    pub fn flag(self) -> Option<PROCESS_CREATION_FLAGS> {
        match self {
            Self::Idle => Some(IDLE_PRIORITY_CLASS),
            Self::BelowNormal => Some(BELOW_NORMAL_PRIORITY_CLASS),
            Self::Normal => Some(NORMAL_PRIORITY_CLASS),
            Self::AboveNormal => Some(ABOVE_NORMAL_PRIORITY_CLASS),
            Self::High => Some(HIGH_PRIORITY_CLASS),
            Self::Realtime => Some(REALTIME_PRIORITY_CLASS),
            Self::Unknown => None,
        }
    }

    /// Fixed diagnostic token.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::BelowNormal => "below_normal",
            Self::Normal => "normal",
            Self::AboveNormal => "above_normal",
            Self::High => "high",
            Self::Realtime => "realtime",
            Self::Unknown => "unknown",
        }
    }
}

/// The class the engine runs at.
///
/// Above normal, so that a key request preempts ordinary normal-priority
/// work instead of queueing behind it. Measured under a saturated CPU in
/// `verification/high-load-input-integrity.md` §5.6. Not `High`: the engine
/// also runs learning maintenance and the input-history writer, and a
/// defect in any of them must not be able to starve the user's own
/// foreground application.
pub const ENGINE_CLASS: PriorityClass = PriorityClass::AboveNormal;

/// Creation flags for every helper process the engine starts: the neural
/// reranking worker and the AI text worker.
///
/// Neither is on the key path (conversion never waits for the reranker, and
/// AI text is a separate request), and the reranker's inference is the most
/// CPU-heavy thing Sakura Input runs. Below normal keeps both behind the
/// user's own work and behind the engine, which is also the class they had
/// when the engine itself inherited below normal from the logon task.
pub const HELPER_CREATION_FLAGS: u32 = CREATE_NO_WINDOW.0 | BELOW_NORMAL_PRIORITY_CLASS.0;

/// What [`apply_engine_class`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The process now runs at `applied`; it started at `inherited`.
    Set {
        inherited: PriorityClass,
        applied: PriorityClass,
    },
    /// `SetPriorityClass` failed, so the process keeps `inherited`.
    Failed {
        inherited: PriorityClass,
        hresult: i32,
    },
}

/// The current process's priority class.
pub fn current() -> PriorityClass {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always
    // valid in the calling process and needs no closing.
    PriorityClass::from_raw(unsafe { GetPriorityClass(GetCurrentProcess()) })
}

/// Moves this process to [`ENGINE_CLASS`] and reports what it replaced.
///
/// A failure is not fatal: an engine at its inherited class is slower under
/// load, while an engine that refused to start would leave the user with no
/// input method at all. The caller records the outcome so that a machine
/// whose policy blocks the change is visible in `engine.log`.
pub fn apply_engine_class() -> Applied {
    let inherited = current();
    let flag = ENGINE_CLASS
        .flag()
        .expect("ENGINE_CLASS is a concrete priority class");
    // SAFETY: the pseudo-handle from `GetCurrentProcess` carries
    // PROCESS_SET_INFORMATION for the calling process.
    match unsafe { SetPriorityClass(GetCurrentProcess(), flag) } {
        Ok(()) => Applied::Set {
            inherited,
            applied: current(),
        },
        Err(error) => Applied::Failed {
            inherited,
            hresult: error.code().0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_concrete_class_round_trips_through_its_flag() {
        for class in [
            PriorityClass::Idle,
            PriorityClass::BelowNormal,
            PriorityClass::Normal,
            PriorityClass::AboveNormal,
            PriorityClass::High,
            PriorityClass::Realtime,
        ] {
            let flag = class.flag().expect("a concrete class has a flag");
            assert_eq!(PriorityClass::from_raw(flag.0), class);
        }
        assert_eq!(PriorityClass::Unknown.flag(), None);
        assert_eq!(PriorityClass::from_raw(0), PriorityClass::Unknown);
    }

    #[test]
    fn the_engine_runs_above_the_class_its_logon_task_gives_it() {
        let task_default = PriorityClass::BelowNormal;
        let rank = |class: PriorityClass| {
            [
                PriorityClass::Idle,
                PriorityClass::BelowNormal,
                PriorityClass::Normal,
                PriorityClass::AboveNormal,
                PriorityClass::High,
                PriorityClass::Realtime,
            ]
            .iter()
            .position(|candidate| *candidate == class)
            .expect("a concrete class")
        };
        assert!(rank(ENGINE_CLASS) > rank(PriorityClass::Normal));
        assert!(rank(ENGINE_CLASS) > rank(task_default));
        assert!(rank(ENGINE_CLASS) < rank(PriorityClass::High));
    }

    #[test]
    fn helpers_start_windowless_and_below_normal() {
        assert_eq!(
            HELPER_CREATION_FLAGS & CREATE_NO_WINDOW.0,
            CREATE_NO_WINDOW.0
        );
        // Exactly one priority-class bit, and it is below normal: two class
        // bits make CreateProcess pick the lowest, silently.
        let class_bits = HELPER_CREATION_FLAGS
            & (IDLE_PRIORITY_CLASS.0
                | BELOW_NORMAL_PRIORITY_CLASS.0
                | NORMAL_PRIORITY_CLASS.0
                | ABOVE_NORMAL_PRIORITY_CLASS.0
                | HIGH_PRIORITY_CLASS.0
                | REALTIME_PRIORITY_CLASS.0);
        assert_eq!(class_bits, BELOW_NORMAL_PRIORITY_CLASS.0);
    }
}
