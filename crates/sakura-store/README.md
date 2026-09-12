# sakura-store

## Purpose

Defines the durable learning and developer-history formats once for engine, settings, and maintenance tools.

## Owns

- Namespaced `input_history` and `learning` record layouts, versions, bounds, and pure codecs.
- `input_history::persistence`: the 30-day retention rule, 64 MiB hard limit, pure age/order selection, canonical `input.bin` placement, transaction path, and exact `ReplaceFileW` replacement primitive.
- The `Sealer` boundary and Windows DPAPI implementation added in Phase 2.2c.

## Must not own

- Writer threads, queues, services, engine lifecycle, input-scope classification, or CLI presentation.
- Engine identity or runtime IDs beyond storing caller-provided values.
- Protocol messages, IPC, ranking, conversion, or learning decisions.

## Allowed dependencies

`sakura-values` is the only unconditional dependency. Target-specific `windows` bindings are limited to `Win32_Foundation` and `Win32_Storage_FileSystem` for `input_history::persistence`; Phase 2.2c may add the reviewed `dpapi` feature only in `crypto`.

## Allowed consumers

`sakura-engine`, `sakura-settings`, and repository maintenance tools.

## Public API budget

At most 12 public types and 23 public functions. The Phase 2.2b allowance is limited to six retention/size/path/replacement functions; two additional functions are reserved for the Phase 2.2c `Sealer::seal`/`open` contract. Public modules and names remain domain-specific; no generic dumping-ground API.

## Invariants

Existing format versions, discriminants, little-endian layout, UTF-8 validation, checksums, size bounds, inclusive retention cutoff, transaction path, replacement ordering, and fail-closed decoding remain byte-compatible. Sensitive scope classification stays in engine; store records only the already-decided durable class. Transaction-marker admission, the writer actor, and protected-frame preparation stay in engine.

## Verification

Run store unit/golden/persistence tests, provenance-fixture decode-to-encode identity, dependency rule R13, DeveloperHistory correspondence, and the existing engine/settings tests through the repository quiet-test wrapper.
