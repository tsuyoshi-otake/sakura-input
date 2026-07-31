//! Sakura Input engine core: the conversion logic, with no Windows in it.
//!
//! Everything here is a pure function of its inputs — `(state, event) ->
//! (state, output)` — which is what makes the IME testable without a running
//! Windows, without TSF, and without a host application (DESIGN 5). A
//! `windows` dependency in this crate is a design violation, not an oversight.
//!
//! The modules stack in one direction, so the dependency graph stays acyclic:
//!
//! - [`cpu`] — which vector instructions this machine has, resolved once.
//! - [`simd`] — kernels selected by that answer; scalar-equivalent by test.
//! - [`text`] — the sink everything writes through.
//! - [`config`] — the small TOML subset every shipped data file is written in.
//! - [`romaji`] — the input FSM, compiled from a config document.
//! - [`width`] — the width and punctuation choke point (DESIGN 5.6).
//! - [`keymap`] — key bindings, also compiled from a config document.
//!
//! [`cpu`] and [`simd`] are the one concession to a specific machine, and
//! they are careful ones: both compile and pass their tests on any
//! architecture, because a core that could only be tested on the target
//! would give up the property the rest of this crate exists for.

pub mod config;
pub mod cpu;
pub mod keymap;
pub mod romaji;
pub mod simd;
pub mod text;
pub mod width;

pub use config::{parse as parse_config, Document, ErrorKind, ParseError, Value};
pub use cpu::{Tier, UnsupportedCpu};
pub use keymap::{Action, KeyMap, KeyMapError, KeyMapErrorKind, Preset, State};
pub use romaji::{Input, Table, TableError, TableErrorKind};
pub use text::TextSink;
pub use width::{Normalizer, PunctuationStyle, Width, WidthPolicy};
