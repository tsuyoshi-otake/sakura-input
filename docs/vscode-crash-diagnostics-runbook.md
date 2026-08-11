# VS Code crash diagnostics runbook (Issue #35)

This is a diagnostic-only, operator-invoked phase. It is separate from Issue
#34 and does not change the TSF write state machine. No installer, update, IME
registration, or normal activation path enables this policy.

## Explicit WER policy

Run the following commands from an elevated administrator command prompt when
machine-wide Code.exe capture is explicitly approved:

```text
sakura_regtool.exe diagnostics vscode-dumps configure
sakura_regtool.exe diagnostics vscode-dumps status
sakura_regtool.exe diagnostics vscode-dumps remove
sakura_regtool.exe diagnostics vscode-dumps clear --confirm
```

`clear` is the only command that removes dump files, and `--confirm` is
mandatory. Running `clear` without that flag reports
`ConfirmationRequired`; `remove` disables the registry policy but deliberately
retains existing dumps. The commands never upload, share, or automatically
send a dump.

`configure` owns exactly this per-application key:

```text
HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\Code.exe
    DumpFolder REG_EXPAND_SZ %LOCALAPPDATA%\SakuraInput\dumps\vscode
    DumpType   REG_DWORD    1
    DumpCount  REG_DWORD    5
```

The policy is machine-wide. It can affect every Windows user on the machine
and every process whose executable name is `Code.exe`, not only the intended
VS Code installation. The dump directory is under the `%LOCALAPPDATA%` of the
account that expands the value. `configure` creates and probes that directory
for write access as the current account; this does not prove that every other
user or a different Code.exe installation can write it. `status` does not write
HKLM, but its current-user ACL probe may create the per-user dump directory.

The command refuses to overwrite any existing `Code.exe` values. Sakura keeps
an ownership marker in its own namespace:

```text
HKLM\SOFTWARE\SakuraInput\Diagnostics\VscodeDumps
```

Matching marker values and matching target values are required before
`remove` can delete anything. A matching value without Sakura's marker is
`UnmanagedConflict`. Modified marker/target values are also unmanaged. Unknown
values, user/third-party subkeys, the global `LocalDumps` policy, and unrelated
keys are preserved. A target or marker key is removed only after Sakura's
known values are deleted and the key is verified empty; a race that adds a
value or subkey therefore leaves the key in place.

The exit/report outcomes are explicit: `Configured`, `AlreadyConfigured`,
`UnmanagedConflict`, `Removed`, `NotConfigured`, `Cleared`,
`ConfirmationRequired`, `AccessDenied`, or `Failed`. `configure` and `remove`
fail with `AccessDenied` when the process cannot write HKLM. `sakura-reg` is the
single registry owner; `sakura-regtool` only parses the command and calls that
API.

## Privacy and evidence boundary

A Code.exe dump can contain process memory, open documents, preedit/commit
text, clipboard data, credentials, and other user input. Treat every dump as
sensitive local evidence. Sakura has no cloud upload, automatic sharing, or
background export path. Dump deletion is explicit and irreversible after the
validated absolute directory check; it is never an implicit consequence of
disabling the policy.

The three evidence levels must not be conflated:

1. The HKLM values were written and Sakura's marker proves ownership.
2. WER generated a dump for a generic test process.
3. WER captured and attributed a real VS Code `Code.exe` crash.

The repository can test the first level through registry abstractions and
policy tests. It does not claim the second level without a separate WER test,
and it does not claim the third level without a naturally occurring or safe,
non-data-loss VS Code crash. Do not inject a crash that could lose unsaved
data. The current Code.exe capture status is **Unverified** and no real dump
has been obtained.

## TSF metadata ring

The optional TSF diagnostic ring is enabled only by an explicit developer
environment opt-in before activation:

```text
SAKURA_TSF_DIAGNOSTICS=1
```

(`on` is also accepted.) It is a fixed-capacity 64-event ring in TSF process
memory. Each event contains only schema/build identity, monotonic sequence,
thread ID, context identity, focus generation, document revision, composition
generation, write ticket/request kind, sync/async path, terminal outcome,
classified error code, and lifecycle event. It never stores preedit, commit or
surrounding text, raw keys, clipboard, reading, candidate surfaces, user
dictionaries, developer history, or hashes/fingerprints derived from text.

The key path performs no heap allocation, lock wait, file I/O, registry I/O, or
network operation. Disabled recording is one atomic load. The ring is not
automatically exported. It is static TSF process memory, and a minidump may
omit or truncate it; there is no guarantee that a WER dump contains a readable
ring. Only when the dump actually includes the memory and symbols can the last
valid event be correlated with a native stack.

## Dump attribution procedure

For each candidate dump, preserve the original file and record:

1. crash time, PID, and the `Code.exe` executable path;
2. the Sakura TSF DLL module path, version, and SHA/build identity;
3. exception code and faulting thread;
4. `!analyze -v`-equivalent analysis and the native stack;
5. the first invalid state or exception location, not merely whether a Sakura
   frame appears;
6. the last valid diagnostic-ring event, if the dump contains the ring, and
   its correspondence to the stack;
7. one final classification:
   `Sakura-attributed`, `Non-Sakura-attributed`, or `Inconclusive`.

Only a `Sakura-attributed` result that explains or reproduces the same
invariant violation is a reason to propose a separate state-machine repair
phase. Until then, keep the sync/async edit-session fallback, generation and
revision invalidation, focus/deactivation/composition termination, document
commit/rollback, Electron `GetSelection`, retry/queue semantics, and write
ownership unchanged.

