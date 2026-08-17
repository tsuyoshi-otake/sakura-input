# Scope manifest — space-key-dispatch

Manifest version: 1
Source revision: `f26191aa16a6b3569cdf004e4852650f7de1a17f`
Domain: Japanese IME Space = convert vs idle fullwidth insert across engine connections.

## Entry points

- `Dispatcher::dispatch` / `apply_key` idle Space and `Action::Convert`
- Named-pipe `SendKey` per connection (`server.rs` share-nothing workers)
- TSF `handle_key` / `observe_write_context` (inventory only; no live ITfContext)

## Exact source paths and SHA-256

| path | SHA-256 | bytes |
|---|---|---:|
| `crates/sakura-engine/src/dispatch.rs` | `37ed0bd31e02c7d45aab4ec268e39a87eb96d23856343cfd59a7504437d91fb2` | 469606 |
| `crates/sakura-engine/src/server.rs` | `a860b3af7a44a779db47efe1d878c564cfaded5bd4bcc5229e7debebafca46de` | 75096 |
| `crates/sakura-engine/src/session.rs` | `811570432f060a54d643c288db75ddf4db7013b6390d2308b1bb1d0d360a86dd` | 78405 |
| `crates/sakura-core/src/preferences.rs` | `1a3de71c4156d3edff0d91c84065df7c013ce87fe302de4c9296bd4dfa849b70` | 48641 |
| `data/keymap-ms-ime.toml` | `3b7d5259a7d96cd7829f8b48749420e05dbda7caaf1567f74accba3f5f0adbac` | 6589 |
| `data/keymap-atok.toml` | `fcdc34378b0451c49f1c6c4924f71d00f3f41a7e33dc434aa4db141a5a2556be` | 7193 |
| `crates/sakura-tsf/src/text_service.rs` | `6f22ce5fe31bcf9825aab000dd4020e0d243787ea2c6e5d36b3aab65e1ba4dc1` | 327832 |
| `crates/sakura-tsf/src/engine.rs` | `7fdfa0f948f4fb867f236257c167471b78e41dbfd2064e209c8702162f44695c` | 60130 |

## Symbols

- `idle_space_commit` arm in `apply_key`
- `Action::Convert` / `begin_conversion`
- `SpaceWidth::is_full` / `ShiftSpaceBehavior::is_full`
- `Dispatcher::reset` / `SessionTable` per pipe worker
- `[composing] space = "convert"` (idle unbound)

## Exclusions

- AI text, neural reranker, dictionary ranking of `変換昨日`
- VS Code crash / write journal
- Installed IME / live `ITfRange`

## Dependency graph

TSF OnKeyDown → pipe SendKey → per-connection Dispatcher → keymap lookup → Convert or idle_space_commit → OutputBuf commit/preedit. History session IDs are process-wide; session tables are not.
