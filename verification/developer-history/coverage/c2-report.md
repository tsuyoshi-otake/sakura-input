# Atomic-condition (C2) coverage — developer-history oracle

Scope: `crates/sakura-engine/src/developer_history_oracle.rs` predicates in `atomic_conditions`.
This is atomic-condition polarity coverage of the independent oracle, not MC/DC of the whole workspace and not line coverage claimed as C2.

| condition | false | true |
|---|---|---|
| `event_key_real` | true | true |
| `event_key_test_only` | true | true |
| `published_on` | true | true |
| `queue_full` | true | true |
| `request_after_publish` | true | true |
| `scope_normal` | true | true |
| `scope_sensitive` | true | true |
| `scope_unclassified` | true | true |
| `service_attached` | true | true |
| `setting_on` | true | true |

Covered polarities: 20/20 (100.0%). Seed `0x4448435332000001`. Cases: 1025.

Line/region coverage of production functions is **not** this table. See `llvm-cov-report.md` when measured on Windows.
