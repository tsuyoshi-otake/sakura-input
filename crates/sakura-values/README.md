# sakura-values

Shared bounded values used across Sakura Input crates. This crate is a dependency leaf.

## Purpose

Define shared value types and capacity bounds without coupling consumers to a protocol codec.

## Owns

Fixed-capacity strings/vectors and overflow; input, appearance, scope, and AI-text values; shared value/container capacities.

## Must not own

Wire codecs, `Reader`, `Sink`, protocol versioning, message headers, I/O, Windows APIs, conversion, keymaps, or runtime orchestration.

## Allowed dependencies

None; Rust `std` only.

## Allowed consumers

All workspace crates may depend on this leaf crate.

## Public API budget

At most 25 public types. Public functions construct, inspect, or convert values only. I/O entry points: zero.

## Issue types

Shared value representation, bounded-capacity behavior, and cross-crate value invariants. Protocol byte-layout work belongs to `sakura-proto`.

## Test commands

Run `cargo test -p sakura-values --locked`, R12, and the `sakura-proto` v22 golden/roundtrip suites through `ci/run-test-quiet.ps1`.
