# Atomic-condition (C2) coverage — Space-key-dispatch oracle

Scope: `crates/sakura-engine/src/space_key_dispatch_oracle.rs` predicates in `atomic_conditions`.
This is atomic-condition polarity coverage of the independent oracle, not MC/DC of `dispatch.rs`.

| condition | false | true |
|---|---|---|
| `ATOM-IDLE` | true | true |
| `ATOM-COMPOSING` | true | true |
| `ATOM-CONVERTING` | true | true |
| `ATOM-LIVE` | true | true |
| `ATOM-PEER-CONVERTING` | true | true |
| `ATOM-SPACE-EVENT` | true | true |
| `ATOM-DUAL-TARGET` | true | true |
| `ATOM-SPACE-EXHAUSTED` | true | true |
| `ATOM-TYPE-EVENT` | true | true |
| `ATOM-CRASH-EVENT` | true | true |
| `ATOM-PENDING-TEARDOWN` | true | true |

Covered polarities: 22/22 (100%). Seed `0x5350_4143_4520_0816`. Cases: 512.
