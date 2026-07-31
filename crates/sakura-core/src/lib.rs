//! Sakura Input engine core: the conversion logic, with no Windows in it.
//!
//! Everything here is a pure function of its inputs — `(state, event) ->
//! (state, output)` — which is what makes the IME testable without a running
//! Windows, without TSF, and without a host application (DESIGN 5). A
//! `windows` dependency in this crate is a design violation, not an oversight.
//!
//! The modules stack in one direction, so the dependency graph stays acyclic:
//!
//! - [`text`] — the sink everything writes through.
//! - [`config`] — the small TOML subset every shipped data file is written in.
//! - [`romaji`] — the input FSM, compiled from a config document.
//! - [`width`] — the width and punctuation choke point (DESIGN 5.6).
//! - [`keymap`] — key bindings, also compiled from a config document.

pub mod config;
pub mod keymap;
pub mod romaji;
pub mod text;
pub mod width;

pub use config::{parse as parse_config, Document, ErrorKind, ParseError, Value};
pub use keymap::{Action, KeyMap, KeyMapError, KeyMapErrorKind, Preset, State};
pub use romaji::{Input, Table, TableError, TableErrorKind};
pub use text::TextSink;
pub use width::{Normalizer, PunctuationStyle, Width, WidthPolicy};
