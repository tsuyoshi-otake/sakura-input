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

- **Read a log that Inno Setup is still writing with `FileShare.ReadWrite`.**
  `[IO.File]::ReadAllText` opens with `FileShare.Read`, so polling the `/LOG=`
  file of a running installer throws a sharing violation (observed 2026-10-04
  on the 2.0.10 reinstall; the installer itself finished normally). Open a
  `FileStream` with `FileShare.ReadWrite -bor FileShare.Delete` and wait for
  both `Installation process succeeded.` and `Log closed.`.

- **A process the logon task starts runs below normal, and so does every child
  it starts without a class flag.** Task Scheduler's default task `Priority` is
  7 (`BELOW_NORMAL_PRIORITY_CLASS`), and `CreateProcess` gives the children of
  a below-normal parent the same class. Observed 2026-10-16 (#322): the
  installed 2.0.14 engine, renderer and neural worker all ran `BelowNormal`,
  and with 32 busy threads a below-normal engine missed the 50 ms key budget in
  every measured run (worst 10.06 s) while above normal stayed at or under
  338 µs (the key budget was 50 ms then; #415 made it 200 ms while the engine
  answers). Check `Get-ScheduledTask` `Settings.Priority` and the live process
  class before blaming conversion code for load-only failures. A process that
  raises its own class must pass an explicit class to the helpers it spawns,
  or they inherit the raised one.

- **PowerShell 7.3+ reserves `clean` as a script-block keyword.** A helper
  function named `Clean` and a call such as `{ Clean }` fail to parse
  ("'clean' の後にステートメント ブロックがありません"), so the whole script stops
  before its first step. Observed 2026-10-16 (#357) in a throwaway release-gate
  script; renaming the helper to `Invoke-CleanCheck` fixed it. Give helpers a
  verb-noun name.

- **`ConvertStringSecurityDescriptorToSecurityDescriptorW` rejects a space
  after a numeric `O:` SID.** `O:S-1-5-18 D:(...)` fails with
  `ERROR_INVALID_SID (0x80070539)` while `O:BA D:(...)` and the same sections
  concatenated (`O:S-1-5-18D:(...)`) parse. Keep `O:...D:` adjacent in
  SDDL-built test descriptors. In the same helper, the SDDL right `DC` is the
  directory-service right `0x2`, not `FILE_DELETE_CHILD` (`0x40`); write
  file-right bits as `0x` hex literals (#360 A1, 2026-10-18).
- **Never canonicalize a path chain a reparse check must inspect.**
  `Path::canonicalize` resolves junctions, so the walked components lose the
  very `FILE_ATTRIBUTE_REPARSE_POINT` the check exists to reject; canonicalize
  only the two endpoints used for the containment compare. To create a
  junction in a test, `New-Item -ItemType Junction` requires an absolute
  `-Target`. Through `cmd /c` from Rust `Command`, the `mklink` form #360 A1
  tried mangled its argv; `args(["/c", "mklink", "/J"]).arg(link).arg(target)`
  with space-free paths worked in #360 #2 (`codex_cli_workdir_tests.rs`), and
  paths that need quoting are unverified (2026-10-18).
- **Never use `user32.dll` as the "other module" when testing per-module
  window classes.** Its instance handle is reserved for system classes.
  Observed 2026-10-18 (#360 A2): in a pwsh P/Invoke probe, `RegisterClassW`
  failed with error 87 under the `user32.dll` handle, even for a fresh class
  name. In a Rust test exe, a duplicate name under it failed the same way.
  Ordinary modules (exe, `kernel32.dll`, `version.dll`, `comdlg32.dll`)
  register the same class name in either order. Test with such a module, and
  test both orders.

- **A module pin needs an over-release test.** Observed 2026-10-18 (#360 A2):
  `GetModuleHandleExW(FROM_ADDRESS)` without `UNCHANGED_REFCOUNT` adds a
  reference the caller never frees. That leaked reference survives one
  `FreeLibrary` just like `GET_MODULE_HANDLE_EX_FLAG_PIN` does. Call
  `FreeLibrary` on the same handle several more times, then assert the module
  is still mapped. Only a real pin passes. The test caught the dropped-PIN
  fault only after this change.

- **`cargo test` never builds a `cdylib` target.** Nothing links the library.
  A test that loads `sakura_tsf.dll` must first run `cargo build -p sakura-tsf
  --lib --locked` itself. Otherwise it tests a stale DLL. Observed 2026-10-18
  (#360 A2): after the sources were restored, the test still loaded a
  leftover DLL that contained the injected fault.

- **Rust's Windows `FileType::is_dir()` is false for a junction or symlink.**
  std defines it as `!is_symlink && is_directory`, and a junction is a name
  surrogate (checked 2026-10-18 in the toolchain's `library/std/src/sys/fs/
  windows.rs`; #360 #2 T2). `symlink_metadata(p)?.is_dir()` alone excludes
  reparse links, so dropping only an extra `is_symlink()` is an equivalent mutant.

- **A log follower that does not share DELETE blocks the rotation rename.**
  Observed 2026-10-27 (#406): with a handle opened `FILE_SHARE_READ |
  FILE_SHARE_WRITE` on `debug.tsv`, the rename over `debug.1.tsv` failed. The
  trace kept every row it had and dropped new ones until the handle closed.
  std opens files with all three share bits, so only an outside tool can do
  this. The writer treats it like any failed rotation: one error, then a
  back-off, no retry loop.

- **A load loop over the `sakura-renderer` lib test binary runs one process at a
  time, or skips the clipboard and z-order tests.** Observed 2026-10-28 (#419):
  four processes of the binary at once failed
  `pad_clipboard::tests::private_station_clipboard_round_trip` in 7 of 48 runs
  and, in a second loop, 5 of 24, each time with `OpenClipboard` denied
  (`0x80070005`) inside the test's private window station; once in the 48 the
  `pad_hardware` topmost-parent test failed as well. One process at a time
  passed 12 of 12, and four processes with `--skip pad_clipboard --skip
  pad_hardware` passed 48 of 48. CI starts the binary once per job and the cause
  was not investigated, so these failures say nothing about the test under
  measurement; do not read them as a product defect.
