//! The Sakura Input conversion engine.
//!
//! The engine is one process per interactive logon session, resident from
//! logon (DESIGN 7). It owns everything the IME knows — the romaji table,
//! the key map, and in later phases the dictionary and the learning store —
//! and serves it over a named pipe to the thin DLL loaded into every host
//! application.
//!
//! This is a library with a two-line binary on top of it, so that every
//! part can be exercised by a test that does not have to start a process:
//! the pipe's security descriptor, the framing, and the request handling
//! each have their own tests, and `tests/` reaches all of them.
//!
//! The modules stack in one direction:
//!
//! - [`session`] — per-editing-session state. Pure; no Windows.
//! - [`dispatch`] — request in, response out. Pure; no Windows.
//! - [`server`] — the accept loop that binds the two to the pipe.
//!
//! The pipe itself is not here. Naming, security, framing and both ends
//! live in [`sakura_ipc`], because the DLL, `regtool --stop` and the
//! renderer's watchdog all need the connecting end and none of them should
//! depend on the engine to get it.

#![cfg(windows)]

pub mod ai_text;
pub(crate) mod candidate_projection;
pub mod composition_fence;
pub mod configuration;
#[cfg(test)]
#[path = "../tests/unit/developer_history_order_tests.rs"]
mod developer_history_order;
pub mod dictionary;
pub mod dispatch;
pub mod event_log;
pub mod fault_injection;
mod history_runtime;
pub mod input_history;
pub mod learning;
pub mod long_conversion;
pub mod prediction;
pub mod server;
pub mod session;
mod shift_ascii_space;
#[cfg(test)]
mod shift_ascii_space_tests;
#[cfg(all(test, feature = "dev-fixtures"))]
#[path = "../tests/unit/shift_latin_order_tests.rs"]
mod shift_latin_order;
#[cfg(all(test, feature = "dev-fixtures"))]
#[path = "../tests/unit/space_key_dispatch_tests.rs"]
mod space_key_dispatch;
pub mod timing;
pub mod ui;
pub mod user_dictionary;

#[cfg(all(test, feature = "context-research"))]
mod context_research_session_size_tests {
    #[test]
    fn context_research_session_size_diagnostic() {
        let current_session_bytes = core::mem::size_of::<crate::session::Session>();
        assert_eq!(crate::prediction::MAX_SUGGESTIONS, 9);
        println!("context-core engine size: current-session={current_session_bytes}");
    }
}
