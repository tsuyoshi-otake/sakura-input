# VS Code crash diagnostics investigation (Issue #35)

## Decision boundary

Issue #35 is a diagnostic-only phase and is separate from Issue #34. The current
TSF write state machine is not changed by this phase. In particular, the edit
session sync/async fallback, write-coordinator epochs and ownership, focus and
deactivation invalidation, composition termination, document commit/rollback,
retry/queue semantics, and the Electron `GetSelection` path remain the same.

Static inspection found that the existing implementation already has bounded
callback validation and fail-closed projection handling. That is not evidence
that it is the source of the VS Code crash. No natural `Code.exe` crash dump or
stack attribution was available at the start of this phase.

## Existing WER boundary

The existing Sakura policy covers only Sakura-owned executables and stores
minidumps under `%LOCALAPPDATA%\\SakuraInput\\dumps`. It does not configure
`LocalDumps\\Code.exe`. Issue #35 adds an explicit, administrator-only command
for that per-application key; the default installer and normal IME registration
remain unchanged.

The Code.exe policy uses `DumpType=1` and `DumpCount=5`, with
`REG_EXPAND_SZ=%LOCALAPPDATA%\\SakuraInput\\dumps\\vscode`. Because the key is
under HKLM, it affects all users on the machine and same-name `Code.exe`
processes. Dumps can contain process memory and input text. There is no upload
or automatic sharing. Removing the policy and deleting dump files are separate
operations.

## Evidence classification

The implementation and operator documentation distinguish these facts:

1. the HKLM registry values were written and ownership was recorded;
2. WER generated a dump for a generic test process;
3. WER captured a real VS Code `Code.exe` crash.

Only the first two can be established by automated tests in this repository.
Until a natural or safe, non-data-loss VS Code crash is captured and attributed,
Code.exe capture remains **Unverified**. No deliberate crash injection is used.

## Baseline

Before source implementation, `rtk proxy cargo test -p sakura-tsf -- --nocapture`
reported 120 passed and 14 failures. The exact test names and panic signatures
are recorded in
`.codex/goal-loop/issue-34-vscode-diagnostics/baseline-sakura-tsf-20260811.md`.
Those failures are handshake/IPC-link assertions and are not treated as crash
evidence.

## Diagnostic ring boundary

The TSF metadata ring is fixed-capacity and opt-in. It records schema/build
identity, monotonic sequence, thread/context/focus/document/composition IDs,
ticket/request metadata, path and terminal outcome, classified error codes, and
lifecycle events. It never records preedit, commit, surrounding/document text,
keys, clipboard, reading, candidates, dictionaries, history, or hashes derived
from text. Event emission uses atomics only: no heap allocation, blocking lock,
file I/O, registry I/O, or network operation occurs on the key path.

The ring lives in TSF process memory. A minidump may omit or truncate it; this
repository does not claim that a WER minidump will contain a readable ring. The
last valid event can be correlated with a native stack only when the dump and
symbols actually include the relevant memory.
