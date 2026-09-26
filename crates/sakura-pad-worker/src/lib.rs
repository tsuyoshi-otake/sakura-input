//! Password-encrypted envelope primitives for Sakura Pad protection.
//!
//! The experimental stdio worker exposes a bounded authenticated envelope
//! scoped to one vault or one memo. It does not yet own production storage,
//! migration, a production unlocked-session owner or renderer integration.

pub mod envelope;
pub mod protocol;
pub mod session_protocol;

pub use envelope::{
    open, seal, unlock, EnvelopeError, Scope, UnlockedEnvelope, MAX_PLAINTEXT_BYTES,
};
