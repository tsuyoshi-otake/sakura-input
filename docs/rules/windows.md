## Windows specifics

- **Real-process tests must isolate `LOCALAPPDATA` unless they explicitly test
  the installed user profile.** The engine's learning, configuration, user
  dictionary, and diagnostics all derive from that root. A candidate UIA test
  once restored a previously learned candidate at index 11 and opened on page 2
  before the test sent PageDown. A unique per-run app-data directory both makes
  the test deterministic and prevents verification from mutating user state.

- **Auxiliary indexes over a mapped dictionary should retain image offsets, not
  copied records.** Copying every 24-byte entry into the prediction index pushed
  private working set over the 15 MiB release gate. A four-byte entry index lets
  the hot path materialize the validated record from the read-only mapping only
  when ranking or rendering it; the compact index passed the footprint gate
  while keeping end-to-end prediction p99 below 0.3 ms.

- **A server-side UI Automation raw provider needs COM initialized on the
  renderer UI thread before the provider/window is created.** The candidate
  window handled `WM_GETOBJECT` and called `UiaReturnRawElementProvider`, yet a
  separate real UIA client saw only the generic host-window provider (empty
  Name and no Sakura AutomationId). Adding an STA guard before window creation
  made the custom `IRawElementProviderSimple` discoverable; the real-process
  `candidate_uia` test now proves Name, AutomationId, control type, bounding
  rectangle, paging updates, and hidden/off-screen state.

- **`FILE_APPEND_DATA` and `FILE_CREATE_PIPE_INSTANCE` are the same bit
  (0x0004).** A named-pipe client that asks for `GENERIC_READ | GENERIC_WRITE`
  is therefore also asking for permission to create a pipe instance, which the
  server's DACL rightly refuses. Clients must request the exact
  `CLIENT_ACCESS` mask — see `sakura-ipc::security`.

- **Inno Setup has no native way to fail an uninstall when an
  `[UninstallRun]` entry exits nonzero.** A failing entry is a line in the log
  and file removal proceeds regardless — which is exactly how an IME leaves
  Windows pointing at a text service whose DLL is gone. `installer/setup.iss`
  runs `--unregister` from a `Check:` function that execs it, reads the real
  exit code and calls `Abort`.

- **`$args` is a PowerShell automatic variable.** Assigning to it inside a
  function shadows the unbound-argument array for the rest of that body. Name
  the local something else (`$installerArgs`).

- **`$PSCmdlet` resolves from the enclosing script scope inside plain
  (non-advanced) functions of an advanced script**, so `-WhatIf` propagates
  into helpers that never declared `[CmdletBinding()]`. Verified with a
  purpose-built probe script rather than assumed.

- **Opening the engine pipe is not a successful protocol handshake.** The
  renderer watchdog used to reset its reconnect delay as soon as `CreateFileW`
  succeeded. A reachable engine that rejected `Hello` therefore produced an
  immediate reconnect storm, even though the source comment claimed backoff
  would slow it. Only a valid `Hello` may reset the delay; protocol rejection
  retains exponential backoff up to the ceiling. The schedule is now a tested
  explicit terminal transition in `sakura-renderer::watch`.

- **Every child wait must consume the caller's remaining deadline, not a fresh
  per-operation timeout.** `sakura-regtool --stop` had an overall deadline but
  each pipe reconnect could independently wait the full two-second patient
  budget, allowing the loop to overshoot its advertised terminal. Each connect
  and sleep is now capped by the remaining duration, with the cap covered by a
  regression test.

- **After a synthetic `WM_DPICHANGED`, wait relative to the most recently
  observed rectangle.** The live candidate UIA test compared the post-DPI move
  against its original caret rectangle. Because an earlier placement had
  already changed that rectangle, the wait completed immediately on stale
  state and raced the new placement. Capturing the DPI rectangle first and
  requiring a subsequent change made the real HWND/UIA assertion deterministic.

- **A Cargo target directory is not the installed product layout.** The
  watchdog correctly starts its sibling engine, but that engine cannot discover
  `{app}\dict\system.dic` when both executables live under `target\...\debug`.
  Installed-layout supervisor tests must pass an explicit dictionary path to
  the supervisor so the restarted child inherits the same validated data root.

- **Diagnostic tier names must come from the same canonical vocabulary as CPU
  dispatch.** The core selected `avx512bw` while the engine event log shortened
  it to `avx512`, causing machine-readable Phase 1 evidence to reject a healthy
  startup. `CpuTier::name()` now emits the exact core tier and has a unit test.

- **Isolating `LOCALAPPDATA` also hides per-user developer tools.** A clean
  verification profile could not find Inno Setup even though it was installed
  under `%USERPROFILE%\AppData\Local\Programs`. Tool discovery used by isolated
  real-process tests must accept an explicit path and check that fixed per-user
  install location instead of treating the isolated application-data root as
  the developer's tool root.
