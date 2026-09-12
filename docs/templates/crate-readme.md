# <crate name>

## Purpose

Describe the one business purpose and the invariant this crate protects.

## Owns

List the state, formats, resources, and terminal outcomes exclusively owned here.

## Must not own

Name excluded responsibilities and their actual owning crates. Change this charter
in a separate PR before adding an excluded responsibility.

## Allowed dependencies

List exact direct normal Cargo package names, including optional and target-specific
dependencies and their feature/target conditions. Write `None` for a std-only leaf.
List development and build dependencies separately within this section.

## Allowed consumers

Name permitted production consumers and development-only consumers separately.

## Public API budget

State a numeric public item/type budget and permitted API operations. Explain each
public entry point; do not expose implementation modules merely to shorten imports.

## Issue types

Map representative changes to the entry point, invariant, external boundary, tests,
and canonical contract. Link the relevant IRV benchmark and its complete read set.

## Test commands

Run from the repository root using PowerShell 7:

```powershell
./ci/run-test-quiet.ps1 -Name '<crate> tests' -Command { cargo test -p <crate> --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

Document feature-specific, integration, ignored desktop, and formal checks when
applicable. An ignored test is not a passing test; include the prerequisite and
the command that actually runs it.
