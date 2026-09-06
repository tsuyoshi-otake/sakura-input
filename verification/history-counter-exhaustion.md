# H4/H6 live counter exhaustion (#132)

Baseline main f48d8f55fcab3f9abb210aa2a442cd8b217df1f1 (v1.0.35); patch base58b63ce (#131). **CONFIRMED** at injected live-counter boundaries, P2 developer-history opt-in. Real-world frequency is unmeasured. This is separate from #116 startup recovery and does not establish durable never-reused IDs.

Old fetch_add + 1 panics in debug at MAX after the atomic itself has already wrapped to zero. Three old-code tests reproduced session allocation, all four record variants and Clear epoch failures; each joins the actual synthetic writer before asserting failure. Release arithmetic was not executed as a before-test; its wrapping implication follows the old code, not a claimed release run.

Checked atomic fetch_update now allows the final MAX once, then returns failure without changing the atomic. Session allocation returns Option; dispatch maps unavailable to reserved zero. All content sinks reject session zero before encoding/enqueue, while the content-free engine marker may retain its existing zero session. Ordinary protocol session and text conversion continue. Exhausted sequence recording increments the existing persistence-failure counter and does not enqueue; unavailable-session content increments dropped_events. Exhausted Clear returns a content-free error without sending Clear or modifying bytes. Clear does not reset sequence/session counters.

## Executable criteria

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-engine --lib counter_exhaustion` with old production | 3 semantic failures, exit101 (`counter-before.log`); actual panic and zero counter observed |
| Same filter with final code | 7 passed, exit0 (`counter-targeted.log`) |
| Eight concurrent allocations at MAX-1 | Exactly one Some(MAX), seven None; repeated allocation None; counter stays MAX |
| Final sequence and Clear epoch | Final sequence stored once, final Clear accepted once, subsequent calls rejected; empty cleared store stays empty |
| All key/commit/AI sinks with unavailable session | Only content-free engine marker remains; three drops, no sequence allocation |
| Dispatcher unavailable history test | Synthetic normal input still yields かな, no content history |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Exit0 (`counter-clippy.log`) |
| `cargo test --locked --workspace` | 1,812 passed, 84 ignored, 0 failed (`workspace-counter.log`) |
| `cargo fmt --all -- --check`; `git diff --check`; repository process-clean script | Exit0, no owned runner survivors |

Final full checks follow final production/test edits. Independent adversarial review PASS; reviewer access was behaviorally read-only, not OS-enforced, and the parent owned all execution. Concurrent terminal stress directly covers session allocation; sequence and Clear use the same checked allocator but have sequential boundary tests, not separate concurrent stress claims. No dependency changes, installation, production store/pipe access or real TSF operations. Existing sensitivity/test_only gates remain before content admission. Stats and APIs expose rejection; Flush/Shutdown still describe preceding accepted writer work and do not pretend that rejected producer records were persisted.

Persistent store generation/high-watermarks after retention or restart, legacy ownership, all unrelated production counters, physical tests and power-loss recovery remain unfinished. A store containing MAX remains rejected on the next startup under #116. An unpersisted counter high-watermark may still be lost on restart; this patch guarantees no wrap within the live service, not globally unique IDs. Full H4/H6 and the program remain IN PROGRESS.
