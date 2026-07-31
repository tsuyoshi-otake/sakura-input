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

- **Uninstall order is a safety property.** The language profile is withdrawn
  first and the CLSID entries last (DESIGN 12.1), so no host can ever activate
  a text service whose class has already been deleted. `[UninstallRun]` also
  halts the whole uninstall if `--unregister` exits nonzero, rather than
  continuing on to delete the files that registration still points at.
- **The DLL is replaced via `restartreplace`.** It is loaded into every running
  host process, so it cannot be overwritten in place. A reboot is the expected
  completion of an upgrade, not a failure, and mixed versions must stay safe
  until then.
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
