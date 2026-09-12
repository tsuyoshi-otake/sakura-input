# sakura-store

## Purpose

Defines the durable learning and developer-history formats once for engine, settings, and maintenance tools.

## Owns

- Namespaced `input_history` and `learning` record layouts, versions, bounds, and pure codecs.
- `learning::LearningLog`: synchronous durable learning transactions, including atomic publication, recovery, exact operation ordering, and receipt-bearing terminal outcomes.
- `input_history::persistence`: the 30-day retention rule, 64 MiB hard limit, pure age/order selection, canonical `input.bin` placement, transaction path, and exact `ReplaceFileW` replacement primitive.
- `crypto`: the `Sealer` boundary and current-user Windows DPAPI implementation.

## Must not own

- Asynchronous writer threads, queues, `InputHistoryService`, engine lifecycle, input-scope classification, or CLI presentation.
- Learning runtime indexes, prediction history, sequence allocation, `Busy` admission, counters, mutexes, maintenance scheduling, and scoring.
- Engine identity or runtime IDs beyond storing caller-provided values.
- Protocol messages, IPC, ranking, conversion, or learning decisions.

## Allowed dependencies

`sakura-values` is the only unconditional dependency. Target-specific `windows` bindings are limited to `Win32_Foundation` and `Win32_Storage_FileSystem` for `input_history::persistence`, plus `Win32_Security_Cryptography` and `Win32_System_Memory` behind the default-on `dpapi` feature for `crypto`.

## Allowed consumers

`sakura-engine`, `sakura-settings`, and repository maintenance tools.

## Public API budget

The final learning-store boundary allows at most 20 public types and 31 public functions. The first pure learning-codec extraction reaches 14/31 by adding `LearningRecord`, `LearningSnapshot`, temporary `DecodedRecord`, and eight low-level functions to the existing 11/23 surface. The transaction move replaces `DecodedRecord` with `LearningLog`, `ReplayView`, `ReplayEvent`, `OperationReceipt`, `LearningLogError`, `LogMaintenance`, and `LogForget`; makes all eight low-level functions crate-private; and exposes exactly `memory`, `append`, `append_repair_suppress`, `read_snapshot`, `open`, `maintain`, `forget_exact`, and `clear`. That produces 20/31. TSV formatting belongs to settings and is excluded. The Phase 2.2b allowance remains limited to six retention/size/path/replacement functions; the `Sealer::seal`/`open` contract uses the two reserved Phase 2.2c functions. Public modules and names remain domain-specific; no generic dumping-ground API.

## Invariants

Existing format versions, discriminants, little-endian layout, UTF-8 validation, checksums, size bounds, inclusive retention cutoff, transaction path, replacement ordering, and fail-closed decoding remain byte-compatible. DPAPI uses current-user protection with flags 0 and no optional entropy. Sensitive scope classification stays in engine; store records only the already-decided durable class. Transaction-marker admission, the writer actor, protected-frame preparation, and encrypted frame layout stay in engine.

Each synchronous `LearningLog` transaction that can publish, recover, clean up, or change maintenance accounting has a receipt-bearing terminal result: success returns an `OperationReceipt`, and `LearningLogError` carries a receipt on error, including a zero-delta receipt when no maintenance failure was counted. The receipt reports the exact maintenance-failure counter delta so the engine can apply it once on both success and error. A filtered canonical log is committed and `Removed` even when a later platform report or cleanup fails; that failure remains observable in the receipt. Recovery that leaves the old runtime authoritative returns an error with its receipt. These outcomes must not be inferred again from paths after the operation returns.

Per-operation ordering remains the current ordering. `open` publishes a required format upgrade before runtime replay and truncates a recoverable tail after replay. Compaction and exact forget prepare their replacement runtime before publication. `clear` publishes and reopens the empty canonical log, attempts the existing ignored-backup removal, and only then prepares the empty runtime. Structural extraction must not move these callbacks or add new replay error points.

## Verification

Run store unit/golden/persistence tests, provenance-fixture decode-to-encode identity, dependency rule R13, DeveloperHistory correspondence, and the existing engine/settings tests through the repository quiet-test wrapper.

For crypto, run store tests with and without default features. The default Windows suite exercises current-user DPAPI roundtrip and malformed-ciphertext rejection; the feature-off suite uses a test-only `PlainSealer`. Cross-target compilation is not runtime test evidence. Read back the synthetic pre-extraction fixture under its original Windows user and verify its hash remains unchanged; never use personal history as a test fixture. Framing and writer lifecycle remain engine-owned.

For learning, `tests/learning_codec.rs` decodes the committed pre-extraction writer fixture and re-encodes its exact bytes. Engine learning regressions still cover upgrades, corruption, bounded reads, compaction, exact-forget fault recovery, clear and repair suppression. Settings owns TSV formatting and its exact escaping test. Pure codec extraction does not move the synchronous file owner or runtime replay; those remain a later atomic transaction step.
This codec extraction includes `crates/sakura-store/tests/fixtures/learning-v3-pre-extraction.hex` and `crates/sakura-store/tests/fixtures/learning-v3-pre-extraction.expected.tsv`. They preserve an actual synthetic `learning.bin` written locally by `bfa5eeb8a9a564f5d3401e7eec9321a2bca105d1` (file SHA-256 `7da8cb554c501bffe551e1f94a7501f1bb94e57cae9664a09d81494d3a0bc249`) and its exact TSV (SHA-256 `7fdd5700feebc5a08ff7ce0caa93ba065fcbe4e2225312fda01fce67956288b8`). That local writer revision is not reachable from the remote history; its `crates/sakura-engine/src/learning.rs` blob is `9abadc46f7885103c9a664811d2f2030dad8bef1`, identical to the remotely reachable PR #190 source revision `ca19035d4784f4ec2cd91421933acc8d8d4a4220`. Record `bfa5eeb8...` as the actual execution provenance and use the full `ca19035d...` revision as the reviewable source provenance. Decode the committed hex fixture, verify read and decode-to-encode identity, and compare exported TSV byte-for-byte; do not require the unreachable commit during CI.
