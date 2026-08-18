//! Sakura Input TSF text service — the in-process half of the IME.
//!
//! This DLL is loaded into *every* application the user types into, including
//! ones that are not ours to crash. That constraint drives everything here:
//!
//! - It stays thin. Conversion, dictionaries and learning live in
//!   `sakura_engine.exe`; this side owns TSF plumbing and nothing else
//!   (DESIGN 3).
//! - It never panics. `panic = "abort"` means an unwrap in a host process is
//!   that application dying with the user's unsaved work, so `unwrap`, `expect`
//!   and `panic!` are lint-denied for this crate and errors come back as
//!   `HRESULT`s.
//! - It never spawns processes. AppContainer-sandboxed hosts cannot reliably do
//!   so, so the engine and renderer are started at logon instead (DESIGN 3).
//!
//! COM vtable methods take `&self`, so mutable state lives behind interior
//! mutability with borrows scoped as tightly as possible (DESIGN 4.4).

#![cfg(windows)]

mod candidate_ui;
mod class_factory;
mod composition;
mod conversion_key;
pub mod diagnostic_ring;
mod display_attributes;
mod edit_session;
mod engine;
mod engine_recovery;
#[cfg(test)]
mod engine_recovery_tests;
mod exports;
mod key_handler;
mod mode_item;
mod reconversion;
mod text_service;
mod write_coordinator;
