# Mutation report — Space-key-dispatch oracle

Tool: `cargo-mutants 27.1.0`
Date: 2026-08-16
Revision: `f26191aa16a6b3569cdf004e4852650f7de1a17f`
Scope: `crates/sakura-engine/src/space_key_dispatch_oracle.rs` only
Config: `verification/space-key-dispatch/cargo-mutants.toml`
Test filter: `cargo test -p sakura-engine --lib -- space_key_dispatch_oracle`
Output: `verification/space-key-dispatch/mutants-out/mutants.out/`

Production `dispatch.rs` is **not** scored here: baseline dual-delivery
fails REQ-SPACE-03, so a green dry-run against production would require
weakening the oracle. That remains a product blocker, not a mutation skip.

## Dry-run (`--check`)

- Exit: 0
- Mutants found: 28
- Unmutated baseline: ok
- Check results: 27 succeeded, 1 unviable
- Log: `mutants-dry-run.log`

## Score (test run)

| | count |
|---|---:|
| Generated | 28 |
| Unviable | 1 |
| Viable denominator | 27 |
| Caught | 27 |
| Missed | 0 |
| Timeout | 0 |
| Raw score (caught / viable) | 27/27 = **100%** |
| Equivalent-adjusted | 100% (no survivors) |

## Canary

`replace no_dual_effect -> bool with true` — **caught** after adding
`canary_no_dual_effect_false_when_both_effects_observed` and asserting
`!no_dual_effect(&mutant)` in the adversarial unfenced check.

## Unviable

`replace space_effect -> SpaceEffect with Default::default()` — `SpaceEffect`
has no Default. Excluded from denominator.

## Adversarial oracle mutant (Phase 1)

`unfenced_mutant` in tests (idle insert even while peer converts) still
exhibits insert∧convert; the oracle rejects it. Kept outside production source.

## Survivors

None. Missed file is empty.
