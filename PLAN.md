# Sakura Input — Implementation Plan

Companion to `DESIGN.md` (v1.0 — final). Phases map to milestones M0–M4 with a
bootstrap phase in front. Each phase ends with **verifiable exit
criteria** (Verify = how to check, Expect = observable result); a phase is
done only when every criterion passes — graded by an independent verifier
run, not self-assessment.

Sizes: S = days, M = 1–2 weeks, L = 2–4 weeks of focused work.

---

## Phase 0 — Bootstrap (repo, CI, skeleton)  [S]

**Objective:** a building, testable, empty workspace with all CI gates in
place before any real code exists.

Tasks:
1. `git init`; private GitHub repo (org policy: never public); one
   tracking issue per phase, this plan checked in.
2. Cargo workspace:
   ```
   crates/
     sakura-core      lib  engine logic, platform-free, no windows dep
     sakura-proto     lib  IPC messages + hand-rolled codec
     sakura-reg       lib  GUIDs, profile/COM registration data
     sakura-tsf       cdylib  TSF text service
     sakura-engine    bin
     sakura-renderer  bin
     sakura-regtool   bin
     sakura-settings  bin  (stub until Phase 4)
     dictc            bin  (stub until Phase 2)
   data/       romaji.toml, it-terms.tsv (seed)
   installer/  setup.iss
   corpus/     golden conversion corpus (grows every phase)
   ```
3. **Dependency-policy gate in CI**: job fails if `cargo tree` shows any
   non-workspace crate other than `windows`/`windows-sys` (§3.1).
4. CI (GitHub Actions, windows-latest): fmt, clippy `-D warnings`, test,
   release build; x64 only for now. **Absolute latency/accuracy gates run
   on a dedicated self-hosted reference runner** (DESIGN §10); shared
   runners gate relative regressions only. Long fuzz campaigns run as
   resumable sharded jobs with the corpus persisted as an artifact —
   windows-latest cannot host a single 72 h job.
5. Toolchain pin, build profile (`panic=abort`, fat LTO,
   `codegen-units=1`), rustfmt/clippy config.

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Workspace builds clean | `cargo build --release` | exit 0, all crates |
| Dep policy enforced | add `serde` to a crate on a branch | CI dep-gate job fails |
| CI green | push to main | all jobs pass |

---

## Phase 1 — M0 Plumbing (the risk killer)  [L]

**Objective:** DESIGN §13 M0 — romaji typed in real apps appears inline
as hiragana and commits on Enter; width policy enforced end-to-end;
install/uninstall clean. No conversion yet.

**Ordering rule: TSF activation is the highest-risk unknown, so it comes
first — against hardcoded logic, before IPC exists — to keep TSF bugs and
IPC bugs from masking each other.**

Tasks, in order:
1. `sakura-proto` v1: fixed-layout messages (CreateSession / SendKey /
   Output / …) with monotonic per-session request ids (stale-response
   guard, DESIGN §4.3), codec + round-trip property tests + fuzz target.
2. `sakura-tsf` skeleton: DLL exports via `sakura-reg`
   (`DllGetClassObject`, `DllRegisterServer`), `ITfTextInputProcessorEx`
   + `ITfKeyEventSink` consuming nothing (pure pass-through).
   *Checkpoint: activate/deactivate in Notepad without crash.*
3. Composition path: `ITfComposition`, edit sessions, display
   attributes — echo keys into preedit, commit on Enter, **logic
   hardcoded in the DLL** for now.
4. `sakura-core`: romaji→kana FSM (trie compiled from `data/romaji.toml`,
   minimal hand-written config parser) + **width normalizer**
   (alnum/number/symbol policy) + data-driven key map (`ms-ime` preset
   default, `atok` alternative) — pure lib, exhaustive unit tests.
5. `sakura-engine` skeleton: named-pipe server (DACL **+ low-IL
   mandatory-label SACL**, verified from a real AppContainer token in
   CI), session table, wires FSM + normalizer; **persistent from logon —
   no idle exit** (DESIGN §4.3/§7); `sakura-regtool` registers the
   per-user launcher task; renderer acts as engine watchdog.
6. DLL switches from hardcoded echo to the IPC path (50 ms timeout →
   pass-through; autostart + reconnect). Hardcoded path deleted.
7. Renderer stub: mode indicator only (あ/A floating + tray icon).
8. `regtool` complete: `--register` / `--unregister` /
   `--enable-profile` / `--stop`.
9. `installer/setup.iss` v0: files + regtool ordering (§12.2); CI builds
   the installer; VM-snapshot script: install → type → uninstall → type.
