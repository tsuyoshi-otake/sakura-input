# rules.md preservation and applicability map

Commit `eedded5` first ran `git mv .claude/memory/rules.md docs/history/rules.pre-154.md`,
preserving the original 45,652 CRLF bytes and
SHA-256 `e4016aa130b536fc168c8b12317fbca1ca4277f90afed9d8f3c5f769de74a687`.
The compact entry point and exact topic extracts follow that move.

| Original section | Exact active destination | Applies when |
| --- | --- | --- |
| `Performance` | `docs/rules/performance.md` | Performance code, benchmarks, or performance claims |
| `CI and verification` | `docs/rules/ci-verification.md` | CI/workflow/test evidence, sandboxing, persistence, diagnostics, error semantics |
| `Windows specifics` | `docs/rules/windows.md` | Windows processes, UIA, pipes, installer, watchdog, deadlines, DPI |
| `This machine` | `docs/rules/this-machine.md` | Cargo, PowerShell, shell edits, release paths, popup capture on this host |
| `TSF re-entrancy safety` | `docs/rules/tsf-reentrancy.md` | TSF/COM callback authority, teardown, cleanup, borrow refusal |
| `Overflow-hazard test construction (dictionaries and prediction)` | `docs/rules/overflow-packaging.md` | Limits, overflow fixtures, packaging regexes, hashes/line endings |
| `Session state that describes a composition` and later findings in that section | `docs/rules/session-and-later-findings.md` | Composition/keymap plus the additional triggers enumerated in the entry table |

No rule is demoted to optional history. The moved snapshot preserves bytes and
chronology; the exact topic extracts remain the active source selected through
the mandatory trigger table.
