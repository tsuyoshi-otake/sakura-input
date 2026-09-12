# D6: GitHub-hosted desktop runner admission

## Decision

Use GitHub-hosted `windows-latest` for the serialized weekly and manually dispatched ignored desktop-test workflow. Keep the capability probe, metadata inventory, exact-one result proof, process cleanup, and evidence upload in every run. A capability or inventory mismatch fails before test execution.

This admits the environment for Phase 1.10 and removes the D6 prerequisite from Phase 5.9. It does not establish that the complete settings and renderer ignored suites pass. That evidence belongs to the first admitted `.github/workflows/desktop-tests.yml` run.

## Provenance

The prerequisite diagnostic ran from commit `98ef18c18c94efdab9744aae93f0c97bb916d208` in GitHub Actions run `34713825191`, job `103607261803`, on 2026-09-12 UTC. The downloaded artifact was inspected at `C:\Users\developer\tmp\sakura-hosted-desktop-34713825191`.

The hosted process ran in session 2 as an interactive user. It had `WinSta0`, the `Default` thread and input desktops, a readable cursor, a foreground window, and a 1024×768 screen. `capability-probe.json` recorded no failures and `probe_passed: true`.

Cargo metadata selected `sakura-settings` target `settings_topic_user32`. Its ignored inventory included `tab_focus_order_skips_hidden_topics_and_ends_at_actions` exactly once. Three serial exact-filter invocations each recorded the requested identity once and `1 passed; 0 failed; 0 ignored; 0 measured`. The quiet wrapper returned one PASS line for each round.

The final process check passed. Cleanup removed only `%LOCALAPPDATA%\SakuraInput` and `$USERPROFILE\tmp\sakura-settings-user32-*`. The remaining `Microsoft`, `AppData`, and empty `tmp` directories were OS-created scratch delegated to teardown of the ephemeral hosted VM; the job did not recursively delete the isolated profile.

## Evidence digests

| Artifact member | SHA-256 |
|---|---|
| `capability-probe.json` | `1cd8c4ace69dc6230b8f6bd8ab8302ddb52429107414160695f80be7096bac57` |
| `cargo-metadata.json` | `77b8a456533d42e4e71abe2cd68a8b5365e6673de8d1da0bd982831ea93dd9b6` |
| `ignored-tests.cargo.raw.log` | `acc9492c2fd0e9d23332519b9e2f648e90d7cc5c93261090ca128bf2818f310e` |
| `exact-tab-round-1.cargo.raw.log` | `4da1a6d6d35db471bf1f3697a741ac105e24a9960aea30188667d2786e6f8a18` |
| `exact-tab-round-2.cargo.raw.log` | `e45c10da8a7cc79c3a235646ecb3f29ffb7091453b0f2f6b82296146a825fb7b` |
| `exact-tab-round-3.cargo.raw.log` | `40edd066349ce353d5f496405108b7f86b3e749553da2d6a93395c2883ee825b` |
| `cleanup-residuals.json` | `6f68692a638ebe360b96c3ab537e2b6f6f4b6ae3394ca4d5394e69677ab90d2d` |
| `cleanup.raw.log` | `4908ae266053c57aea187828a431983ace7b4f17d12215a627f685650e92193e` |

The source identity recorded by the job is `crates/sakura-settings/tests/settings_topic_user32.rs`, SHA-256 `42a6a740f21dbf285127a9a04882be9b1526ae88c196917040955198bd434907`.

After the renderer tests moved behind the library target, a local metadata/listing-only check at commit `3191d525403c` enumerated the current manifest identities exactly: renderer library 1, renderer binary 0, renderer integration 10, and settings integration 21. The captured log is `C:\Users\developer\tmp\sakura-desktop-inventory-current.log`, 5,265 bytes, SHA-256 `a7c68ab308c9a8dffe3cd43e8d203ac509729b5d0a2d5322a7305f1985b6dddf`. Its command and process cleanup completed successfully in approximately 25 seconds. This check proves inventory membership only; it did not execute the ignored UI tests.

## Admitted workflow boundary

The scheduled workflow runs one Windows job at a time with `cancel-in-progress: false`, a 180-minute job timeout, and three modes: `all`, `settings`, and `renderer`. Weekly runs use `all`; dispatch can select any mode.

The workflow compares Cargo's current ignored-test inventory against 21 settings identities and 11 renderer identities. The indicator identity is discovered through the `sakura-renderer` library target; the binary target currently contributes zero ignored identities. It excludes only `the_pad_stays_open_for_a_look`, whose current source reason identifies it as a manual design-review harness with a 900-second default duration. The remaining 10 renderer identities and all 21 settings identities run serially through `ci/run-test-quiet.ps1`; every invocation must prove its exact identity and a one-passed summary.

This decision must be revisited if `windows-latest` loses interactive User32 capability or repeated scheduled evidence shows environment instability. Inventory changes require a reviewed manifest update rather than silently changing the executed set.
