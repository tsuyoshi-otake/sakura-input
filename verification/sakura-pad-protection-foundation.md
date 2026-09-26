# Sakura Pad protection foundation — #269, 2026-09-27

Status: preparatory implementation; **production Pad protection is not yet
implemented**. The existing v2 DPAPI storage, installed application and user
data are unchanged. No release, installation, real migration or key
registration was performed. Original topmost/title fixes have separate
evidence in `verification/sakura-pad-quality.md`.

## Rubric and evidence

| Criterion | Verify | Expect / observed |
|---|---|---|
| Scoped encryption fails closed | Wrapped `cargo test -p sakura-pad-worker --release -- --test-threads=1` | RFC 9106 vector, wrong password, scope mismatch, tamper, nonce changes, substitution and malformed bounds pass |
| Child lifetime and failure frames | Same command, `worker_stdio` integration target | Owned child round-trip succeeds; failures contain no plaintext/diagnostic echo; cancellation reaps exact child |
| Code/dependency checks | fmt, clippy `-D warnings`, `ci/dep-policy.ps1` and `-SelfTest`, `cargo audit --file Cargo.lock` | Pass; active architecture gates R1/R2/R8/R9/R12/R13 pass. Existing advisory R5/R7 warnings and future R3/R4/R6 paths remain |
| Dependency adoption | crates.io version API for every newly locked crate | All 17 newly added versions older than seven days, not yanked; exact direct versions pinned and Cargo.lock updated |
| Demo state transitions | `node --test --test-timeout=10000 docs/prototypes/sakura-pad-protection.test.cjs` | 20 pure-model and script-syntax tests pass, including existing-policy reauthentication; these are not browser/UIA evidence |
| Forward-version guard | Wrapped `cargo test -p sakura-renderer --lib` | 155 passed / 1 ignored; explicit newer DPAPI document primary or backup does not fall back to older data. Corrupt legacy primary still recovers from valid v2 backup |
| Rendered prototype | Chrome via configured browser tool | BLOCKED: tool rejected `file://` by URL security policy; no alternate browser/server workaround attempted. Visual/DPI/keyboard/HC review remains unverified |

The first debug suite passed 10 tests (7 unit, 3 subprocess), recorded in TKW
run `b4d23c79136affd92bd2cc86ea3c2d38`, request
`68d908698367405f94c69d94d69cd7e1`. After the independent known vector and
substitution tests were added, release tests passed **12** (9 unit, 3
subprocess), run `e60cb2543f42e774981c51873dfd003e`, request
`e37c6c56438943a195a68998887f63dd`. Unit execution was 1.17 s, subprocess
execution 0.30 s on this host. These are suite times, not an unlock latency
SLA or a memory-budget measurement. The repository process check was clean
after both runs. Final correlation/failure-classification changes passed
**13 tests** (10 unit, 3 subprocess), run `ba58f430759618f0f0de40a0a2a399c5`,
request `6e4552b78b2c4a6b88ea10df1d9a6c0c`; the process check was clean.
Latest worker clippy request:
`66f127f6c4454259b3c873445c39eae5` (passed). Audit run
`4c8ff7538ffbfa689bdbd880380f1924` scanned 101 packages with 1,271 loaded
advisories and reported no vulnerability.

The prototype model's first run, TKW `c864d8a02b8dd6563239d7045010865b`,
found the missing cancellation/scope rules. After the model and UI wiring were
corrected, TKW run `f322574f21866d07315f9176ea0cc76c` passed **20 tests**,
request `d5f2691ca4e24d95aef04c63986a80b2`. These cover masking,
whole-vs-memo isolation, recovery registration, old-password reauthentication
for policy changes, cancellation, save failure/retry, stale unlock rejection
and both inline scripts parsing. A previous 20-test run found one test setup
that attempted to modify an individually locked memo; the test was corrected
to unlock the memo first. The Node process and repository runner checks were
clean after the final pass. Root source inspection confirmed input capture
before UI refresh, one delegated action handler, and Pad DOM clearing when
whole lock takes effect. Static color calculation gives white text on primary
button backgrounds at 5.44:1 (light), 6.62:1 and 8.04:1 (dark/default and
hover). Actual visual, keyboard, UIA and high-contrast behavior remain untested.

