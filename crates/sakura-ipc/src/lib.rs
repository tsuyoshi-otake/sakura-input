//! The pipe.
//!
//! Everything about the channel between the TSF DLL and the engine lives
//! here: what it is called, who is allowed to open it, how bytes are
//! framed on it, and both ends of it. Four processes need some part of
//! that — the DLL connects, the engine listens, `sakura_regtool --stop`
//! connects to ask the engine to exit, and the renderer's watchdog
//! connects to check that it is alive — and every one of them has to
//! agree exactly.
//!
//! Agreement here is not a style preference. The server withholds
//! `FILE_CREATE_PIPE_INSTANCE` from sandboxed callers, and that bit is
//! the same bit as `FILE_APPEND_DATA`, so a client that asks for
//! `GENERIC_WRITE` is *denied outright* ([`security::CLIENT_ACCESS`]
//! explains why). A second implementation of "open the pipe" written
//! from the same documentation would get that wrong, and the failure
//! would appear only inside a sandboxed host — a browser tab — which is
//! exactly where nobody is attached to a debugger.
//!
//! What is deliberately *not* here: sessions, conversion, key handling.
//! This crate knows how to move an opaque payload; [`sakura_proto`] knows
//! what the payload means. The split is what lets the protocol's tests
//! and fuzzers run on plain byte slices with no pipe in sight.

#![cfg(windows)]

pub mod client;
pub mod security;
pub mod transport;

pub use client::Client;
pub use security::{pipe_name, sddl, Descriptor, CLIENT_ACCESS};
pub use transport::{Fault, PipeInstance, MAX_INSTANCES};
