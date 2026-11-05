# 2.0.27 release audit (#436)

The four pre-publication checks ran together against the merged conversion-diagnostics source before release preparation. This records pattern findings, not an assertion that every pattern is a secret.

| Check | Result and disposition |
|---|---|
| Tracked text | 1,370 tracked text files; 178 matches in 64 files, fully indexed below. No API-token/private-key pattern matched. |
| Email-shaped text | Five matches: an SSH Git address, two Win32 import declarations, and two synthetic test addresses. No personal mailbox occurs in tracked content under this scan. |
| Account names | 86 occurrences of the established public repository/maintainer identity in URLs, CODEOWNERS, update trust pins, licenses and development records. These are intentional public project metadata, not credentials. |
| UUIDs | 18 occurrences: ten product/COM/test constants and eight historical agent/verification identifiers. The latter are provenance identifiers, not authorization tokens or user input. |
| User-path-shaped text | 67 occurrences: generic path examples, keyboard/REST false positives, and historical development-tool/evidence paths using the generic developer account. These existing records contain no private input or credential contents; they are not all executable paths or newly introduced data. |
| Credential assignments | Two occurrences of the same explicitly synthetic prototype/test demo password. Neither is used for a deployed account or API. |
| Git identities | 898 commits across local refs. Five author and five committer identities: the maintainer's established public name/handle and mailbox, GitHub automation, Devin integration, a contributor placeholder, and Cursor Agent. The same maintainer forms were already recorded by the preceding release audit. No identity rewrite was performed. |
| Visibility, tags and releases | Repository already public; 65 tags and 55 releases; 2.0.26 was latest and no 2.0.27 tag/release existed. No visibility change. |
| License | Root MIT LICENSE present; SHA-256 `aeedc4ef393103e90ea3e28a82133406f5a97f267f7d5adc8e4484f662162d6d`. |

The scan covers email/account/path/UUID patterns, common API-token formats, private-key headers and quoted credential assignments. Matches were inspected by value and source context. Existing maintainer attribution, technical constants, synthetic examples and historical tool provenance were classified explicitly rather than falsely reported as zero findings. No confirmed credential or private-input leak requiring a history rewrite was identified. Private audit working files are not release assets.

## Complete finding index

Values are not duplicated here; the indexed source locations provide the evidence. Repeated matches on one line contribute to the Matches column.

