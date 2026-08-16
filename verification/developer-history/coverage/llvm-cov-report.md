# llvm-cov / line coverage — developer-history

## Label honesty

This file is **not** C2. Atomic-condition polarity coverage of the independent
oracle lives in `coverage/c2-report.md`.

## This run

| Measurement | Status |
|---|---|
| `cargo llvm-cov` on `sakura-engine` | **Not run** — crate is `#![cfg(windows)]` on this Linux agent |
| Host oracle PBT / C2 generator | Executed via `scripts/generate-developer-history-pbt-artifacts.rs` |

## Windows follow-up commands

```powershell
cargo llvm-cov --package sakura-engine --lib --tests -- \
  --ignored false
# Focus regions of interest when reviewing HTML:
# - server::Shared::runtime_services history branch
# - Dispatcher::set_input_history
# - InputHistoryService::record_key exclusion arms
# - sakura-settings cli await_developer_history_terminal
```

Do not paste llvm line % into the C2 table.
