# Oracle provenance

source: `crates/sakura-engine/src/space_key_dispatch_oracle.rs`

static production-import scan: pass

forbidden tokens checked: crate::dispatch, crate::session, crate::server, sakura_core::keymap, KeyMap, idle_space_commit, Dispatcher, SessionTable

Expected values come from `verification/space-key-dispatch/requirements.md`, not from observed production OutputBuf commits.