| Category | Tracked file | Lines | Matches |
|---|---|---|---:|
| account_name | `.claude/goal-loop/developer-history-lifecycle/verify.sh` | 76 | 1 |
| account_name | `.claude/goal-loop/shift-latin-backspace-order/rubric.md` | 2, 12 | 2 |
| account_name | `.claude/memory/journal.md` | 1256, 1551, 2245, 2422, 2444, 2458, 2474, 2576, 2631, 2695, 2865, 2930, 2982, 3095, 3185, 3484, 3566, 3578, 3653, 3693, 3695, 3886 | 24 |
| account_name | `CODEOWNERS` | 2, 3, 4, 5, 6, 7, 8, 9 | 8 |
| account_name | `Cargo.toml` | 39 | 1 |
| account_name | `THIRD_PARTY_NOTICES.md` | 22, 63 | 2 |
| account_name | `ci/rulesets/README.md` | 12 | 1 |
| account_name | `crates/sakura-settings/src/update_trust.rs` | 174, 257, 290 | 3 |
| account_name | `crates/sakura-settings/src/update_trust_tests.rs` | 78 | 1 |
| account_name | `crates/sakura-settings/src/updater.rs` | 63, 65, 965 | 3 |
| account_name | `docs/architecture/agent-refactor-plan.md` | 4, 8, 530, 588 | 6 |
| account_name | `docs/plans/sakura-pad-protection.md` | 5 | 1 |
| account_name | `installer/setup.iss` | 107 | 1 |
| account_name | `scripts/publish-release.ps1` | 13, 53, 54 | 3 |
| account_name | `scripts/sign-update-manifest.ps1` | 33, 36 | 2 |
| account_name | `scripts/verify-all-phases.ps1` | 490, 495, 510, 528 | 4 |
| account_name | `scripts/verify-phase0.ps1` | 112, 127, 140, 148 | 4 |
| account_name | `scripts/verify-phase5.ps1` | 378, 395 | 2 |
| account_name | `scripts/verify-update-manifest.ps1` | 31, 34 | 2 |
| account_name | `scripts/write-release-manifest.ps1` | 63, 65 | 2 |
| account_name | `verification/fixtures/update-signing-v2/manifest-positive.txt` | 3, 12 | 2 |
| account_name | `verification/fixtures/update-signing-v2/manifest-tampered.txt` | 3, 12 | 2 |
| account_name | `verification/settings-screen-audit.md` | 50 | 1 |
| account_name | `verification/shift-latin-order/correspondence-and-audit.md` | 3 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-boundary/stdout.log` | 5 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-events/stdout.log` | 5 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-large/stdout.log` | 5 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-medium/stdout.log` | 5 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-reach-aiueo/stdout.log` | 5 | 1 |
| account_name | `verification/shift-latin-order/tlc/ShiftLatinInput-small/stdout.log` | 5 | 1 |
| account_name | `verification/update-signing-v2.md` | 60 | 1 |
| credential_assignment | `docs/prototypes/sakura-pad-protection.html` | 852 | 1 |
| credential_assignment | `docs/prototypes/sakura-pad-protection.test.cjs` | 14 | 1 |
| email | `crates/sakura-settings/src/input_history.rs` | 859 | 1 |
| email | `crates/sakura-settings/src/updater_tests.rs` | 782 | 1 |
| email | `installer/setup.iss` | 336, 426 | 2 |
| email | `third_party/smile-chat-public/frontend/public/glossaries/ja_part70.json` | 298 | 1 |
| local_user_path | `.claude/goal-loop/shift-latin-backspace-order/rubric.md` | 12 | 1 |
| local_user_path | `.claude/memory/journal.md` | 1984, 2098, 2119, 2519, 3601, 3611 | 6 |
| local_user_path | `crates/sakura-core/src/keymap/keymap_tests.rs` | 181, 182 | 2 |
| local_user_path | `crates/sakura-settings/src/cli.rs` | 1245 | 1 |
| local_user_path | `crates/sakura-settings/src/ui_tests.rs` | 577 | 1 |
| local_user_path | `docs/architecture/desktop-runner-admission.md` | 11, 34 | 2 |
| local_user_path | `docs/history/CLAUDE.pre-154.md` | 112, 113, 118, 142 | 4 |
| local_user_path | `docs/history/issues/vscode-crash-investigation-20260802.md` | 15, 16, 21, 45 | 4 |
| local_user_path | `docs/neural-eval-communication-20260811.md` | 19, 25, 26 | 4 |
| local_user_path | `docs/research/context-prediction-current-state.md` | 12 | 1 |
| local_user_path | `third_party/smile-chat-public/frontend/public/glossaries/ja_part106.json` | 72, 84 | 2 |
| local_user_path | `third_party/smile-chat-public/frontend/public/glossaries/ja_part51.json` | 1032 | 1 |
| local_user_path | `verification/dual-tsf-candidate-board/tla-record.md` | 7 | 1 |
| local_user_path | `verification/dual-tsf-physical-key-arbitration/tla-record.md` | 7 | 1 |
| local_user_path | `verification/history-suggestion-convert/tla-record.md` | 7 | 1 |
| local_user_path | `verification/homophone-detail-verification.md` | 53, 56 | 2 |
| local_user_path | `verification/shift-latin-order/tla-record.md` | 5 | 1 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-boundary/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-events/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-large/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-medium/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-reach-aiueo/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/shift-latin-order/tlc/ShiftLatinInput-small/stdout.log` | 6, 7, 8, 9 | 4 |
| local_user_path | `verification/space-key-dispatch/tla-record.md` | 7 | 1 |
| local_user_path | `verification/space-oracle-correspondence-results.json` | 306 | 1 |
| local_user_path | `verification/space-oracle-correspondence.md` | 69 | 1 |
| local_user_path | `verification/space-tlc-execution-results.json` | 84 | 1 |
| local_user_path | `verification/space-tlc-execution.md` | 66, 67 | 2 |
| local_user_path | `verification/tsf-predicting-space/tla-record.md` | 14 | 1 |
| local_user_path | `verification/tsf-probe-host-insert/tla-record.md` | 6 | 1 |
| uuid | `.claude/goal-loop/shift-latin-backspace-order/journal.md` | 14, 25, 50, 73 | 4 |
| uuid | `.claude/memory/journal.md` | 2464 | 1 |
| uuid | `ISSUE-COMPLETION-REVIEW.md` | 35 | 1 |
| uuid | `crates/sakura-reg/src/com_server.rs` | 88 | 1 |
| uuid | `crates/sakura-reg/src/guids.rs` | 79, 86 | 2 |
| uuid | `crates/sakura-reg/src/user_profile.rs` | 156, 157 | 2 |
| uuid | `docs/history/CLAUDE.pre-154.md` | 127 | 1 |
| uuid | `docs/history/issues/vscode-crash-investigation-20260802.md` | 30 | 1 |
| uuid | `installer/setup.iss` | 100 | 1 |
| uuid | `scripts/vm-smoke.ps1` | 83, 84, 85 | 3 |
| uuid | `verification/homophone-detail-verification.md` | 72 | 1 |

## Release contract and verification

The release changes only package/installer version, the existing updater sequence, workflow default tag, notes and this audit record. Third-party dependency versions remain pinned. Sequence 35 must remain exact LF text. The tag may be pushed only after the preparation commit reaches main. The candidate must match GitHub source attestations, and the locally signed update manifest must verify against the pinned keyring before and after draft upload.

Installation verification must check the published installer hash, installed payload hashes and COM registration, resident engine/renderer paths, a fresh active history generation, and synthetic conversion. Installer success alone is not proof that the resident version changed. Native host/UI behavior has a separate scope from an engine pipe probe; record that distinction in the completion report.

## IRV

The required scope is release metadata, release/publishing scripts, update-signing and installer verification contracts. Scope expands into privacy/provenance and resident-process checks because a published installer and an installed runtime need separate evidence. This release preparation adds no runtime owner or dependency direction. The next release can reuse this audit index and the existing publisher/installer verification contract; it must still audit its own commit and authenticate its own artifacts.
