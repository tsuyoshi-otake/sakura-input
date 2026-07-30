# Sakura Input — Design Document

A Japanese Input Method Editor (IME) in the spirit of ATOK, **specialized
for IT engineers**: fast, accurate kana-kanji conversion with strong
personalization, built Windows-first on a portable core — implemented in
**pure Rust, from scratch**, with no third-party runtime dependencies and
the smallest achievable footprint.

Status: **v1.0 — final** (2026-07-31; hardened by three independent
adversarial review passes: TSF/installer feasibility, engine/performance
realism, product/plan consistency)

---

## 1. Goals

- **IT-engineer-first.** This is a specialist IME, not a generalist one:
  technical vocabulary (IT terminology, product names, katakana loanwords,
  English identifiers) is prioritized in conversion and prediction, and
  the classic engineer annoyance — unwanted full-width alphanumerics — is
  eliminated by a hard width policy (§2, §5.6).
- **Conversion quality first.** Multi-segment kana-kanji conversion whose
  accuracy on *technical Japanese* beats generalist IMEs, and that
  *improves with use* through per-user learning (ATOK's defining trait).
- **Pure Rust, full scratch.** Every component — TSF COM glue, tries,
  Viterbi, IPC codec, config parser — is hand-written Rust. No MeCab, no
  NLP/dictionary/serialization crates. The only permitted dependency is
  the `windows`/`windows-sys` FFI bindings (§3.1).
- **Fastest and lightest.** Every keystroke must feel instantaneous:
  ≤ 5 ms for kana composition, ≤ 20 ms for full conversion, ≤ 10 ms for
  prediction updates — with the smallest achievable footprint: engine
  private working set ≤ 15 MB (the dictionary is a file-backed mmap that
  stays out of private memory), zero heap allocation per keystroke at
  steady state. Budgets are enforced by tests (§10).
- **Never lose user text, never crash the host app.** The in-process
  component must be minimal and fail-safe; all real work happens
  out-of-process.
- **Local-first and private.** No network access in the core product. All
  learning data stays on the machine.
- **Portable core.** The conversion engine has zero platform dependencies so
  macOS (InputMethodKit) and Linux (fcitx5) frontends can be added later.

### Non-goals (v1)

- Cloud conversion / cloud sync, handwriting, voice input.
- Running on the secure desktop (UAC/login screen) — requires special
  signing arrangements; even ATOK does not do this.
- macOS/Linux frontends (design for them, don't build them yet).
- Mobile.
- **Non-admin environments.** v1 installs machine-wide and requires
  elevation — including updates (auto-update triggers one UAC prompt).
  Users without admin rights on managed machines cannot install v1; a
  per-user install / Chrome-style updater-service model is explicitly
  post-v1. Stated here so it is a decision, not an oversight.
- Generalist vocabulary parity (celebrity names, long-tail proper nouns):
  the general lexicon is deliberately trimmed for size (§6.1); the moat is
  technical coverage, learning, and speed — not breadth.

---

## 2. Product requirements (ATOK-parity baseline)

Input:
- Romaji→kana with a fully customizable mapping table (incl. `n`/`nn`
  handling, sokuon via consonant doubling, `xtu`, symbol mappings).
- Kana input mode (direct kana keyboard layout) as a later option.
- Input modes: Hiragana / Katakana / Half-width katakana / Full-width
  alphanumeric / Half-width alphanumeric / Direct.
- **Key map: Microsoft IME preset by default.** The key map is data
  (config-defined, editable in §8), shipped as presets — `ms-ime`
  (default) and `atok` — plus per-key overrides. The `ms-ime` preset
  follows Windows 11 Microsoft IME conventions:
  - 半角/全角 (Alt+` on US layouts): IME on/off.
  - 無変換: hiragana ⇄ katakana toggle; 変換: reconversion — both
    reassignable to IME-off/on, mirroring Win11's key-assignment options.
  - Space: convert; further Space / ↑↓: candidate navigation; Tab in the
    candidate window: expanded table view; Tab during typing: suggest
    focus (§5.3).
  - ←/→: segment focus move; Shift+←/→: segment resize.
  - F6–F10 and Ctrl+U/I/O/P/T: kana/width transforms.
  - Enter: commit; Escape: revert; Ctrl+Backspace: 確定の取り消し.
- **Alphanumeric width policy (spec-critical).** A setting pins the width
  of alphanumerics and symbols regardless of input mode:
  `alnum_width = half | full | follow_mode` (default **half** — engineers
  never want `ｄｏｃｋｅｒ`), independently `number_width` and
  `symbol_width`. The policy governs every place the IME *chooses* a
  width: typed text in alnum modes, conversion candidates containing
  alnum runs, prediction output, and commit-time normalization. An
  explicit F9/F10 keypress is direct user intent and still overrides it
  for that segment. Enforced at a single choke point in the engine
  (§5.6), so no code path can leak the wrong width.
- **Punctuation style.** `punctuation_style = kuten_touten (、。) |
  comma_period (，．) | mixed (、．)` — a first-class, frequently-toggled
  setting for engineers writing mixed EN/JP docs; enforced at the same
  §5.6 choke point as width.
- **Per-app profiles.** Match by process name (e.g. `WindowsTerminal.exe`,
  `Code.exe`, `devenv.exe`) → default input mode (e.g. direct/half-alnum
  in terminals), width-policy overrides, and **suggest on/off** —
  terminal profiles disable the suggest UI by default so Tab keeps
  meaning shell completion. A profile sets the *default* for newly
  created contexts only: an explicit in-session mode change by the user
  wins for the lifetime of that context and never silently resets on
  refocus.
- **Mode persistence.** Input mode is tracked per-process (Windows 11
  MS-IME default behavior), with a "same mode everywhere" global option.

Conversion:
- Space = convert; repeated Space / arrows = candidate navigation; Tab =
  predictive completion; Enter = commit; Escape = revert stages.
- Multi-segment conversion with segment resize (Shift+←/→) and per-segment
  candidate selection (←/→ to move focus).
- F6–F10 transforms (hiragana / katakana / half-kana / full-alnum /
  half-alnum), applied per segment. F9/F10 keep MS-IME semantics — the
  raw typed romaji, cycling case variants; semantic English surfaces
  (どっかー→Docker) are *conversion candidates* offered via Space (§5.6),
  never a silent F10 substitution.
- Candidate window with paging, annotations (POS, usage notes, homophone
  hints — ATOK's homophone guidance is a differentiator worth copying).
- Prediction ("suggest"): as-you-type candidates from prefix search over
  dictionary + user history, shown in a suggest list under the caret.
  **Tab is the primary acceptance key**: the first Tab focuses the top
  prediction, further Tab / Shift+Tab cycle forward/backward, Enter
  commits the focused one, Escape returns to the plain preedit, and
  continuing to type narrows the list. Shift+Enter commits the top
  prediction directly without entering the list. Space still triggers
  normal conversion at any point — prediction never blocks it. The
  suggest-accept key is remappable in the key-binding editor (§8), and
  per-app profiles can disable suggest entirely (default for terminals).
- **Tech-term conversion.** Kana readings resolve to English/katakana
  technical surfaces (どっかー→Docker, くばねてす→Kubernetes, ぎっと→git),
  identifier-case transforms (camelCase / snake_case / kebab-case) are
  offered on the focused segment, and IT-domain homophones outrank
  generalist ones (けいしょう→継承 before 警鐘, §5.6).
- **Recent-context conversion.** The result of converting the current
  preedit takes the last few *committed* inputs into account — grammatical
  left context carries over across commits, homophones the user just
  picked stay picked, and a session that is clearly about engineering
  biases harder toward technical terms (§5.8).
- Reconversion: select committed text, hit Henkan → re-enter conversion
  (TSF `ITfFnReconversion`).
- **Commit undo (確定の取り消し).** Ctrl+Backspace immediately after a
  commit pulls the committed text back into composition with its reading
  restored — same binding and behavior as Microsoft IME (ATOK's 確定
  アンドゥ is the same idea). Depth 1 in v1. The armed state expires on
  the next keystroke, caret move, or focus change — outside that window
  Ctrl+Backspace passes through to the app (delete-previous-word).
- **External edits during composition.** Paste or any app-initiated text
  change while a composition is active finalizes the current preedit
  first (committed as-is — composed text is never discarded, §1). A
  commit is applied as one atomic edit session, so an app-level Ctrl+Z
  undoes the whole committed run in a single step, not char-by-char.
- **Physical layouts: JIS and US are both first-class.** Every binding
  that assumes a JIS-only key (Henkan, Muhenkan, 半角/全角) has a
  configurable equivalent (Alt+` by default) so US keyboards lose no
  functionality. Ctrl+Space is deliberately **not** bound by default —
  it is IntelliSense in every major IDE, the target user's home turf —
  but remains available as an opt-in binding.
- Numpad digits always input directly as half-width, regardless of mode.
- User dictionary: register/edit words with reading, POS, comment.
- Learning: last-selected candidate per (reading, context) wins next time;
  frequency + recency boosting; segment-boundary corrections are learned.

Explicitly out of v1: live conversion (ATOK/macOS-style auto conversion
while typing) — the engine API is designed to support it, but ship it later
behind a flag.

---

## 3. High-level architecture

Mozc-proved three-process split. The component loaded into host apps is a
thin TSF DLL; the engine and UI live in separate per-user processes.

```
┌────────────────────────────── host app process ───────────────┐
│  app (Word, Chrome, Terminal, game, …)                        │
│  └── sakura_tsf.dll  (TSF text service, Rust/COM, tiny)       │
│        - key event → engine request                           │
│        - applies engine response to the edit context          │
│        - draws nothing, converts nothing, stores nothing      │
└───────────────┬───────────────────────────────────────────────┘
                │ named pipe (length-prefixed binary frames), per session
┌───────────────▼───────────────────────────────────────────────┐
│  sakura_engine.exe  (Rust, one per user session)              │
│   - romaji FSM, lattice builder, Viterbi/N-best, prediction   │
│   - system dictionary (mmap, read-only)                       │
│   - user dictionary + learning store (writable, single owner) │
└───────────────┬───────────────────────────────────────────────┘
                │ same IPC
┌───────────────▼───────────────────────────────────────────────┐
│  sakura_renderer.exe (candidate window, mode indicator)       │
│  sakura_settings.exe (settings UI, dictionary editor)         │
└───────────────────────────────────────────────────────────────┘
```

Why out-of-process (all four reasons matter):
1. **Bitness/arch.** The DLL loads into every process, so it ships
   per-arch: x64 + x86, plus ARM64X on ARM64 machines — a hybrid binary
   cargo cannot produce on its own; treated as an explicit R&D spike
   with a documented fallback (§14), not a routine build target. The
   engine ships once per machine arch.
2. **Crash isolation.** An engine bug kills a background process, not the
   user's document. The DLL degrades to pass-through on IPC failure.
3. **Shared state.** One engine process owns the learning store and user
   dictionary — no cross-process write contention, learning is instantly
   visible in every app.
4. **Sandboxed hosts.** Browsers/UWP run at low integrity and cannot host
   nontrivial code or open user files. The pipe's ACL explicitly grants
   access from AppContainer/low-IL clients; the DLL keeps no state.

The renderer is separate from the engine so a UI hang can't block
conversion, and separate from the host so candidate windows work even in
sandboxed/fullscreen apps (drawn as a top-level layered window positioned
via the text service's `ITfContextView` rects).

### Component inventory

| Component            | Language        | Runs in            | Responsibility                          |
|----------------------|-----------------|--------------------|-----------------------------------------|
| `sakura_tsf.dll`     | Rust (COM)      | every host app     | TSF glue, IPC client, zero logic         |
| `sakura_engine.exe`  | Rust            | per user session   | all conversion, dictionaries, learning   |
| `sakura_renderer.exe`| Rust (Win32)    | per user session   | candidate window, mode indicator, engine watchdog |
| `sakura_settings.exe`| Rust (Win32)    | on demand          | settings, user-dictionary editor         |
| `sakura_setup.exe`   | Inno Setup      | install/update     | installer (declarative script, §12)      |
| `sakura_regtool.exe` | Rust (Win32)    | install/update     | TSF/COM (de)registration helper (§12)    |
| `dictc`              | Rust            | build time         | dictionary compiler (TSV → binary image) |

### 3.1 Dependency policy (the full-scratch rule)

Everything is Rust, and everything is ours. The only crates allowed in any
shipping binary are `windows`/`windows-sys` — auto-generated Windows
API/COM bindings; that *is* the platform, not a library. Everything else
is `std`-only, hand-written:

| Instead of…              | We hand-roll…                                  |
|--------------------------|------------------------------------------------|
| MeCab / Vibrato / etc.   | our own lattice + Viterbi + connection matrix  |
| marisa / LOUDS crates    | our own LOUDS trie (builder + query)           |
| protobuf / serde         | fixed-layout binary IPC codec, versioned (§7)  |
| toml / config crates     | a minimal hand-written config parser           |
| smallvec / bumpalo       | per-session arena + inline buffers (§5.7)      |

TSF COM classes are implemented directly in Rust via the `windows` crate's
`implement` machinery. The reference material (Mozc, SampleIME) is C++, so
TSF patterns are ported by *reading*, never by linking. Build profile:
`panic = "abort"`, fat LTO, `codegen-units = 1`, no `unwrap` in the DLL;
DLL binary target ≤ 1 MB.

The rule governs shipping *runtime* binaries. Build and packaging tooling
is exempt — in particular, the installer is Inno Setup (§12), because
commodity install machinery is not worth reimplementing.

---

## 4. Platform integration layer (Windows / TSF)

### 4.1 TSF surface to implement

- `ITfTextInputProcessorEx` — activation/deactivation per thread manager.
- `ITfThreadMgrEventSink`, `ITfTextEditSink`, `ITfTextLayoutSink` — focus,
  edit, and layout change tracking (layout → reposition candidate window;
  also the retry signal for `TS_E_NOLAYOUT`, §4.4).
- `ITfKeystrokeMgr::PreserveKey` / `OnPreservedKey` — IME on/off and
  mode keys are registered as *preserved keys*, the documented TSF
  mechanism for global TIP hotkeys: they work with no focused editable
  document and before app-level accelerators can steal them.
- `ITfActiveLanguageProfileNotifySink` — detect the user switching to
  another TIP (Win+Space): hide renderer windows, end the engine
  session, release state — never leave an orphaned candidate window.
- `ITfKeyEventSink` — the hot path. Every key: ask engine "do you consume
  this?" then apply the result inside an `ITfEditSession`.
- `ITfComposition` / `ITfCompositionSink` — preedit lifecycle. Preedit is
  rendered by the *host* via composition display attributes (underline
  styles for raw/converting/focused segment) — this is what makes preedit
  appear inline in Word, terminals, Electron, etc.
- `ITfFnReconversion` — reconversion of committed text.
- `ITfDisplayAttributeProvider` — segment underline/highlight styles.
- `InputScope` reading (`ITfInputScope`) — detect password fields
  (`IS_PASSWORD`) → force direct input, disable learning and prediction.
- Registration: `ITfInputProcessorProfileMgr` with
  `TF_IPP_CAPS_IMMERSIVESUPPORT | TF_IPP_CAPS_SYSTRAYSUPPORT` (UWP + Win11
  requirements), language `ja-JP`, category `GUID_TFCAT_TIP_KEYBOARD` plus
  UI-less mode categories (`GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT`,
  `GUID_TFCAT_TIPCAP_UIELEMENTENABLED`). The exact category set is
  enumerated in one place (`sakura-reg`, the single source of truth);
  secure-desktop capability (`GUID_TFCAT_TIPCAP_SECUREMODE`) is
  deliberately **not** registered, matching the §1 non-goal.

### 4.2 Known app-compat hazards (test matrix from day one)

- **UI-less mode** (fullscreen games, some UWP): the system may suppress
  our candidate UI and query it via `ITfUIElement` — candidate list must be
  exposed as data, not only as our own window.
- **IMM32-only legacy apps**: serviced through the OS's IMM/TSF bridge;
  verify preedit + candidate behavior in at least one (e.g. old notepad
  clones, some games).
- **Consoles**: Windows Terminal and conhost have distinct TSF paths.
- **Electron/Chromium**: composition events are the most common source of
  bug reports for every IME; keep a Chromium-specific test page.
- **RDP / VDI**: input service runs remotely; nothing special to do but
  must be tested.
- **Touch/on-screen keyboard**: the tablet keyboard has its own
  suggestion UI and TSF interplay; verify coexistence, don't fight it.
- **Elevated hosts (admin terminals, Task Manager, elevated VS):** the
  TIP runs inside a High-IL process while renderer/engine run at medium
  IL; UIPI changes window/message behavior across that boundary. The
  target user lives in admin shells — elevated hosts are in the M0 test
  matrix, not the release checklist.
- **IDE key collisions:** Ctrl+Space (IntelliSense) and Tab (snippets,
  shell/path completion) are owned by the persona's daily tools —
  defaults avoid them (§2) and terminal/IDE profiles ship suggest-off.
- **DPI change mid-composition** (undocking, RDP resize): the candidate
  window must rescale/reposition live, not on next open.

### 4.3 Failure policy

- **The engine is persistent, not on-demand.** Engine + renderer start
  at logon (launcher task, §7) and stay resident — the engine is ≤ 15 MB
  and mostly idle. A sandboxed (AppContainer/low-IL) DLL instance cannot
  reliably start processes or activate Task Scheduler's COM server, so
  the DLL *never* spawns anything; the renderer (normal IL) is the
  watchdog that restarts a crashed engine. This also removes the
  cold-start trap: a 150 ms cold start (§10) can never collide with the
  50 ms per-keystroke budget, because cold start happens once per logon,
  not after every idle timeout.
- **Timeouts.** The DLL never blocks a keystroke for more than 50 ms
  (steady-state IPC timeout) — on timeout, pass the key through. A
  reconnect after an engine crash gets one bounded 200 ms grace on the
  first key. Timeouts are counted and surfaced in settings diagnostics.
- **Stale responses.** Every frame carries a monotonic request id (§7).
  After any timeout the DLL discards responses with stale ids and
  resyncs session state from the engine before trusting new ones — a
  late "commit X" must never land on top of text the app already
  received raw.
- **Crash with live preedit.** The DLL owns the currently displayed
  preedit string (it rendered it). If the engine dies mid-composition,
  the DLL finalizes the composition with exactly that text — composed
  text is never dropped (§1). Then: pass-through until the watchdog
  restarts the engine, plus a one-time balloon if the renderer is
  reachable.
- **Composition denial / teardown.** Some hosts reject compositions
  (`ITfContextOwnerCompositionSink` can refuse `StartComposition`) —
  fall back to commit-as-you-go, no preedit. On focus loss (alt-tab, a
  popup stealing focus) or layout teardown with a live composition, the
  composition is finalized in place (committed as-is) — never silently
  discarded. Both paths are in the §4.2 test matrix.

### 4.4 TSF implementation discipline (hard-won rules, stated up front)

- **The edit-session lock can fail.** `RequestEditSession(TF_ES_SYNC)`
  may be refused when the document is already locked, and `OnKeyDown`
  must decide "consumed" *before* knowing whether the edit will apply.
  Policy: claim keys the IME would handle, fall back to an async edit
  session when sync is refused, and reconcile (roll composition state
  back) if the async session ultimately fails.
- **`TS_E_NOLAYOUT` is normal, not an error**: `GetTextExt` fails until
  the host has computed layout for the range. The candidate window
  defers positioning and retries on the next `OnLayoutChange` — it never
  draws at (0,0) or a stale rect.
- **No re-entrant TSF calls from TSF callbacks** (Cicero re-entrancy):
  never request new edit sessions or touch thread-manager state from
  inside `OnEndEdit` / `OnLayoutChange` / composition sinks — defer via
  a posted message to the next message-loop iteration. Every C++ TSF
  codebase learned this the hard way; we inherit the rule, not the
  bruises.
- **COM vtables take `&self`** — all TIP state lives behind interior
  mutability. Borrows are scoped tightly around each callback and never
  held across a call back into TSF (edit sessions re-enter synchronously
  on the same stack). With `panic = "abort"`, a `RefCell` double-borrow
  is a host-app crash — so this rule is enforced by review plus a
  debug-build borrow-tracking assertion.

---

## 5. Conversion engine

The engine is a deterministic, replayable state machine: `(session state,
key event) → (new state, output commands)`. All randomness-free, all
testable without Windows.

### 5.1 Input pipeline

```
key event
  → mode dispatch (direct? pass through)
  → romaji FSM (table-driven, longest-match, pending-input aware)
  → preedit (reading string, cursor)
  → [prediction query on every change]
  → [conversion on Space]
```

Romaji FSM: a compiled trie of the mapping table. Each entry:
`input_seq → (output kana, carry-over)`, e.g. `tt → っ + carry "t"`,
`n` + consonant → `ん`. The table is data (TOML), user-overridable;
compile to the trie at load. This single mechanism also covers AZIK-style
custom layouts for free.

### 5.2 Kana-kanji conversion — lattice + Viterbi (Mozc/MeCab lineage)

This is the proven architecture; do not innovate here in v1 —
algorithm-wise. The *implementation* is 100 % ours: no MeCab, no external
tokenizer, trie, or model crates (§3.1).

1. **Lattice build.** For every position in the reading, common-prefix
   search into the system dictionary, user dictionary, and learned words
   yields candidate word edges `(surface, reading, left_id, right_id,
   word_cost)`. Add synthetic edges: numbers (arabic/kanji/counters —
   counter phonetics are irregular, 一本 いっぽん / 三本 さんぼん /
   六本 ろっぽん, and get their own small reading table), single-kana
   fallback (so conversion never fails), katakana transliteration,
   date/symbol converters, and **OOV spans grouped by character-type
   run** (hiragana/katakana/latin/digit transitions) with a calibrated
   cost — an unknown multi-char term becomes one plausible guess-segment
   instead of fragmenting into single kana. This matters more here than
   in Mozc, because the deliberately trimmed lexicon (§6.1) raises the
   OOV rate for exactly the terms we specialize in.
2. **Search.** Viterbi over `word_cost + connection_cost(right_id_prev,
   left_id_next)`. Connection matrix is a class-bigram cost table,
   memory-mapped. The class count is **frozen at ~1.3 k (Mozc's own
   taxonomy) before any `dictc` work starts** — a flat `u16[classes²]`
   is then ≈ 3.4 MB, a named line item in the §10 budget. Class growth
   costs quadratically (4 k classes = 32 MB, nearly the whole dictionary
   budget), so any expansion requires matrix compression first, never
   budget creep.
3. **N-best.** A* backward enumeration for the candidate list per segment
   and for whole-sentence alternates.
4. **Segments.** The Viterbi path induces bunsetsu segmentation. Segment
   resize (Shift+←/→) re-runs the search with the user's boundary pinned
   **and every other segment boundary pinned too** — only the resized
   segment and its immediate neighbor may change. A local resize must
   never resegment distant parts of the sentence (a classic converter
   UX failure; snapshot-tested, §11). The correction is recorded by
   learning.
5. **Per-segment candidates.** For the focused segment, enumerate N-best
   words spanning exactly that reading span, re-scored with left/right
   context fixed. Attach annotations (homophone notes, POS) from the
   dictionary's annotation table.

Personalization enters as **cost adjustments layered on top** (§5.4), never
by mutating the base dictionary.

### 5.3 Prediction

Two sources, merged and deduped, re-ranked:
- **Dictionary prediction:** predictive trie search (reading prefix →
  frequent words/phrases; the dictionary build marks prediction-worthy
  entries with a separate precomputed cost).
- **History prediction:** user's committed strings, keyed by reading
  prefix, with recency-weighted scores; n-gram continuation ("the user
  typed 東京, last time they continued with 都庁") for next-word prediction.

Interaction model: the suggest list is a separate UI state from
conversion candidates, driven by Tab (§2): Tab focuses/cycles, Shift+Tab
cycles backward, Enter commits, Escape falls back to the plain preedit.
The engine Output distinguishes `suggest` from `candidates` so the
renderer can draw them differently (inline dropdown vs. full candidate
window), and so UI-less hosts can query them separately.

Budget: prediction must return in ≤ 10 ms; it runs on every preedit
change, so it gets **one persistent thread** in the engine. Coalescing is
a hand-rolled single-slot atomic mailbox — the newest query overwrites a
pending one; no channel, no allocation on the hand-off, so the zero-alloc
keystroke invariant (§5.7) survives prediction. When a per-app profile
disables suggest (§2), the engine skips prediction work for that session
entirely.

### 5.4 Learning (the ATOK-ness)

Storage: append-only log + in-memory index, compacted on idle; all under
`%LOCALAPPDATA%\SakuraInput\learning\`. The log is the source of truth:
a missing or corrupt index is rebuilt from the log at startup, so a crash
costs at most the entries since the last flush — never the store. Every
record is length- and checksum-prefixed, so a torn trailing write (crash
or disk-full mid-append) is detected and truncated, never treated as
whole-log corruption. On `ERROR_DISK_FULL` the engine degrades
gracefully: skip the learning write, count it in diagnostics, keep
converting. Both the log format and `config.toml` carry a format-version
field; parsers default unknown/missing fields instead of failing to
load, and upgrades are tested against previous-version files (§11) — a
format change must never brick the IME or silently discard years of
learning.

Signals recorded on every commit:
- `(reading, chosen surface, left/right word context)` — homophone choice.
- Segment boundary corrections (reading span → chosen segmentation).
- Committed sentences (for history prediction), unless input scope is
  sensitive.

Application at conversion time, in priority order:
1. Exact `(context, reading) → surface` match: large negative cost bonus
   (effectively "last choice wins").
2. `(reading) → surface` frequency/recency score: moderate bonus with
   exponential decay (half-life ~30 days).
3. Learned segmentation constraints: soft boundary bonuses.

Caps and hygiene: bounded store — LRU beyond ~100 k entries at a packed
index budget of ≤ 64 B/entry, i.e. ≤ ~8 MB of the 15 MB private-WS
budget, reconciled by arithmetic in §10's table — never learn in
password/private scopes, one-click "clear learning data", exportable
(export/clear are shipped, tested features, not aspirations — §11).

### 5.5 Engine API (IPC-visible, platform-agnostic)

Session-oriented protocol (hand-rolled binary schema, versioned):

```
CreateSession() → session_id
SendKey(session_id, key, modifiers, input_scope) → Output
  Output = { consumed: bool,
             preedit: { segments: [ {text, underline_kind} ], cursor },
             candidates: { list, focused, page } | absent,
             commit_text | absent,
             mode | absent }
GetCandidates / SelectCandidate / ResizeSegment / Revert / Commit
Reconvert(text) → same Output shape
DeleteSession
```

The same API serves macOS/Linux frontends later; nothing in it is
TSF-shaped. The DLL is a dumb translator between this protocol and TSF
edit sessions.

### 5.6 IT-domain ranking layer (the specialization)

Specialization is a distinct cost layer, exactly like personalization
(§5.4) — the base model stays generic, priors sit on top:

- **Domain prior table.** Entries tagged `IT` in the dictionary image
  (from the curated overlay, §6.2) receive a negative cost bonus, so
  technical homophones win by default: かんすう→関数, けいしょう→継承
  (not 警鐘), いこう→移行 (not 意向), こんぱいる→コンパイル.
- **English-surface candidates.** Readings map to raw ASCII surfaces
  (どっかー→Docker, くばねてす→Kubernetes/k8s, ぎっと→git). Casing
  variants (docker / Docker / DOCKER) are *generated*, not stored, with
  the user's casing preference learned per word.
- **Identifier-case transforms.** On the focused segment, offer
  camelCase / snake_case / SCREAMING_SNAKE / kebab-case renderings of its
  English surface — pure string transforms, no dictionary entries needed.
- **Width policy choke point.** All output text (conversion, prediction,
  F-key transforms, reconversion) passes one width normalizer immediately
  before leaving the engine, applying the §2 alnum/number/symbol width
  policy. One enforcement point means no leak paths.
- **Cost algebra is specified, not vibes.** Every layered bonus (domain
  prior here, learning §5.4, commit cache §5.8) is a *bounded
  proportional discount* on the entry's base cost — never a flat
  constant, which over-corrects common words and barely moves rare ones.
  Precedence is structural, enforced by clamp ordering: **exact-context
  learned choice > commit cache > domain prior** — an automatic prior
  can never override what the user explicitly chose; a composed-layers
  test asserts this (§11). N-best's A* heuristic is recomputed per query
  from the *boosted* lattice, so a bonus can never make the heuristic
  inadmissible and prune the very path it was meant to promote.
- **Homophone override guard.** IT-favored homophones whose rival
  reading is everyday vocabulary (警鐘を鳴らす, ご意向) are *not* flipped
  by the flat prior alone — they flip only when the recent-context
  domain-coherence signal (§5.8) confirms a technical session. The
  general corpus slice carries explicit anti-regression cases for these.

### 5.7 Memory & allocation engineering

- **Zero allocation on the hot path — scope stated precisely.** The
  guarantee covers three paths: the kana path, the conversion path
  (Space → Output), and the prediction hand-off (§5.3). Preallocated
  buffers, a per-session lattice arena that is reset, not freed, and
  **fixed-capacity Output/IPC buffers with defined truncation** — no
  growable strings on any covered path. The reading buffer is capped at
  128 chars (further input is rejected with a beep, matching MS-IME's
  own bound). A counting allocator asserts the invariant in CI on all
  three paths, not just kana typing (§10, §11).
- **The dictionary never enters private memory.** The image is a
  file-backed read-only mmap accessed through fixed-layout views — no
  deserialization at load, no owned copies. Lookups traffic in offsets;
  strings are materialized only into the final Output. The flip side:
  clean file-backed pages are the first thing Windows evicts under
  memory pressure, so the engine re-touches a small hot set (top trie
  levels, connection matrix) on an idle timer. The 20 ms conversion
  budget (§10) is a *warm-cache* number; post-eviction conversions are
  tracked separately as informational, not gated.
- **Compact sessions.** A session is a few KB: reading buffer, segment
  table, candidate references (offsets, not strings). Tries and the
  connection matrix are shared, file-backed pages — one engine process
  serves all apps, so the machine pays once.

### 5.8 Recent-context conversion

"Consider what I just typed": conversion of the current preedit is biased
by the last few committed inputs. Three mechanisms, all cheap:

- **Left-context carryover.** A new conversion does not start from a
  generic beginning-of-sentence state. The engine remembers the right
  connection id of the tail word of the previous commit in the same
  session and seeds the Viterbi's initial connection cost with it.
  Committing 「医者に」 and then typing 「いった」 scores 行った above
  言った because the left context 「に」 survives the commit boundary.
  Carryover is **gated**: it resets on sentence-final punctuation
  (。！？) and sentence-final polite forms, on explicit mode switches,
  and on focus change — grammatical context must not leak across true
  sentence boundaries, where it is usually wrong.
- **Commit cache (recency bonus).** A per-session ring buffer holds the
  last N commits (default 8, ≈500 chars, in-memory only). Words whose
  surface appears in the buffer get a decaying cost bonus: once you have
  typed コンテナ, the next こんてな converts the same way, and a homophone
  you just picked stays picked for the rest of the writing session — even
  before the persistent learning (§5.4) has recorded it. **Commit undo is
  a negative signal**: Ctrl+Backspace (§2) evicts the undone entry from
  the buffer and rolls back the carryover state — the cache must never
  re-suggest the exact choice the user just retracted, and a fast
  Tab-accepted mistake must not echo through the next eight commits.
- **Domain coherence.** The buffer's ratio of `IT`-tagged words
  dynamically scales the IT-domain prior (§5.6): a session that is
  clearly about engineering pushes technical homophones harder; a prose
  session relaxes back toward the general model. The multiplier is
  explicitly capped: "IT words → stronger prior → more IT words" is
  positive feedback by construction, and the cap — not the buffer size —
  is the safety bound.

The buffer is volatile by design: in-memory only, never persisted, capped,
cleared on session end or explicit request, and always bypassed in
sensitive input scopes (§9). Persistent influence is the learning store's
job (§5.4); this layer is deliberately short-lived so it can be aggressive
without polluting long-term state.

---

## 6. Dictionaries and data

### 6.1 System dictionary

- **Sources.** The full-scratch rule applies to *code*, not corpus data:
  we compile open TSV *data* with our own `dictc` — Mozc's dictionary
  data (BSD; includes readings, POS ids, corpus-derived costs, and the
  connection matrix) as the seed, optionally SudachiDict (Apache-2.0).
  No MeCab binaries, no external dictionary formats at runtime. License
  audit is a build-time gate; no ATOK/proprietary data ever.
- **Deliberately trimmed general lexicon.** Rare proper nouns, celebrity
  names, and long-tail generalist entries are dropped to hit the size
  target; domain coverage comes back via the IT overlay (§6.2). A
  general-Japanese accuracy floor is tracked so trimming never breaks
  everyday text (§11).
- **Format:** compiled by `dictc` from TSV into one read-only,
  memory-mapped, versioned image:
  - a versioned **table directory** in the header — each section is a
    tagged offset/length and readers skip unknown tables, giving the
    on-disk format the same forward-compat treatment as the IPC
    protocol (§7); adding a table later must never force rework of the
    Phase 2 reader and its fuzzer,
  - readings: LOUDS trie (or `marisa`-style patricia trie) keyed by kana,
  - values: packed `(surface_ref, left_id, right_id, cost, flags)`
    arrays — including the **prediction-worthiness cost §5.3 needs, from
    day one**, so prediction (Phase 4) does not force a format bump,
  - surfaces: front-coded string pool,
  - connection matrix: flat `u16[classes²]` (~1.3 k classes frozen, §5.2),
  - annotations: side table keyed by entry id.
- Target size: ≤ 35 MB on disk (trimmed lexicon + IT overlay), mapped
  lazily; cold-start to first conversion ≤ 150 ms.
- Update mechanism (v1, deliberately boring): dictionary updates ship
  with releases. The installer runs `regtool --stop` (the engine exits,
  releasing its mmap), replaces `system.dic` atomically, and the
  watchdog restarts the engine — no reboot needed for the dictionary, no
  host-app involvement, and no live hot-swap machinery to build. Only
  the in-use TSF DLL itself needs the reboot path (§12.3).

### 6.2 IT-term overlay dictionary (the specialization asset)

A hand-curated, in-repo TSV (`data/it-terms.tsv`) — this file is the
product's moat and is expected to grow forever.

**Primary seed: the smile-chat glossary.** The in-house glossary shipped
with smile-chat (`frontend/public/glossaries`; ~9,650 Japanese entries,
~7,900 with kana readings; domain-tagged: software-engineering, security,
cloud/AWS/OCI, database, programming, project-management, …) is converted
by a one-shot importer: `term` + `reading` → dictionary entries,
`normalizedTerms` English aliases → English-surface candidates
(くらうど→cloud), `domain` → the `IT` tag plus finer facets, and
definition texts → candidate annotations (§8). In-house data — licensing
is a non-issue.

**The hard part of the import is not parsing — it is
POS/connection-id/cost assignment**, which the glossary does not carry.
A wrong or uniform default class means an entry either never surfaces or
surfaces ungrammatically, and a flat default cost makes ranking *inside*
the IT vocabulary meaningless (Docker must outrank an obscure acronym).
So: bootstrap left/right ids and costs by surface/reading match against
Mozc's dictionary where they overlap; the remainder get class-by-shape
defaults (katakana noun / ASCII noun) with corpus-estimated frequencies.
English/loanword terms additionally need **phonetic reading variants**
curated per term (くばねてす／くーばねてぃす → Kubernetes) — loanword
readings have no canonical form, and a missed variant is a hard zero for
that lookup. Both are explicitly budgeted Phase 2 tasks, not "curation
polish." Curation continues on top of this seed:

- katakana tech terms with readings (コンテナ, 冪等性, 疎結合, 排他制御),
- English/ASCII surfaces with kana readings (Kubernetes / k8s ←くばねてす,
  nginx ←えんじんえっくす, PostgreSQL ←ぽすぐれ),
- abbreviations and initialisms as first-class surfaces (k8s, i18n, a11y,
  LGTM, CI/CD),
- engineering homophones pinned to the technical sense (継承, 移行, 実行,
  更新, 環境変数, 型推論 …),
- product/framework/company names with canonical casing (GitHub, iOS,
  PyTorch — casing is data, not guesswork).

Compiled into the same binary image with the `IT` domain tag (§5.6);
overlay entries outrank general entries at equal reading. Contribution is
a first-class path: plain TSV plus CI lints (duplicates, reading validity,
license cleanliness) — no tooling needed beyond `dictc`.

### 6.3 User dictionary

Human-editable source of truth (TSV/JSON in the profile dir), compiled to a
small trie in memory at load; edited via `sakura_settings.exe`, hot-reloaded
by the engine on file change. Users pick POS from a small curated
picklist (~20 categories); a hand-tuned table maps each to internal
connection classes (Mozc maintains the same artifact) — users never see
raw class ids, and no entry gets a generic class that silently breaks
grammatical connections.

### 6.4 Data directory layout

```
%ProgramFiles%\SakuraInput\
  bin\...                    (per-arch exes + DLLs, §12)
  dict\system.dic            (shared read-only image, one copy per
                              machine, mmap-shared across users; swapped
                              atomically by the installer on upgrade)
%LOCALAPPDATA%\SakuraInput\
  userdict\user.tsv
  learning\log-*.bin, index.bin
  config\config.toml         (key bindings, romaji table, width policy)
  logs\engine.log            (no text content ever — events/timings only;
                              rotated at 5 MB × 2 files — bounded forever)
```

Per-user directories hold only user-owned data; everything shared and
read-only lives once per machine under Program Files.

---

## 7. IPC

- Transport: named pipe `\\.\pipe\sakura_input_<session_sid>`, one engine
  per interactive session. Security descriptor: DACL allows the user's
  SID + `ALL APPLICATION PACKAGES`, **plus an explicit low-integrity
  mandatory-label SACL (`S:(ML;;NW;;;LW)`)** — integrity is checked *in
  addition to* the DACL, and a pipe without the label defaults to Medium
  and rejects every sandboxed writer no matter what the DACL grants (the
  single most common named-pipe-vs-sandbox bug; §3's architecture
  depends on getting it right). CI verifies by connecting from a real
  AppContainer token. Remote (SMB-originated) clients are rejected by a
  local-only check in the engine — the DACL alone does not distinguish
  local from remote.
- Framing: 4-byte length + hand-rolled fixed-layout binary messages
  (explicit little-endian layout, no codegen, no reflection; the codec is
  a few hundred lines of plain Rust with exhaustive round-trip and fuzz
  tests). Every request carries a protocol version **and a monotonic
  per-session request id** — the stale-response guard of §4.3; a named
  pipe is a byte stream, and without correlation ids a late reply to a
  timed-out request would be mis-attributed to the next one. Engine
  answers `UNSUPPORTED_VERSION` rather than guessing — the DLL then
  pass-throughs (mixed versions happen mid-update).
- Strings cross the pipe as UTF-8; the DLL converts to/from UTF-16 at the
  TSF boundary, surrogate-pair-safe (non-BMP input is a standing test
  case, §11).
- Engine lifecycle: engine + renderer start **at logon** (per-user
  launcher task) and stay resident; the renderer is the watchdog that
  restarts a crashed engine (§4.3). The DLL never launches processes — a
  sandboxed instance can't reliably do so, and shouldn't. On idle the
  engine trims caches instead of exiting: an idle exit would re-pay the
  cold start against a 50 ms keystroke budget — a scheduled failure, not
  an optimization. Learning is flushed periodically and at logoff.
- Security stance: the engine treats the pipe as a hostile boundary —
  strict input validation, per-message size caps, fuzzed (§11).

---

## 8. UI

- **Candidate window** (`sakura_renderer.exe`): borderless layered window,
  DirectWrite text, positioned from the composition's screen rect
  (received via the DLL → engine → renderer), never steals focus,
  per-monitor DPI aware (including DPI changes mid-composition, §4.2).
  Shows: candidates with shortcut digits, page indicator, annotation
  pane (homophone hints) on the side, toggled by a key. Vertical-writing
  hosts (Word 縦書き) report rotated layout rects — placement logic
  handles both orientations. The window is exposed to UI Automation
  (screen readers announce candidates), alongside the `ITfUIElement`
  data path for UI-less hosts.
- **Mode indicator**: small floating "あ/A" near the caret on mode change
  (Win11-style), plus tray icon.
- **Settings**: key binding editor (presets: `ms-ime` default / `atok`,
  per-key overrides), romaji table editor, dictionary
  manager (user dict CRUD, import/export ATOK/Mozc/MS-IME formats),
  learning data viewer/reset, diagnostics (IPC timeouts, engine version).
- All UI text localized ja/en.

---

## 9. Security and privacy

- IME sees everything the user types — treat the whole codebase as
  security-sensitive. No network capability in DLL/engine/renderer
  (enforced: no networking crates linked; CI check).
- Password fields (`IS_PASSWORD` input scope): direct input mode forced,
  no prediction, no learning, no logging, and the recent-context buffer
  (§5.8) is neither read nor written.
- The recent-context buffer is memory-only and dies with the session; it
  is never written to disk.
- Logs contain events and timings, never text.
- Crash handling: local minidumps via WER LocalDumps for our processes,
  never uploaded automatically; dumps are treated as sensitive since they
  may contain composition text. Retention is capped (`DumpCount=5`) —
  dumps are pruned, never accumulated unboundedly.
- Learning store is plain local data under the user profile; document how
  to wipe it. Consider DPAPI encryption at rest (decide by M3).
- Binaries signed; installer per-machine but all state per-user.

---

## 10. Performance budgets (enforced in CI)

| Operation                              | Budget (p99) |
|----------------------------------------|--------------|
| Key → Output engine round-trip (kana only) | 5 ms     |
| Space → full conversion, 30-char reading (warm cache) | 20 ms |
| Prediction update per keystroke        | 10 ms        |
| Candidate page render                  | 8 ms         |
| Engine cold start at logon → ready     | 150 ms       |
| New app session on a warm engine       | 5 ms         |
| DLL IPC timeout (hard cap, then pass-through) | 50 ms |

Engine benches run on the golden corpus. **Absolute budgets are asserted
on named reference hardware (a dedicated self-hosted runner); shared CI
runners gate only relative regression against a rolling baseline** —
shared-runner variance would make hard millisecond gates chronically
flaky, and a flaky gate is a gate people learn to ignore. The 5 ms row
is the *engine round-trip*; time-to-glass additionally depends on the
host's message pump and is measured separately under synthetic host load
(§11). Cold start is measured on a freshly-installed image (Defender
first-touch scan included); AV outliers are recorded, not budgeted.

Memory budgets (steady state, measured in CI on the reference VM):

| Metric                                        | Budget  |
|-----------------------------------------------|---------|
| `sakura_tsf.dll` binary size                  | ≤ 1 MB  |
| Engine private working set (dict mmap excluded, file-backed) | ≤ 15 MB |
| Renderer private working set                  | ≤ 10 MB |
| Heap allocations per keystroke (steady state, kana + conversion + prediction hand-off) | 0 |
| Dictionary image on disk                      | ≤ 35 MB |
| — of which connection matrix (~1.3 k² × u16, §5.2) | ≤ 4 MB |
| Learning index in memory (≤ 64 B × 100 k, §5.4) | ≤ 8 MB |

---

## 11. Testing strategy

1. **Engine unit tests** (pure Rust, no Windows): FSM tables, lattice,
   Viterbi, N-best, segment resize, learning application. Deterministic
   replay: a session is a list of key events → snapshot-test the full
   Output stream. Property tests include non-BMP / surrogate-pair
   round-trips end-to-end.
2. **Conversion accuracy regression**: golden corpus of (reading →
   expected top-1 conversion) pairs, target sentence accuracy tracked over
   time; any change to costs/dictionary shows a diff report, not just
   pass/fail. The corpus is weighted toward technical Japanese (commit
   messages, design docs, engineering chat) with a general-Japanese slice
   guarding the trimming floor (§6.1); the IT slice has its own, stricter
   target. The corpus is **split into a tuning set and a held-out
   grading set — gates are computed on held-out data only**, so costs
   and overlay entries cannot be tuned (even unintentionally) against
   the sentences that grade them. The general slice carries explicit
   anti-regression cases for IT-favored homophones in everyday use
   (警鐘を鳴らす, ご意向 — §5.6). Accuracy-vs-Mozc gates run against a
   **checked-in baseline file** with a documented regeneration
   procedure — CI never needs Mozc installed.
3. **Fuzzing**: IPC message fuzzer against the engine; romaji FSM fuzzer;
   dictionary image parser fuzzer (an attacker-supplied `system.dic` must
   not crash the engine).
4. **TSF integration**: scripted UIA tests against Notepad + a Chromium
   test page + Windows Terminal + an elevated console; a CI check
   connects to the engine pipe from a real AppContainer token (§7).
   Manual matrix (Word/Excel incl. 縦書き, Electron apps, one game, RDP,
   DPI change mid-composition) per release — each manual entry is a
   *named human-sign-off exception* to the scripted-verification rule,
   shrinking over time as UIA scripts absorb entries.
5. **Latency tests**: budgets of §10 asserted on reference hardware.

---

## 12. Packaging, registration, updates

### 12.1 Installer: Inno Setup + a tiny Rust registration helper

The full-scratch rule (§3.1) covers runtime components, not packaging
tooling. The installer is a standard Inno Setup script
(`installer/setup.iss`) producing `sakura_setup.exe`. Inno provides the
commodity machinery — file copy with rollback, upgrade detection
(AppId), ARP entry, in-use-file handling (`restartreplace`), silent
flags, SignTool hooks — none of which is worth reimplementing.

Everything IME-specific stays in Rust: `sakura_regtool.exe`, a small
helper built on the shared `sakura_reg` crate (single source of truth,
also used by the DLL's `DllRegisterServer` for the regsvr32 dev
workflow). Its commands:

- `--register` / `--unregister`: COM `CLSID` entries in both registry
  views, `ITfInputProcessorProfileMgr::RegisterProfile`, and the §4.1
  category (de)registrations. `--unregister` is internally ordered to
  fail safe: the language profile — the thing that lets Windows *try to
  activate* the TIP — is removed first, CLSID entries last. A partial
  failure can strand dead registry keys, but never a live profile
  pointing at a missing DLL.
- `--enable-profile`: per-user `InstallLayoutOrTip` + engine launcher
  task (§7); run once per user via a lightweight logon stub. For the
  installing user it must run **as the actual interactive user — never
  under the elevated installer's token**: under "run as different
  user", SCCM/Intune, or SYSTEM deployment, the elevated process's HKCU
  is a *different hive*, and writing there silently enables the IME for
  the wrong account while install reports success. The installer
  resolves the console session (`WTSQueryUserToken` +
  `CreateProcessAsUser`) or defers entirely to the logon stub. The stub
  also **self-repairs**: at each logon it verifies the TSF registration
  is intact and re-registers if a Windows feature update wiped it — a
  documented failure mode for third-party IMEs.
- `--stop`: ask engine/renderer to exit over the pipe (used before
  upgrade/uninstall), then terminate stragglers.

### 12.2 Install / uninstall flow

Install (`[Files]` + `[Run]`, in order): copy per-arch payload (x64 +
x86 + ARM64X DLLs, exes, `dict\system.dic`, third-party license texts
for the dictionary data) → `regtool --register` →
`regtool --enable-profile`.

Uninstall ordering is safety-critical — a stale TSF registration
pointing at a deleted DLL bricks text input — and Inno's
`[UninstallRun]` executes *before* file removal, which is exactly the
ordering we need:

1. `regtool --unregister` (TSF profile + categories first). The
   uninstaller **halts on a nonzero exit code** instead of continuing to
   file removal — ordering alone is not atomicity, and continuing past a
   failed deregistration is precisely the brick scenario.
2. `regtool --stop`.
3. Inno removes files; in-use DLLs are queued for delete-on-reboot and
   the reboot-required state is reported.
4. User data under `%LOCALAPPDATA%` is kept unless the user opts into
   purge (uninstall page checkbox / `/PURGE=1`).

CI exercises this path on VM snapshots every release: install → type →
uninstall → verify typing still works.

### 12.3 Upgrade & auto-update

- In-place upgrade: same AppId; `PrepareToInstall` runs
  `regtool --stop`; engine/renderer exes and the dictionary image are
  replaced atomically (§6.1 — the engine is stopped, so nothing maps
  them). The TSF DLL is different: it sits loaded in nearly every
  process that ever focused a text field, so **a reboot is the *normal*
  completion of an update, not a rare fallback** — `restartreplace`
  queues the swap and IPC version negotiation (§7) keeps
  old-DLL/new-engine combinations safe until then. On RDS/Citrix the
  pending rename is machine-global: one update implies a reboot
  affecting every logged-on user — documented in the admin guide.
- Updates require elevation (machine-wide install; §1 non-goals): the
  auto-updater triggers one UAC prompt. Non-admin users get a "new
  version available" notice to hand to their admin instead of a
  silently failing update.
- Auto-update (M4): the settings app checks GitHub Releases over
  WinHTTP, verifies Authenticode signature + hash, and runs the
  installer silently. Strictly opt-in — network code exists *only* in
  the settings/updater component, so the §9 no-network rule for DLL,
  engine, and renderer is unaffected.

### 12.4 Silent operation & distribution

- Standard Inno flags: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART
  /RESTARTEXITCODE=3010 /LOG=<path>`, plus `/PURGE=1` on uninstall.
- Every CI build emits a signed installer (SignTool integration; EV
  certificate recommended for SmartScreen reputation).
- Distribution: GitHub Releases only. MSI for enterprise GPO remains a
  possible post-v1 addition.

---

## 13. Milestones

- **M0 — plumbing (retire the biggest risk first).**
  TSF text service in pure Rust (COM via the `windows` crate)
  registers/activates on Win11; romaji→hiragana composes inline and
  commits on Enter; the alphanumeric width policy is enforced end-to-end
  (a trivial feature, but it proves the commit-path normalizer); engine
  process + hand-rolled IPC + pass-through fallback work; installer
  installs/uninstalls cleanly.
  *Exit: type ローマ字 into Notepad, Terminal, and Chrome and get
  hiragana — and `docker` never comes out full-width.*
- **M1 — conversion.** System dictionary compiled from Mozc data + IT
  overlay v0; lattice + Viterbi; English-surface candidates
  (どっかー→Docker); Space converts, candidate window with paging; commit
  works.
  *Exit: golden-corpus top-1 accuracy ≥ 80 % of Mozc's on the same
  corpus; IT-term slice ≥ 95 %.*
- **M2 — real IME.** Multi-segment editing (focus move, resize), F6–F10,
  identifier-case transforms (§5.6), N-best per segment, revert/commit
  semantics complete, learning v1 (homophone + recency), recent-context
  v1 (left-context carryover + commit cache, §5.8), user dictionary.
  *Exit: daily-drivable by the author.*
- **M3 — ATOK-ness.** Prediction/suggest, history prediction, domain
  coherence (§5.8), homophone annotations in the candidate UI,
  reconversion, settings UI, per-app profiles, import from MS-IME/ATOK
  user dictionaries.
  *Exit: technical-corpus accuracy ≥ Mozc baseline; author prefers it to
  Mozc for daily engineering work.*
- **M4 — hardening & release.** App-compat matrix green, fuzzing clean,
  signed installer, auto-update, docs. Optional flag: live conversion.

---

## 14. Risks

| Risk | Mitigation |
|------|------------|
| TSF app-compat black holes (Electron, games, UWP) | M0 targets the three worst hosts first; UI-less mode support from the start; pass-through fallback |
| Dictionary licensing | Only BSD/Apache sources (Mozc data, SudachiDict); build-time license gate |
| Conversion quality plateau below expectations | Costs/matrix inherited from Mozc data give a strong floor; learning layer is where we differentiate, and it's independent of base quality |
| Stale TSF registration breaks user's typing | Uninstall/rollback tested in CI VM snapshots; registration is idempotent and versioned |
| Scope creep toward cloud/NN features | Non-goals list; NN reranker explicitly deferred until after M4, and only as an offline-trained, on-device reranker of N-best |
| TSF-in-Rust has little public prior art (references are C++) | M0 exists solely to retire this; COM classes via the `windows` crate's `implement`; Mozc/SampleIME patterns ported by reading |
| Hand-rolling parsers/codecs/tries enlarges the bug surface | Every hand-rolled format gets round-trip property tests + a fuzzer (§11); formats are fixed-layout and boring by design |
| Trimmed lexicon degrades general Japanese | Accuracy corpus keeps a general slice with a hard floor (§11); learning + user dictionary recover the long tail per user |
| ARM64X hybrid DLL: cargo cannot produce it — needs a bespoke MSVC `link /MACHINE:ARM64X` merge of paired ARM64+ARM64EC objects, with ~zero Rust prior art | Explicit spike with a documented fallback (native-ARM64-only DLL; x64-emulated hosts on ARM64 unsupported until proven); never on a milestone's critical path |
| Windows feature updates silently wipe third-party TSF registrations | Logon-stub self-check re-registers on every logon (§12.1) — the risk is retired continuously, not once at release |
| Hard latency gates on shared CI runners are chronically flaky | Absolute budgets asserted on a dedicated reference runner; shared CI gates relative regressions only (§10) |

---

## 15. Reference material

- Mozc (Google 日本語入力 OSS): architecture, dictionary format ideas,
  data licensing — the primary prior art for the client/server split and
  lattice conversion. Reading source only; nothing is linked.
- MeCab / CRF cost model literature for the class-bigram design (design
  reference only — no MeCab code, binaries, or models are used).
- Microsoft TSF documentation + Windows classic samples (SampleIME).
- ATOK behavior (as a user-visible spec only — no code, no data):
  key bindings, homophone guidance UX, learning behavior.
