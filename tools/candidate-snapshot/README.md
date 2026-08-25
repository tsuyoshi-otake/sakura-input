# Sakura Input candidate snapshot evaluator

This directory is an independent, non-shipping evaluator for Issue #93. It
captures whole-reading N-best candidates through the `sakura-core` dictionary
API and preserves the converter's existing order. It does not implement a
ranker and does not alter engine or dictionary behavior.

The directory is intentionally a nested Cargo workspace. Copy the complete
directory into a v1.0.5 or v1.0.23 worktree. The relative
`../../crates/sakura-core` dependency then resolves to that worktree's core.
Cargo records path-package versions in `Cargo.lock`, so a historical worktree
must regenerate only that worktree-local lock offline before using `--locked`.
The registry dependencies remain exact-pinned by `Cargo.toml`; compare them to
the committed lock when auditing the capture. Build legacy artifacts without
features; build a v1.0.23/current artifact with `modern` when the checked-out
core exposes the newer metadata APIs.

```powershell
# v1.0.5-compatible artifact
cargo generate-lockfile --offline `
  --manifest-path tools/candidate-snapshot/Cargo.toml
cargo build --locked --offline --release `
  --manifest-path tools/candidate-snapshot/Cargo.toml

# v1.0.23/current artifact with origin/path/input-support metadata
cargo generate-lockfile --offline `
  --manifest-path tools/candidate-snapshot/Cargo.toml
cargo build --locked --offline --release --features modern `
  --manifest-path tools/candidate-snapshot/Cargo.toml
```

The input is the committed Issue #93 JSON fixture. Only `case_id` and
`reading` are passed into conversion; expected surfaces, roles, references,
and rationales stay outside the engine boundary. The evaluator refuses a
fixture candidate limit other than the production core limit (18).

```powershell
candidate-snapshot.exe `
  --dictionary C:\artifacts\system.dic `
  --fixture eval\corpus\behavioral\ranking-comparison-issue93\fixture.json `
  --git 0123456789abcdef0123456789abcdef01234567 `
  --variant v1.0.23 `
  --source-diff-sha256 clean `
  --it-bias on `
  --output after.json
```

`--it-bias on|off` is always required. `off` disables the options-controlled
IT bias values; it is not an assertion that a compound-only bonus patch was
applied. Exact ablation identity belongs in `--variant` and
`--source-diff-sha256`; the latter accepts either `clean` or a 64-hex digest
of the exact source diff applied to the worktree.

Each output self-describes the Git revision, evaluator executable SHA-256,
dictionary SHA-256, fixture SHA-256, source-diff identity, variant, and
canonical options hash. Candidate records contain ordered surfaces, final
path cost, segment ranges/IDs/flags, terminal/truncation state, and common
system-entry provenance. Base/local cost and ranking-pass metadata are
explicitly `null`/`unsupported`; the public API does not expose them. Legacy
artifacts also emit newer origin/path-evidence fields as `null`/unsupported.

The fixed registry dependencies are `serde 1.0.219`, `serde_json 1.0.140`,
and `sha2 0.10.9`. No dependency is added to a shipping Sakura crate.
The committed lockfile, including transitive registry packages, was audited
against crates.io on 2026-08-25; its newest selected release was 34 days old,
so every package cleared the repository's seven-day quarantine.
