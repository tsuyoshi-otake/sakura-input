# installer/

Inno Setup packaging. This is the one place the full-scratch rule does not
apply (DESIGN 3.1): commodity install machinery is not worth reimplementing,
and getting it wrong is how an IME bricks somebody's ability to type.

| File | Phase | Purpose |
|------|-------|---------|
| `setup.iss` | 1 | Inno Setup script — layout, registration, uninstall |
| `out/` | — | Build output; git-ignored |

Related files elsewhere in the tree, listed here because they only make sense
together with this script:

| File | Phase | Purpose |
|------|-------|---------|
| `../.github/workflows/installer.yml` | 1 | CI: build the x86_64 release, compile `setup.iss` with Inno Setup, upload `sakura_setup.exe` |
| `../scripts/vm-smoke.ps1` | 1 | Install → type → uninstall → verify typing still works, against a disposable VM snapshot (DESIGN 12.2) |

Properties the script must hold to, learned from how IMEs fail:

- **Uninstall order is a safety property.** At
  `CurUninstallStepChanged(usUninstall)`, the SYSTEM cleanup task is removed and
  then the language profile is withdrawn before any file deletion. A nonzero
  deregistration result restores the cleanup task and aborts Uninstall, so no
  live registration can be left pointing at files the same run deletes.
- **Runtime files are versioned side by side.** The TSF DLL, engine, renderer,
  settings payload, dictionary, and notices are copied below
  `versions/<version>-<build-id>`. The stable root tools register the new DLL
  explicitly after the copy, so a host process may keep an older image loaded
  without blocking activation and a normal update does not require a reboot.
  Obsolete version directories are removed on a best-effort basis after the
  switch. A directory whose DLL is still mapped is left for the hidden
  `Sakura Input Maintenance\Payload Cleanup` task, which runs as SYSTEM at
  every logon and retries without elevating the interactive IME task.
- **Updates preserve both scheduled tasks.** Their actions use stable root
  bootstraps, so deleting and recreating either task before copying payloads
  only creates a failure window. The updater starts Setup normally and lets
  Inno perform UAC elevation, which preserves the original-user token used for
  fresh per-user registration; existing logon tasks are not rewritten.
  `--enable-profile` also waits for the stable logon bootstrap after the update,
  so the newly active engine and renderer start in the current desktop instead
  of waiting for the next sign-in.
- **Two install-time preconditions gate everything else (DESIGN 3.2/12.2).**
  `MinVersion=10.0.22000` refuses anything older than Windows 11, and
  `InitializeSetup` refuses a CPU without AVX before any file is copied —
  the whole workspace is built with `-C target-feature=+avx`
  (`.cargo/config.toml`), so installing without it would let the DLL fault on
  its first instruction inside every process that loads it instead of
  failing here, cleanly.
- **No x86 payload.** This product targets Windows 11 on x86_64 only
  (DESIGN 3.2), so `[Run]` calls `regtool --register --no-wow64` explicitly
  rather than relying on its `Wow64::Auto` default, and 32-bit host
  applications fall back to MS-IME by design, not by omission.
