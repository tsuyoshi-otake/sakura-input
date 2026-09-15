# dictc

## Purpose

Thin command-line binaries for the offline dictionary build. `scripts/build-dictionary.ps1` runs them.

## Owns

- Argument parsing, file I/O, and exit codes for these binaries: `dictc`, `mozc-trim`, `inflection-expand`, `glossary-import`, `category-split`, `llm-detail-targets`, `llm-detail-drafts`, `llm-detail-promote`, and `context-dataset`.
- Also, until they move to `tools/`, the evaluation binaries `corpus-eval`, `neural-eval`, and `compound-homophone-scan`.

## Must not own

- Compilation, parsing, overlay, or detail rules. These belong to `dictc-core`.
- The image format, which belongs to `sakura-core`.
- Anything linked into a runtime crate.

## Allowed dependencies

- Normal: `dictc-core`, `sakura-core`, `sakura-proto`, `sakura-rerank-proto`, `serde`, `serde_json`, `sha2`, `unicode-normalization`.
- Development and build: none.

## Allowed consumers

- No crate depends on this package.
- `scripts/build-dictionary.ps1` and developers run its binaries with `cargo run -p dictc --bin <name>`.

## Public API budget

- Zero library items. This package has binaries only.

## Issue types

| Change | Where to look |
| --- | --- |
| Command-line flag or output path | The matching `src/bin/*.rs` file, or `src/main.rs` for the compiler |
| Build pipeline stage order | `scripts/build-dictionary.ps1` |
| Rule behind a stage | `dictc-core` |

## Test commands

Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name 'dictc tests' -Command { cargo test -p dictc --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```
