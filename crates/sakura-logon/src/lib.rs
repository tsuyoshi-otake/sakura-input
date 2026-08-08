//! Explicit terminal-state controller for logon repair and bootstrap.
//!
//! Each operation is attempted exactly once. The caller can observe every
//! terminal branch through [`Outcome`] and its bit-mapped process exit code;
//! there is no intermediate state that silently becomes success.

#![cfg(windows)]

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Succeeded,
    Failed,
}

impl StepState {
    const fn from_success(success: bool) -> Self {
        if success {
            Self::Succeeded
        } else {
            Self::Failed
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Succeeded => "ok",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Engine,
    Renderer,
}

/// Complete terminal state for one logon invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub task_repair: StepState,
    pub profile_repair: StepState,
    pub engine_launch: StepState,
    pub renderer_launch: StepState,
}

impl Outcome {
    pub const fn is_success(self) -> bool {
        matches!(self.task_repair, StepState::Succeeded)
            && matches!(self.profile_repair, StepState::Succeeded)
            && matches!(self.engine_launch, StepState::Succeeded)
            && matches!(self.renderer_launch, StepState::Succeeded)
    }

    /// Zero is complete success. Each failed step owns one stable bit so Task
    /// Scheduler history and the status file identify all simultaneous faults.
    pub const fn exit_code(self) -> i32 {
        let mut code = 0;
        if matches!(self.task_repair, StepState::Failed) {
            code |= 1;
        }
        if matches!(self.profile_repair, StepState::Failed) {
            code |= 2;
        }
        if matches!(self.engine_launch, StepState::Failed) {
            code |= 4;
        }
        if matches!(self.renderer_launch, StepState::Failed) {
            code |= 8;
        }
        code
    }

    pub fn status_record(self) -> String {
        format!(
            "version=1\ttask_repair={}\tprofile_repair={}\tengine_launch={}\trenderer_launch={}\texit_code={}\n",
            self.task_repair.name(),
            self.profile_repair.name(),
            self.engine_launch.name(),
            self.renderer_launch.name(),
            self.exit_code()
        )
    }
}

/// Runs both repairs and both launches once, in dependency order.
///
/// Launching still proceeds when a repair fails: the current session may have
/// intact machine state, and the renderer can recover an engine launch failure.
/// The nonzero terminal result makes Task Scheduler retry the failed work.
pub fn execute<TR, PR, L>(mut repair_task: TR, mut repair_profile: PR, mut launch: L) -> Outcome
where
    TR: FnMut() -> bool,
    PR: FnMut() -> bool,
    L: FnMut(Component) -> bool,
{
    Outcome {
        task_repair: StepState::from_success(repair_task()),
        profile_repair: StepState::from_success(repair_profile()),
        engine_launch: StepState::from_success(launch(Component::Engine)),
        renderer_launch: StepState::from_success(launch(Component::Renderer)),
    }
}

/// Publishes the last terminal state. The record has fixed field names and no
/// user text; truncation means a torn write cannot be mistaken for an older run.
pub fn write_status(path: &Path, outcome: Outcome) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(outcome.status_record().as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn every_failure_combination_has_a_unique_observable_terminal_code() {
        let mut codes = Vec::new();
        for failed_bits in 0..16 {
            let outcome = execute(
                || failed_bits & 1 == 0,
                || failed_bits & 2 == 0,
                |component| match component {
                    Component::Engine => failed_bits & 4 == 0,
                    Component::Renderer => failed_bits & 8 == 0,
                },
            );
            assert_eq!(outcome.exit_code(), failed_bits);
            assert_eq!(outcome.is_success(), failed_bits == 0);
            codes.push(outcome.exit_code());
        }
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 16);
    }

    #[test]
    fn a_wiped_profile_is_repaired_before_components_launch() {
        let profile_present = Cell::new(false);
        let order = RefCell::new(Vec::new());
        let outcome = execute(
            || {
                order.borrow_mut().push("task");
                true
            },
            || {
                order.borrow_mut().push("profile");
                profile_present.set(true);
                true
            },
            |component| {
                assert!(profile_present.get(), "launch raced profile repair");
                order.borrow_mut().push(match component {
                    Component::Engine => "engine",
                    Component::Renderer => "renderer",
                });
                true
            },
        );
        assert!(outcome.is_success());
        assert_eq!(
            order.into_inner(),
            ["task", "profile", "engine", "renderer"]
        );
    }

    #[test]
    fn status_contains_only_fixed_tokens_and_the_terminal_bitmask() {
        let outcome = Outcome {
            task_repair: StepState::Succeeded,
            profile_repair: StepState::Failed,
            engine_launch: StepState::Succeeded,
            renderer_launch: StepState::Failed,
        };
        assert_eq!(
            outcome.status_record(),
            "version=1\ttask_repair=ok\tprofile_repair=failed\tengine_launch=ok\trenderer_launch=failed\texit_code=10\n"
        );
    }
}
