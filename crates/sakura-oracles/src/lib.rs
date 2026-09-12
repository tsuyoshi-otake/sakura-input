//! Development-only reference oracles for Sakura Input engine behavior.
//!
//! These modules encode independently stated expected behavior. Shipping
//! crates may use this crate only as a development dependency.

pub mod developer_history_oracle;
#[cfg(test)]
mod developer_history_oracle_tests;
pub mod shift_latin_oracle;
#[cfg(test)]
mod shift_latin_oracle_tests;
pub mod space_key_dispatch_oracle;
#[cfg(test)]
mod space_key_dispatch_oracle_tests;
