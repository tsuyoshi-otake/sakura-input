# sakura-store

## Purpose

Defines the durable learning and developer-history formats once for engine, settings, and maintenance tools.

## Owns

- Namespaced `input_history` and `learning` record layouts, versions, bounds, and pure codecs.
- Persistence rules, atomic file replacement, bounded retention, and compaction pure functions added in their planned phases.
- The `Sealer` boundary and Windows DPAPI implementation added in Phase 2.2c.

## Must not own

- Writer threads, queues, services, engine lifecycle, input-scope classification, or CLI presentation.
- Engine identity or runtime IDs beyond storing caller-provided values.
- Protocol messages, IPC, ranking, conversion, or learning decisions.

## Allowed dependencies

`sakura-values` is the only unconditional dependency. `windows` is allowed only behind the `dpapi` feature and only in `crypto`.

## Allowed consumers

`sakura-engine`, `sakura-settings`, and repository maintenance tools.

## Public API budget

At most 12 public types and 15 public functions. Public modules and names remain domain-specific; no generic dumping-ground API.

## Invariants

Existing format versions, discriminants, little-endian layout, UTF-8 validation, checksums, size bounds, and fail-closed decoding remain byte-compatible. Sensitive scope classification stays in engine; store records only the already-decided durable class.

## Verification

Run store unit/golden tests, provenance-fixture decode-to-encode identity, dependency rule R13, and the existing engine/settings tests through the repository quiet-test wrapper.
