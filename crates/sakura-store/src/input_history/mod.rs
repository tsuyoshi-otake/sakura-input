mod codec;
mod format;
#[allow(unsafe_code)]
pub mod persistence;

pub use format::*;

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