10. Multi-arch: x86 DLL build; **ARM64X spike** (bespoke
    `link /MACHINE:ARM64X` merge — unproven in Rust; fallback documented
    in DESIGN §14). The spike's *conclusion* is the exit requirement,
    not a working ARM64X binary.

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Typing matrix | manual: Notepad, Windows Terminal, Chrome | romaji→hiragana inline; Enter commits |
| Width policy | `alnum_width=half`; type `docker` in 英数 mode | always half-width — never `ｄｏｃｋｅｒ` |
| Crash resilience | kill `sakura_engine.exe` mid-composition | live preedit finalized as-is (nothing dropped); pass-through continues; watchdog restarts engine |
| Focus loss mid-composition | script: alt-tab with live preedit | preedit finalized in place, never discarded |
| IPC latency | bench harness on reference runner | SendKey round-trip p99 < 5 ms |
| Zero-alloc kana path | counting-allocator assertion | 0 heap allocs/keystroke — gated in this phase, where the code ships |
| Sandbox access | CI connects to pipe from AppContainer token | connect + round-trip succeed |
| Elevated host | manual: admin terminal / Notepad-as-admin | composition + candidate flow work |
| Clean uninstall | VM script | typing intact afterwards (MS-IME fallback) |
| DLL size | release artifact | ≤ 1 MB |
| No orphaned processes | list engine/test procs after test runs | none |

---

## Phase 2 — M1 Conversion  [L]

**Objective:** DESIGN §13 M1 — real multi-word kana-kanji conversion with
a candidate window.

Tasks:
1. `dictc`: TSV schema + license gate; LOUDS trie builder; image writer
   (readings trie, value arrays, front-coded surface pool, connection
   matrix, annotations); byte-deterministic output.
2. `sakura-core` dictionary reader: mmap fixed-layout views, common-prefix
   search, offsets-not-strings discipline; **reader fuzz target** (a
   hostile image must not crash the engine).
3. Data pipeline: **freeze the connection-class taxonomy (~1.3 k, DESIGN
   §5.2) before any image work**; import + trim Mozc dictionary data;
   **smile-chat glossary importer** (`frontend/public/glossaries` →
   `it-terms.tsv`: ~7,900 reading-bearing entries; English aliases →
   English surfaces; domains → `IT` tags; definitions → annotations).
4. **Overlay POS/cost assignment — its own task, not "curation"**
   (DESIGN §6.2): bootstrap left/right ids + costs by surface/reading
   match against Mozc data; class-by-shape defaults + corpus frequency
   estimates for the rest; phonetic reading variants per English/loanword
   term; counter-phonetics reading table (一本/三本/六本).
5. Lattice + Viterbi over a reset-not-freed arena; connection costs;
   synthetic edges (single-kana fallback, katakana, numbers + irregular
   counters, OOV char-type-run grouping); IT domain prior as a bounded
   proportional cost layer under the precedence contract (DESIGN §5.6).
6. N-best (A*) per sentence — heuristic recomputed from the *boosted*
   lattice per query (admissibility, DESIGN §5.6); Output carries
   candidate lists.
7. Candidate window in the renderer (positioned from layout-sink rects
   with `TS_E_NOLAYOUT` retry, paging, digit selection) + `ITfUIElement`
   data exposure for UI-less mode; DirectWrite footprint measured now
   against the 10 MB budget, not deferred to Phase 5.
8. Quality harness: golden corpus (tech-weighted + general slice incl.
   homophone anti-regression cases), **split tuning vs held-out — gates
   compute on held-out only**; checked-in Mozc baseline file with a
   documented regeneration procedure (CI never runs Mozc); latency
   benches; counting-allocator assertion extended to conversion +
   prediction hand-off paths.

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Accuracy | held-out corpus vs checked-in Mozc baseline | top-1 ≥ 80 % of Mozc; IT slice ≥ 95 % (interim ≥ 90 % acceptable with a tracked reading-variant gap list; ratchets to 95 % by Phase 4) |
| Latency | bench | conversion p99 ≤ 20 ms (30-char reading) |
| Footprint | CI measurement | image ≤ 35 MB; engine private WS ≤ 15 MB |
| Robustness | dict-image fuzzer | no crash on hostile image |
| Candidates | manual + UIA script | window follows caret, paging works, UI-less mode serves data |

---

## Phase 3 — M2 Real IME (daily-drivable)  [L]

**Objective:** DESIGN §13 M2 — complete editing semantics + first
personalization; the author switches to Sakura full-time.

Tasks: segment focus/resize (constrained re-search); F6–F10 transforms;
identifier-case transforms (§5.6); complete revert/commit semantics
incl. commit undo (確定アンドゥ, Ctrl+Backspace);
learning v1 (exact-context homophone + recency decay, bounded store,
checksummed torn-write-safe log, sensitive-scope bypass); recent-context
v1 (left-context carryover with sentence-boundary gating + commit cache
with commit-undo eviction, §5.8); user dictionary (TSV + hot reload +
POS picklist → connection-class mapping table, §6.3); config/learning
format-version fields + old-file upgrade tests; whole-session replay
snapshot tests. The 1-week dogfood is *elapsed time*, run concurrently
with early Phase 4 work — it does not consume engineering budget.

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Editing semantics | replay test suite | segment ops / F-keys / revert chains all snapshot-clean |
| Learning flips homophones | scripted session | correct once → same reading converts correctly next time |
| Context carryover | scripted session | 「医者に」確定 → 「いった」 → 行った wins |
| Commit cache | scripted session | homophone picked once stays picked within session |
| Undo semantics | scripted session | commit undo evicts the cache entry + rolls back carryover; carryover resets at 。！？ |
| Resize locality | replay suite | segment resize never perturbs distant segments |
| Old-format upgrade | load previous-version config/learning files | no crash, no data loss, unknown fields default |
| Budgets hold | benches on reference runner | all §10 budgets green |
| Dogfood | 1 week as default IME, graded on artifacts | engine.log: 0 pass-through-fallback events; issue tracker: 0 open P0/P1; ≥ 5 days of real commits/docs typed through Sakura |

