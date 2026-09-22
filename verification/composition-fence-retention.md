# Historical composition retention — #259

Verified 2026-09-22 against the changes based on `4f28f14`.

## Contract and ownership

`CompositionFence` owns both live composition claims and pending recovery
Space latches. Previously its `HashSet` retained every distinct historical
host until another composition or recovery Space for that host. This is
unbounded reachable retention; dropping the owner frees it.

Keep the most recently armed **1,024 distinct host names** in a `VecDeque`.
Rearming refreshes recency without banking another Space. Evict before
insertion, avoiding a transient capacity increase. At saturation the oldest
host loses its pending absorption: this is an explicit behavior change,
not an equivalent refactor. There is no time expiry. Live claims are stored
separately and never evicted by the historical-name limit.

Pending lookup/update is O(B), B <= 1,024, rather than O(total historical
names); storage is O(B * name length). Product `Session::process_name` is
bounded to 128 UTF-8 bytes. Live counts remain hash-based, and disconnected
claim inspection remains scoped to the queried host. The three legacy
unowned claim methods had only unit-test callers; those tests now exercise
the production connection/session-owned API.

## Executable evidence

| Verify | Expect | Observed |
|---|---|---|
| `historical_host_names_are_bounded_and_newest_recovery_is_one_shot` before implementation | Regression detects old retention | Failed: 10,000 retained versus 1,024 expected |
| Same test after implementation | Latest 1,024 recover once; no active claims | Passed |
| `composition_fence::tests` | Case folding, peer protection, rearm, no claim, wrong/late finalization, concurrent owners | All 16 passed, including 6 new tests |
| Engine lib + `space_key_dispatch_pipe` + `zero_alloc_dispatch`, `dev-fixtures` | Real pipe and hot-path behavior preserved | 562 passed, 0 failed, 2 ignored |
| Counting allocator using the actual source and owned-claim methods | Flat retention at 10k–100k names; zero after drop | Table below |
| Workspace Clippy, all targets, CI features + `context-research`, `-D warnings` | No compiler/lint regressions | Passed |
| `ci/check-process-clean.ps1` and PID/path inventory | No owned runner or test runtime left | Passed; installed resident processes preserved |

The release-mode allocation probe includes `composition_fence.rs` directly.
It uses a counting system allocator, warms stdout before recording a baseline,
cycles one name 10,000 times, consumes that latch, then cycles 100,000 unique
15-byte names. It samples after dropping each temporary name. All old names
are checked for no live claim; exactly the latest 1,024 latches consume once.

| Phase | Retained Rust allocation bytes above baseline |
|---|---:|
| Same name, 10,000 cycles | 464 |
| Distinct names, every 10,000 from 10,000 through 100,000 | 32,136 |
| All retained latches consumed | 16,776 (reusable container capacity) |
| Fence dropped | 0 |

The previous unbounded implementation retained 428,460 additional bytes after
10,000 distinct names. The new probe measured an average 308 ns per saturated
miss over 100,000 calls on this machine in release mode; this is a microbenchmark,
not an end-to-end key-latency guarantee. Raw probes and logs are retained locally
under `~/tmp/sakura-memory-fix-20260922/`; the before-fix audit is under
`~/tmp/sakura-memory-audit-205-20260922/`.

Tests were run through `ci/run-test-quiet.ps1` with TKW timeouts. The targeted
Cargo command is `cargo test --locked --offline -p sakura-engine --features
dev-fixtures --lib --test space_key_dispatch_pipe --test zero_alloc_dispatch
-- --test-threads=1`. The integrated #259/#261 workspace test with
`sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture,sakura-engine/context-research`
passed 1,981 tests, failed 0, and ignored 97 across 114 reported suites.
Ignored dictionary, long-running, benchmark and native-desktop tests were not
silently counted as executed. No installed binary or user profile was updated.

## Separate renderer observation — #260

The installed `2.0.5-b66781491cd31dad` resident had previously increased by
5.34 MiB, with handles/GDI/USER also increasing. Candidate-only stress
(6,600 show/paint/hide cycles) did not reproduce that resource growth.

An additional owned-process test used the same installed payload, fresh
isolated profile, private engine/renderer pipes, and the actual Pad trigger
and close messages. No user Pad content was loaded or changed.

| Pad open/close cycles | Private bytes | Handles | GDI | USER |
|---:|---:|---:|---:|---:|
| 0 | 5,402,624 | 118 | 0 | 8 |
| 1 | 10,010,624 | 285 | 23 | 44 |
| 11 | 10,014,720 | 285 | 23 | 46 |
| 51 | 10,006,528 | 285 | 23 | 46 |
| 101 | 10,022,912 | 285 | 23 | 46 |
| 201 | 10,067,968 | 285 | 23 | 48 |
| 201, after three seconds idle | 10,067,968 | 285 | 23 | 44 |

Pad initialization accounts for a repeatable 4,608,000-byte one-time jump in
this workload. The app stores one `PadWindow`; close hides it, and owner drop
destroys it. Handles/GDI stayed fixed after initialization; transient USER
objects returned to the initial Pad count. Private bytes ended 57,344 bytes
above the first-open sample, so this test does **not** prove a memory plateau
or attribute the original resident observation. #260 remains open for
matched longer workloads and allocation-stack attribution. No speculative
renderer fix was made. The probe finished in 21 seconds, joined its relay,
shut its engine down gracefully, and left no owned processes.

## Scope / IRV

- **A — Required scope:** fence state/API/tests, dispatcher/session lifecycle
  callers, the #102 Space contract, private-pipe Space tests, and allocation
  measurement. Production change is confined to `composition_fence.rs`.
- **B — Expansion:** the separate resident observation required Pad creation,
  close/drop ownership and an isolated installed-payload probe. Its uncertainty
  is tracked in #260 rather than changing the renderer spec.
- **C — Architecture:** state remains under the existing fence mutex; no new
  crate, dependency, wire field or persistence format. Tests now use the same
  owner/session finalization path as production.
- **D — Future IRV:** the bound, eviction behavior and counterexamples are
  colocated with the owner. Future Space changes must preserve this saturation
  contract; they no longer need to interpret obsolete unowned claim methods.
  This does not establish whole-process or whole-repository leak freedom.
