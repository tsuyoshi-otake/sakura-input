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

pub mod configuration;
pub mod dictionary;
pub mod dispatch;
pub mod event_log;
pub mod input_history;
pub mod learning;
pub mod prediction;
pub mod server;
pub mod session;
pub mod ui;
pub mod user_dictionary;