---

## Phase 4 — M3 ATOK-ness  [L]

**Objective:** DESIGN §13 M3 — prediction, annotations, reconversion,
settings; better than Mozc *for engineering work*.

Tasks: prediction thread with request coalescing (dictionary +
history sources); Tab-driven suggest flow (focus/cycle/commit/escape,
§5.3); domain coherence scaling (§5.8); homophone annotation
pane in the candidate UI; reconversion (`ITfFnReconversion`);
`sakura-settings` (config editor incl. key-map presets + suggest-accept
remap, user-dict CRUD, MS-IME/ATOK/Mozc import **and export**, learning
viewer/**export/clear**, diagnostics incl. IPC-timeout counters);
per-app profiles incl. suggest-off; composed cost-layers precedence test
(learned > commit cache > domain prior, DESIGN §5.6). Import of ATOK /
MS-IME formats is partly reverse-engineering — sized accordingly ([L],
not squeezed into a settings side-task).

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Prediction latency | bench | p99 ≤ 10 ms per keystroke |
| Accuracy | corpus harness | technical corpus ≥ Mozc baseline |
| Reconversion | manual matrix | committed text re-enters conversion in Word/Notepad |
| Import | fixture files | MS-IME / ATOK / Mozc user dicts import losslessly |
| Export | fixture round-trip | user dict exports to all three formats; learning export/clear work |
| Layer precedence | composed scripted session | explicit learned choice always beats the domain-coherence prior |
| Accuracy ratchet | held-out corpus | IT slice ≥ 95 % (Phase 2 interim gap closed) |
| Dogfood | 2-week comparison, graded on artifacts | preference statement + 0 open P0/P1 + clean diagnostics (no steady-state IPC timeouts) |

---

## Phase 5 — M4 Hardening & release  [M]

**Objective:** DESIGN §13 M4 — ship v1.0.

Tasks: app-compat sweep (Word/Excel incl. 縦書き, Electron apps, one
game, RDP, UWP, console edge cases, touch keyboard coexistence) with a
burn-down list; DPI-change-mid-composition test (drag between mixed-DPI
monitors with live preedit); UIA accessibility pass on the candidate
window; WER LocalDumps setup — verify DumpCount=5 cap + engine.log
rotation actually bound disk usage; logon-stub registration self-repair
verified against a simulated Windows-feature-update profile wipe;
fuzzing campaign (IPC, dict image, FSM) run long, sharded + resumable;
code-signing pipeline; opt-in auto-update in settings (WinHTTP,
signature + hash verification, silent installer run); third-party
license texts bundled; user docs (ja README / guide); GitHub Releases
v1.0.

Exit criteria:

| Criterion | Verify | Expect |
|---|---|---|
| Compat matrix | release checklist | all hosts green or documented workaround |
| Fuzzing | 72 h campaign | zero crashes/hangs |
| Signed artifacts | release job | Authenticode-valid installer + binaries |
| Update path | staged rollout test | dictionary + engine swap completes without reboot; DLL swap queued via restartreplace — reboot is the *expected* completion, mixed versions safe until then |
| Registration self-repair | simulated profile wipe + relogon | logon stub restores profile registration without user action |
| Disk bounds | crash-loop + long-run test | dumps capped at 5, engine.log rotation holds total under its cap |
| Release | GitHub Releases | v1.0 tagged, installer attached |

---

## Cross-phase rules

- **Gates ratchet, never loosen.** Once a budget or accuracy floor passes
  in CI it may not regress in any later phase.
- **TDD where measurable** (FSM, codec, lattice, dictc): red → green,
  with tests run and verified by the parent session, not assumed.
- **One tracking issue per phase**; PR series per task group; commit
  messages in English referencing the issue.
- **TSF folklore goes to memory.** Every verified Windows/TSF quirk is
  distilled into `.claude/memory/rules.md` the day it is confirmed —
  this domain is 90 % accumulated folklore.
- **After every test run, confirm runner processes exited** (repo-wide
  policy; orphaned runners have burned us before).
- **Manual test matrices are named human-sign-off exceptions.** Every
  "manual matrix" verify step names the responsible human on the
  release checklist; the manual set must shrink over time as UIA-driven
  scripts absorb entries, never grow silently.
