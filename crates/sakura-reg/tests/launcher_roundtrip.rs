//! Registers the logon task against the real Task Scheduler and removes it
//! again.
//!
//! Ignored by default because it changes machine state: it creates a
//! scheduled task under the running account and — if that account may
//! create task folders — a `Sakura Input` folder. Everything the test
//! creates it also removes, but a test run should not silently reconfigure
//! the machine it runs on, so this one is opt-in:
//!
//! ```text
//! cargo test -p sakura-reg --test launcher_roundtrip -- --ignored
//! ```
//!
//! What it proves that the unit tests cannot: that the settings in
//! `launcher::register` are a combination the Task Scheduler actually
//! accepts. Several of them (`PT0S` for "no execution limit", a logon
//! trigger scoped by user id, `TASK_RUNLEVEL_LUA` with an interactive
//! token) are rejected at registration time in some combinations, and the
//! rejection is an `HRESULT` from a COM call that no amount of local
//! reasoning predicts.

#![cfg(windows)]

use sakura_reg::{launcher, ComApartment};

#[test]
#[ignore = "creates and deletes a real scheduled task"]
fn the_logon_task_registers_and_unregisters() {
    let _com = ComApartment::new().expect("COM apartment");

    // Leftovers from an interrupted earlier run would make the assertions
    // below pass for the wrong reason.
    launcher::unregister().expect("a clean starting point");
    assert!(
        !launcher::is_registered(),
        "the task must be absent before the test registers it"
    );

    let program = std::env::current_exe().expect("this test's own binary");
    launcher::register(&[program.as_path()]).expect("registration");
    assert!(
        launcher::is_registered(),
        "the task must be visible immediately after registration"
    );

    // Normal upgrades and logon repair preserve the stable task instead of
    // rewriting its ACL-protected definition. The ensure operation remains
    // idempotent and reports success when that task is already present.
    launcher::register_if_missing(&[program.as_path()]).expect("existing registration");
    assert!(launcher::is_registered());

    launcher::unregister().expect("removal");
    assert!(
        !launcher::is_registered(),
        "the task must be gone after removal"
    );

    // Uninstall runs this unconditionally, including on machines where
    // registration never succeeded.
    launcher::unregister().expect("removing an absent task is not an error");
}

#[test]
fn registering_nothing_is_refused() {
    let _com = ComApartment::new().expect("COM apartment");
    assert!(
        launcher::register(&[]).is_err(),
        "a task with no actions would look installed and do nothing"
    );
    assert!(
        launcher::register_if_missing(&[]).is_err(),
        "the no-op path must not make an empty task request look valid"
    );
}
