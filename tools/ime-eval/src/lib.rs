//! Offline quality-measurement runner for Sakura Input.
//!
//! Shipping IME binaries must not depend on this crate. It exists so that
//! mechanical contracts stay in deterministic oracles and Japanese meaning
//! quality can be judged blindly by a calibrated Luna Max session.

pub mod aggregate;
pub mod backend;
pub mod blind;
pub mod calibration;
pub mod capture;
pub mod capture_engine;
pub mod cli;
pub mod codex;
pub mod corpus;
pub mod gate;
pub mod hash;
pub mod history_approval;
pub mod identity;
pub mod isolation;
pub mod judge;
pub mod oracle;
pub mod paths;
pub mod prompt;
pub mod quality;
pub mod report;
pub mod schema;
pub mod types;

pub use types::Error;