After the new forward-version guard, wrapped renderer library test run
`19e66341bc58a6a2e959566016f8ba27`, request
`4c95137f69e7417ca3b8503e7024d368`, passed **155** and ignored **1**.
The narrower storage tests passed first in run
`242932bd052027d3f3e667e5209f69aa`, request
`c8d394e750bd4ced824f6b1a5472f933`. Renderer clippy with `-D warnings`
passed in run `74d5bce243f064c13276879dafcaf259`, request
`495b47b8cf3945d8b3c3f07a56ed5ea7`; full fmt check passed. The repo
runner check was clean after both test runs. This guard only recognizes an
explicit newer `SKRLPADn` magic after DPAPI decryption. A future raw protected
format needs a separate durable cutover marker and no legacy fallback.

Dependency age evidence is retained locally under
`~/tmp/sakura-pad-crypto-dependency-age.json`. Newly admitted versions:
`aead 0.5.2`, `aes 0.8.4`, `aes-gcm 0.10.3`, `argon2 0.5.3`, `base64ct 1.8.3`,
`blake2 0.10.6`, `cipher 0.4.4`, `ctr 0.9.2`, `ghash 0.5.1`, `inout 0.1.4`,
`opaque-debug 0.3.1`, `password-hash 0.5.0`, `polyval 0.6.2`, `rand_core 0.6.4`,
`subtle 2.6.1`, `universal-hash 0.5.1`, `zeroize 1.8.2`. AES/GHASH/POLYVAL direct
pins activate available clearing features; remaining clearing limits are
recorded in the worker README, not hidden behind a complete-erasure claim.

## Hardware evidence (read-only)

The user reports a YubiKey 5-series device. Windows reports an attached Yubico
VID 1050 / PID 0407 device with a FIDO HID interface. No exact model, firmware,
PIN, serial or credential inventory was read. `ykman` is not installed.

Root reran `~/tmp/probe-webauthn.ps1` after inspecting its source. It loads
webauthn.dll, reads export presence and invokes only
`WebAuthNGetApiVersionNumber`. Runtime API version is **9**, DLL version
10.0.26100.9278 on OS build 26200. Export presence includes MakeCredential
and GetAssertion, but neither was called. Installed SDK PRF fields and the
[Microsoft PRF salt contract](https://learn.microsoft.com/en-us/windows/win32/api/webauthn/ns-webauthn-webauthn_hmac_secret_salt)
establish an OS API path, not that this key successfully derives a PRF.
Enrollment, PIN/touch, cancellation, removal, spare-key recovery and OS dialog
focus relative to topmost Pad remain required hardware tests.

## Actual scope / IRV

A. The new worker owns envelope cryptography and bounded stdio framing;
its tests and README are the local contract. Root additionally inspected the
pinned crypto feature/Drop implementations to qualify clearing claims.
The HTML prototype owns demo interaction state. Workspace Cargo/lock and the
closed dependency policy/design exception own build admission. The existing
PadStore owns the narrow forward-version guard and its three tests. This
builds on the previous Pad storage and settings contracts mapped in the plan.

B. Scope expanded to dependency closure and worker lifetime because linking
third-party crypto into the renderer violates the existing core runtime
policy. Settings UX also requires distinct draft/committed protection state;
merely drawing password fields would miss that transaction boundary. There
is also a real downgrade seam at `PadStore::load`: an unknown newer primary
previously fell back to a readable older backup. A limited guard now stops
that case, while the planned protected format still needs an independent
cutover state. There was no unrelated production refactor. UI rendering is explicitly unverified
due to browser policy, rather than replaced by a source-only visual claim.

C. No production state ownership moved. Existing Pad state/storage still owns
v2 DPAPI data and now surfaces explicit newer-version errors before trying an
older recovery copy. The experimental worker owns transient derived keys and returns
only an authenticated result or a content-free failure. Its new dependencies
are rejected from every other workspace crate and nested tool workspace.
The worker is intentionally absent from installer/release payload inventories.

D. Future integration can start with the worker README, envelope/protocol
tests, protected-store downgrade test and product UI contract without rediscovering the library choices or
OS API presence. It still must define unlocked-session ownership, document
schema, safe legacy/backup migration, recovery and trusted renderer-worker
integration. No reduction in production IRV is claimed before those contracts
are implemented and measured.
