# Repository agent entry point

Read `CLAUDE.md` before repository work.

- Preserve existing and concurrent work. Never reset, clean, stash, or
  overwrite edits and untracked files you do not own.
- Use `rg` for discovery and `apply_patch` for edits. Use `gh` for GitHub
  operations; do not use GitHub connectors.
- Never create a public repository or hard-code credentials. Put temporary
  files under `~/tmp/`, never directly under `~/`.
- Route ordinary Cargo tests through `ci/run-test-quiet.ps1`; retain the
  original exit status and prove repository test processes exited afterward.
- Keep refactor dependencies acyclic, state transitions terminal, and hot-path
  work bounded. Verify changed behavior and report residual risk.
