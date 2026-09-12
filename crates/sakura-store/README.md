# sakura-store

## Purpose

Defines the durable learning and developer-history formats once for engine, settings, and maintenance tools.

## Owns

- Namespaced `input_history` and `learning` record layouts, versions, bounds, and pure codecs.
- `input_history::persistence`: the 30-day retention rule, 64 MiB hard limit, pure age/order selection, canonical `input.bin` placement, transaction path, and exact `ReplaceFileW` replacement primitive.
- `crypto`: the `Sealer` boundary and current-user Windows DPAPI implementation.

## Must not own

- Writer threads, queues, services, engine lifecycle, input-scope classification, or CLI presentation.
- Engine identity or runtime IDs beyond storing caller-provided values.
- Protocol messages, IPC, ranking, conversion, or learning decisions.

## Allowed dependencies

`sakura-values` is the only unconditional dependency. Target-specific `windows` bindings are limited to `Win32_Foundation` and `Win32_Storage_FileSystem` for `input_history::persistence`, plus `Win32_Security_Cryptography` and `Win32_System_Memory` behind the default-on `dpapi` feature for `crypto`.

## Allowed consumers

`sakura-engine`, `sakura-settings`, and repository maintenance tools.

## Public API budget

At most 12 public types and 23 public functions. The Phase 2.2b allowance is limited to six retention/size/path/replacement functions; the `Sealer::seal`/`open` contract uses the two reserved Phase 2.2c functions. Public modules and names remain domain-specific; no generic dumping-ground API.

## Invariants

Existing format versions, discriminants, little-endian layout, UTF-8 validation, checksums, size bounds, inclusive retention cutoff, transaction path, replacement ordering, and fail-closed decoding remain byte-compatible. DPAPI uses current-user protection with flags 0 and no optional entropy. Sensitive scope classification stays in engine; store records only the already-decided durable class. Transaction-marker admission, the writer actor, protected-frame preparation, and encrypted frame layout stay in engine.

## Verification

Run store unit/golden/persistence tests, provenance-fixture decode-to-encode identity, dependency rule R13, DeveloperHistory correspondence, and the existing engine/settings tests through the repository quiet-test wrapper.

For crypto, run store tests with and without default features. The default Windows suite exercises current-user DPAPI roundtrip and malformed-ciphertext rejection; the feature-off suite uses a test-only `PlainSealer`. Cross-target compilation is not runtime test evidence. Read back the synthetic pre-extraction fixture under its original Windows user and verify its hash remains unchanged; never use personal history as a test fixture. Framing and writer lifecycle remain engine-owned.
