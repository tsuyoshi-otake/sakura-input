# Sakura Input repository guide

This is the compact entry point. Read `AGENTS.md` and `README.md`, then load
only the architecture, owner decision, verified rule, or dated history relevant
to the current issue.

## Architecture navigation

Start with `docs/architecture/README.md`. It distinguishes current evidence
from the planned target; an analysis document does not mean the target exists.

| Issue type | CURRENT implementation paths | Target architecture | Status |
| --- | --- | --- | --- |
| Core conversion and wire types | `crates/sakura-core`, `crates/sakura-proto` | `docs/architecture/analysis/core-proto.md` | FUTURE target documented |
| Engine state, ranking, history | `crates/sakura-engine` | `docs/architecture/analysis/engine.md` | FUTURE target documented |
| TSF/COM and edit sessions | `crates/sakura-tsf` | `docs/architecture/analysis/tsf.md` | FUTURE target documented |
| Renderer and settings | `crates/sakura-renderer`, `crates/sakura-settings` | `docs/architecture/analysis/renderer-settings.md` | FUTURE target documented |
| Dictionary, contracts, tools | `crates/dictc`, `tools`, `data` | `docs/architecture/analysis/contracts-dictc-tools.md` | FUTURE target documented |
| Documentation, verification, CI | `docs`, `verification`, `ci`, `.github/workflows` | `docs/architecture/analysis/docs-verification-ci.md` | FUTURE target documented |
| Cross-layer refactor | all rows above | `docs/architecture/agent-refactor-plan.md` | FUTURE phased plan |

Use `rg` to locate the relevant business term, entry point, callers, invariants,
side effects, and tests before editing. Architecture documents guide boundaries;
CURRENT code and tests remain authoritative until a planned phase lands.

## Owner decisions

Read only the decision files relevant to the task:

| Issue or policy | Required decision |
| --- | --- |
| Test output / Issue #111 | `docs/decisions/test-output.md` |
| Product direction | `docs/decisions/product-direction.md` |
| Release signing | `docs/decisions/release-signing.md` |
| Update trust state / Issue #150 | `docs/decisions/update-trust-state.md` |
| AI text / Issue #58 | `docs/decisions/issue-58-ai-text.md` |
| Mode indicator / Issue #26 | `docs/decisions/issue-26-mode-indicator.md` |
| Candidate popup / Issue #27 | `docs/decisions/issue-27-candidate-popup.md` |
| Candidate details / Issue #28 | `docs/decisions/issue-28-candidate-details.md` |
| Reviewed dictionary details / Issue #30 | `docs/decisions/issue-30-dictionary-details.md` |
| Sakura reranker / Issue #32 | `docs/decisions/issue-32-reranker.md` |
| IME key behavior | `docs/decisions/input-behavior.md` |
| Developer input history | `docs/decisions/developer-history.md` |

Consult `.claude/memory/rules.md` for verified rules relevant to the code or
command being changed. These rules include machine-specific Cargo constraints,
Windows process behavior, performance evidence, and state-machine lessons.

## Stable product floor

Sakura Input is an IT-engineer-first Windows Japanese IME. Ordinary Japanese
conversion remains a quality floor. Preserve technical spelling and case,
identifiers, paths, URLs, commands, Markdown, and Japanese/ASCII boundaries
without degrading general Japanese candidates.

## VS Code crash investigation

- The 2026-08-02 investigation is unfinished dated evidence, not a confirmed cause or completed fix.
- Read the exact extracted handoff at `docs/history/issues/vscode-crash-investigation-20260802.md` before resuming it.
- Preserve the Electron `GetSelection` path; begin renewed work with current code inspection and logs or dumps.

## Historical provenance

`docs/history/CLAUDE.pre-154.md` preserves the complete pre-compaction handoff.
It is optional dated context, not required reading for unrelated work. Use
`docs/history/CLAUDE-section-map.md` to locate every original section. Historical
test totals, artifacts, installed build IDs, and agent/runtime observations do
not establish current verification.
