# Main required checks

This change has one purpose: protect `main` with the stable checks that Phase 0.6 requires.

`main.json` is the versioned GitHub ruleset definition. It is active only for `refs/heads/main`, has no bypass actors, requires pull requests with zero approvals, blocks deletion and non-fast-forward updates, and uses strict required status checks. `crate-tests` is intentionally absent until Phase 7.8.

The three alias jobs in `ci.yml` expose `fmt`, `workspace-tests`, and `dll-size` while their work remains aggregated in `Build and test`. Each alias always runs after that job and fails unless its upstream result is `success`.

Committing this definition does not apply it to GitHub. After a commit has successfully emitted all six exact check names, a repository administrator should apply it with authenticated GitHub CLI:

```powershell
gh api --method POST repos/tsuyoshi-otake/sakura-input/rulesets --input ci/rulesets/main.json
```

Before applying, confirm that `fmt`, `dependency-rules`, `workspace-tests`, `irv-regression`, `dll-size`, and `Dependency policy` all completed successfully on the same commit. If `main-protection` already exists, update that ruleset instead of creating a duplicate.
