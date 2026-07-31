# installer/

Inno Setup packaging. This is the one place the full-scratch rule does not
apply (DESIGN 3.1): commodity install machinery is not worth reimplementing,
and getting it wrong is how an IME bricks somebody's ability to type.

| File | Phase | Purpose |
|------|-------|---------|
| `setup.iss` | 1 | Inno Setup script — layout, registration, uninstall |
| `out/` | — | Build output; git-ignored |

Two properties the script must hold to, both learned from how IMEs fail:

- **Uninstall order is a safety property.** The language profile is withdrawn
  first and the CLSID entries last (DESIGN 12.1), so no host can ever activate
  a text service whose class has already been deleted.
- **The DLL is replaced via `restartreplace`.** It is loaded into every running
  host process, so it cannot be overwritten in place. A reboot is the expected
  completion of an upgrade, not a failure, and mixed versions must stay safe
  until then.
