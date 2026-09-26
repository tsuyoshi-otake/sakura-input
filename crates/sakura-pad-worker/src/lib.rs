//! Password-encrypted envelope primitives for Sakura Pad protection.
//!
//! The experimental stdio worker exposes a bounded authenticated envelope
//! scoped to one vault or one memo. It does not yet own production storage,
//! migration, unlocked sessions or renderer integration.

pub mod envelope;
pub mod protocol;

pub use envelope::{open, seal, EnvelopeError, Scope, MAX_PLAINTEXT_BYTES};
