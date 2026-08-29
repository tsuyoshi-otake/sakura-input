# Sakura Input — Design Document

A Japanese Input Method Editor (IME) in the spirit of ATOK, **specialized
for IT engineers**: fast, accurate kana-kanji conversion with strong
personalization, built Windows-first on a portable core. The TSF DLL and
engine core are implemented in **pure Rust, from scratch**. The optional,
out-of-process long-conversion reranker is also a Rust binary, while its
separately isolated ONNX Runtime DLL and model are external runtime artifacts.

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
- **Rust Sakura code, isolated optional runtime.** TSF COM glue, tries, Viterbi,
  engine IPC codec, config parser, and `sakura_neural_worker.exe` are
  hand-written Rust. The core has no MeCab or runtime NLP dependency beyond
  `windows`/`windows-sys` FFI bindings. The optional worker (§5.2.1) is outside
  the TSF DLL and engine process and dynamically loads an installer-provided
  ONNX Runtime DLL plus a separately attributed model; the product therefore
  does not claim to have no native third-party runtime dependencies.
- **Fastest and lightest.** Every engine key path must feel instantaneous:
  ≤ 5 ms for kana composition, ≤ 20 ms for full conversion, ≤ 10 ms for
  prediction updates — with the smallest achievable footprint: engine
  private working set ≤ 15 MB (the dictionary is a file-backed mmap that
  stays out of private memory), zero heap allocation per keystroke at
  steady state. This is an engine-process budget; it deliberately excludes the
  optional neural worker's model/runtime memory. Budgets are enforced by tests
  (§10).
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
- **Punctuation style.** Two independent choices, one per role:
  `comma = touten (、) | full_comma (，) | half_comma (,)` and
  `period = kuten (。) | full_period (．) | half_period (.)`. All nine
  combinations are reachable because real conventions mix them — `、。`
  for ordinary prose, `，．` for a JIS-style paper, `，。` for 公用文,
  and `,.` for a manuscript that will be typeset from plain text
  (LaTeX, Markdown), where a full-width `．` is the wrong character. A
  first-class, frequently-toggled setting for engineers writing mixed
  EN/JP docs; enforced at the same §5.6 choke point as width. The choke
  point owns exactly four code points — `、` `，` `。` `．` — and that
  does not change when a half-width mark is selected: it *emits* ASCII
  `,`/`.` without *claiming* them, so an ASCII comma arriving as input
  stays an ordinary symbol governed by `symbol_width` and the `,` in
  `foo(a, b)` is never reinterpreted as a 読点. The setting decides which
  mark is offered **first**, not which marks exist: converting a lone
  punctuation mark lists the whole family of four, configured glyph at
  the top (§8.4).
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
1. **Bitness/arch.** The DLL loads into every process, so its
   architecture is dictated by the host's, not by ours. The supported
   configuration is **Windows 11 on x86-64 with AVX + SSSE3** and nothing else
   (§3.2): 64-bit hosts get the text service, 32-bit hosts fall back to
   MS-IME, and ARM64 is out of scope. Keeping to one architecture is
    what makes the engine free to assume AVX + SSSE3 everywhere and to dispatch
    to AVX2 at run time. AVX-512BW+VL is an additional bench-only candidate;
    shipping remains on AVX2 until direct measurements are backed by stable
    end-to-end and cross-host evidence.
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
sandboxed/fullscreen apps (drawn as a top-level Win32 popup positioned via
the text service's `ITfContextView` rects).

### Component inventory

| Component            | Language        | Runs in            | Responsibility                          |
|----------------------|-----------------|--------------------|-----------------------------------------|
| `sakura_tsf.dll`     | Rust (COM)      | every host app     | TSF glue, IPC client, zero logic         |
| `sakura_engine.exe`  | Rust            | per user session   | all conversion, dictionaries, learning   |
| `sakura_neural_worker.exe` | Rust + dynamically loaded ONNX Runtime | on demand, local child process | optional long-conversion N-best reranking only |
| `sakura_renderer.exe`| Rust (Win32)    | per user session   | candidate window, mode indicator, engine watchdog |
| `sakura_settings.exe`| Rust (Win32)    | on demand          | settings, user-dictionary editor         |
| `sakura_setup.exe`   | Inno Setup      | install/update     | installer (declarative script, §12)      |
| `sakura_regtool.exe` | Rust (Win32)    | install/update     | TSF/COM (de)registration helper (§12)    |
| `dictc`              | Rust            | build time         | dictionary compiler (TSV → binary image) |
| `ime-eval`           | Rust            | eval time          | quality measurement: oracles, blind Judge, gates |

### 3.1 Dependency policy (the full-scratch rule)

Every Sakura-authored shipping binary is Rust, including the isolated neural
worker. Core shipping binaries allow only `windows`/`windows-sys` — auto-generated Windows
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

Because it is a *linked-code* rule, procedural-macro crates are exempt too.
`#[implement]` and `#[interface]` — the machinery that makes TSF COM classes
possible in Rust at all — arrive as `windows-implement` and
`windows-interface`, which pull in `proc-macro2`, `quote`, `syn` and
`unicode-ident`. Those four compile for the *host*, run once at build time to
emit source text, and contribute no bytes to any shipped artifact. They
  therefore appear in `Cargo.lock` and never in a binary.  The offline `dictc`
  compiler also uses closed `serde`/`serde_json`, SHA-256, and Unicode-NFC
  dependency closures solely to reject malformed, duplicate-key, stale, or
  schema-unknown LLM-detail JSONL release inputs and to make exact target
  identities reproducible; none are linked by a shipping runtime crate.
  The offline `tools/ime-eval` quality-measurement runner uses the same closed
  `serde`/`serde_json` and SHA-256 closure to load evaluation cases, constrain
  Judge JSON, and hash Judge/corpus identity; it is not a shipping runtime
  crate. `ci/dep-policy.ps1` encodes both exceptions and verifies the resolved graph
  of every runtime crate does not contain the offline detail-parser closure.
The list is closed, not a category — a new name gets added only with a written
reason, so "it's just a build dependency" cannot become a loophole.

There is one separately reviewed exception for the optional
`crates/sakura-neural-worker` workspace member. Its closed
`$IsolatedWorkerRuntime` allowlist in `ci/dep-policy.ps1` admits the
`ort` binding, strict JSON/SHA manifest validation, Unicode tokenizer support,
and their closed dependency closure solely for that Rust worker; the binding is
configured for `load-dynamic`, rather than for a bundled ONNX Runtime. The
intended release payload keeps `onnxruntime.dll` beside
`sakura_neural_worker.exe` and keeps the model as a separate artifact. Neither
the TSF DLL nor `sakura_engine.exe` links this runtime. This is isolation, not
a claim that the complete installed product has no native third-party runtime
dependencies.

### 3.2 Target platform and instruction set

**Windows 11 (build 22000 or later), x86-64, AVX + SSSE3 required.** One
architecture, one OS floor. Everything else — x86 hosts, ARM64 machines,
Windows 10 — is an unsupported configuration rather than an untested one,
and the difference matters: an unsupported configuration is one we
deliberately decline to ship into, so it gets a clear refusal at install
time instead of a subtly broken IME.

What the narrowing buys, in order of how much it is worth:

- **The ARM64X problem disappears.** A hybrid ARM64X DLL is a link-time
  merge of paired ARM64 and ARM64EC objects that cargo cannot produce, with
  essentially no Rust prior art. It was the largest unpriced risk in the
  plan and it is now simply out of scope.
- **AVX + SSSE3 is a floor, not a branch.** The whole workspace is built
  with `-C target-feature=+avx,+ssse3` (`.cargo/config.toml`). The narrow
  ASCII pass-through scanner uses SSSE3 `pshufb`, so checking only AVX would
  be an invalid safety contract. Both features predate the Windows 11 x64
  hardware baseline, so the floor costs no real users.
- **Above the floor, concrete kernels are resolved once.** Startup reads a
  `CpuFeatures` set and resolves a `KernelSet`, whose width-normalization
  member is one `WidthScanStrategy`. AVX2 is the standard 256-bit fast path;
  AVX+SSSE3 remains the 128-bit compatibility path. AVX-512 is never selected
  merely because a CPU advertises it: the benchmark candidate requires the
  complete `AVX-512F + AVX-512BW + AVX-512VL` set and evaluates the exact ZMM
  ownership boundaries `64, 65, 95, 96, 127, 128, 129, 255, 256, 257, 512`
  against AVX2. A future production admission requires every owned length to
  show a 5% median direct-kernel win with no slow-tail loss, plus stable
  end-to-end and cross-host evidence. Until then, the candidate is bench-only
  and the shipping resolver keeps AVX2. Inputs below any tested threshold
  delegate to the exact AVX2 scanner; in particular, a 16--63 byte run does
  not execute any AVX-512 instruction merely because the candidate can use ZMM
  for a measured longer range. The normalizer performs one selected-kernel call
  for inputs of 16 bytes or more, never a per-call CPUID, feature match, or
  lazy-initialization check. Every vector kernel has a
  scalar reference implementation that is the definition of correct, and a
  test that asserts each kernel available on the machine running the tests
  agrees with it byte for byte. A disagreement is a text-corruption bug, so
  "the fast path and the slow path must be observably identical" is a hard
  rule.

**When the detection happens: once, at startup, before any work.** The
engine resolves CPU features into one `KernelSet` in the first few statements of `main` —
before the pipe is created, before the dictionary is mapped — and stores
the answer in a startup-only `OnceLock`; the normalizer instead reads the
already-published `WidthScanStrategy` pointer. Three reasons it is startup
and not first use:

1. **A per-call `is_x86_feature_detected!` is a branch on the hot path.**
   The macro caches its answer, but the cached load and test still sit
   inside the width normalizer, which runs on every string the engine
   emits. Resolving a function pointer once moves that cost to a place
   where nothing is waiting on it.
2. **The startup log gets to name the strategy.** "avx-ssse3-128",
   "avx2-hybrid", or a calibrated "avx512bw-vl-from-64" (and its 128/256
   variants) in the first line of the log turns "why is it slower on my
   machine?" into a question with an answer, and makes a benchmark number
   reproducible without asking the reporter to identify their CPU.
3. **A missing baseline should fail loudly and immediately**, not on
   whichever keystroke first reaches vector code. If either AVX or SSSE3 is
   absent the engine refuses to start with a message naming the requirement
   rather than dying of `SIGILL` somewhere unattributable.

That last check is a backstop and is honestly labelled as one: because the
whole binary is compiled with `+avx,+ssse3`, a machine missing either feature may fault
before `main` ever runs. Setup's install-time gate (§12.2) is the check
that actually protects users; the startup check catches the remaining case
of files copied onto a machine that never ran the installer.

Where SIMD is *not* used is worth stating too, because the temptation is to
vectorize what is measurable rather than what is slow. A keystroke carries
one to three bytes; there is nothing there to vectorize, and the per-key
budget in §10 is dominated by IPC and TSF, not by arithmetic. The kernels
live where the byte counts are actually large: the width choke point
(§5.6), which every string leaving the engine passes through, and — from
M1 — dictionary search over the LOUDS trie and the connection matrix.

The width scanner only skips runs whose ASCII bytes can remain unchanged. It
does not vectorize kana-to-kanji conversion or full-width rewriting, so a
Japanese or full-width-heavy corpus is a regression guard rather than an
AVX-512 speed claim. The direct benchmark therefore labels its actual ZMM,
YMM/VL, and XMM path counts alongside the measured percentiles.

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
- `ITfLangBarItem` / `ITfLangBarItemButton` / `ITfSource` — the Windows 11
  `GUID_LBI_INPUTMODE` item. It is hidden until this TIP owns a focused
  editable context, then exposes the current あ/ア/ｱ/Ａ/A mode and a narrowly scoped
  menu; focus loss and deactivation hide/remove it immediately rather than
  leaving a permanent notification-area icon.
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

#### 4.1.1 Input-mode indicator assets

The input-mode button uses original, pre-rasterized 32-bit premultiplied BGRA
assets instead of rendering a host-dependent font inside `GetIcon`. Each mode
has independently authored 16 px and 32 px forms. The 16 px form is used below
24 logical pixels and the 32 px form at or above that boundary, allowing the
language bar to perform only a small final scale for intermediate DPI values.
Both sizes are generated from Yu Gothic UI Semibold with mode-specific optical
bounds; they are not runtime font fallbacks.

Dark-taskbar and light-taskbar variants share the exact alpha mask. Their
foreground pixels are respectively white and near-black, with every BGRA color
channel premultiplied by alpha before embedding. All four corners stay fully
transparent. Direct input has a dedicated slashed `A` asset and must never map
to the plain half-width-alphanumeric `A` asset.

`GetIcon` creates a top-down 32-bit DIB, copies one immutable embedded asset,
and passes it with a monochrome mask to `CreateIconIndirect`. The temporary
bitmaps are deleted immediately; the returned HICON is caller-owned according
to the TSF contract. Failure to select or construct an asset is a local COM
failure and must not cache a partial GDI object in the host process. The source
assets are reproducibly generated by `scripts/generate-mode-indicator-assets.ps1`.

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
   memory-mapped. The pinned Mozc taxonomy is **frozen at exactly 2,672
   classes before any `dictc` work starts**. A flat `u16[classes²]` would
   consume about 13.6 MiB, so `dictc` stores each row's exact modal cost
   plus sorted exceptions. The reader reconstructs every cell without an
   approximation and the encoded matrix remains a named ≤ 4 MiB line item
   in the §10 budget. Class growth costs quadratically, so any expansion
   requires a new measured compression design, never budget creep.
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

### 5.2.1 Optional local long-conversion reranker

**Implementation status:** the former DeBERTa Tiny runtime and installer path
have been removed. The Rust worker now admits only the content-addressed
`Sakura-Rerank-Tiny-v1` contract. The self-authored model is distributed under
MIT in the normal installer and defaults to long-conversion scope. Gate A is
still not accepted and the final holdout remains unused; release inclusion is
an owner product decision, not a claim that those quality gates passed.

The normal converter remains the lattice/Viterbi N-best generator. The optional
reranker never generates a candidate and is not a per-keystroke prediction
service: it may score only the first six candidates of an existing normal
`Space` conversion. The engine keeps a one-slot latest-wins mailbox and one
long-conversion thread. That thread starts the Rust `sakura_neural_worker.exe`
lazily as a local child process and communicates through versioned, bounded
binary frames on standard input/output. The worker dynamically resolves the
installer-provided ONNX Runtime DLL; the TSF DLL and engine neither load that
DLL nor the model, and the synchronous conversion path never waits for the
worker.

The worker is discovered beside `sakura_engine.exe`. Its research model
directory is `neural/sakura-rerank-tiny-v1/`, containing `model.onnx` and
`manifest.json`. A missing worker or model, invalid frame,
process crash, start failure, response timeout, or unavailable exact result is a
local-fallback outcome; the existing dictionary ranking remains final.

A request is eligible only for a classified `Normal` input scope, at a complete
non-direct preedit, and only if the reading/candidate snapshot satisfies the
long-conversion threshold (at least ten Unicode scalar values or a first path of
at least three segments) and has at least two candidates. Password, URL, Email,
Digits, unknown/unclassified scopes, and `test_only` input are excluded before
the worker boundary. The engine uses the reading locally to build that immutable
snapshot; the worker request contains candidate surfaces, costs, and
fingerprints needed to score it. The worker has no access to the document,
composition, user dictionary, or learning store.

The engine consumes a score only when owner, session, composition generation,
reading, and candidate-set fingerprint match exactly. Backspace, Escape, commit,
focus/deactivation, or later input makes an older result unusable. Once the
conversion candidate UI is shown, its ordering is frozen: no asynchronous result
may reorder it. Explicit user learning, exact cache, and user-dictionary
precedence are applied outside this worker and remain authoritative. Worker
restarts are bounded with exponential backoff and deterministic jitter.

The admitted research artifact is `Sakura-Rerank-Tiny-v1-research-prototype`,
contract version 1, FP32 SHA-256
`b3fe1e0aa7229edfd0760162d648f10328b0d75224a9cd49f2ba986b7db2ccbd`.
The runtime manifest also binds the reviewed research manifest, Gate A failure,
final-holdout non-use, MIT licensing, and explicit distribution authorization.
Protocol v1 supplies only
the existing candidate surfaces, local costs, and fingerprints. Context and
reading tensors are zeroed; available features are normalized local cost,
candidate order, and surface length. The listwise model score is the complete
selection signal, not a residual penalty added to local cost. The installed IME
does not import Python, PyTorch, or training dependencies.

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
1. Exact `(context, reading) → surface` match: a **strength-gated** default
   candidate. One recent confirmation may affect only an already-near base
   candidate; two confirmations may affect the upper candidate group; three or
   more recent confirmations establish a strong exact-context preference. This
   preserves immediate learning without letting one anomalous selection
   unconditionally replace the converter's grammatical/contextual result.
2. `(reading) → surface` frequency/recency score: a more conservative
   strength-gated fallback. It is limited to the upper base candidates even
   when frequently chosen, because it has no current grammatical context.
   Both layers use exponential decay with a half-life of about 30 days.
3. Learned segmentation constraints: soft boundary bonuses.

Caps and hygiene: bounded store — LRU beyond ~100 k entries at a packed
index budget of ≤ 64 B/entry, i.e. ≤ ~8 MB of the 15 MB private-WS
budget, reconciled by arithmetic in §10's table — never learn in
password/private scopes, one-click "clear learning data", exportable
(export/clear are shipped, tested features, not aspirations — §11).

### 5.4.1 Explicit developer input history

Developer input history is a separate, opt-in store for local IME development.
It is disabled by default and can only be enabled explicitly with
`config set developer-mode on`. It is never enabled by test flags or inferred
from the build type. The store lives under
`%LOCALAPPDATA%\\SakuraInput\\history\\input.bin`, separate from learning and
diagnostic logs.

While enabled, each real key event records the key code, character when
available, modifiers, repeat flag, scope classification, action, state and
mode transition, rendered preedit before and after the action, commit/delete
output, beep result, and the sequence/session identifiers. Conversion commits
also record the reading, chosen surface, and neighboring context. The history
service also appends an engine-identity marker when it starts, carrying the
package version and — for installed builds — the `versions/<version>-<build-id>`
release label so `history show` / `history export` can attribute the log to a
build. Synthetic test input is excluded. An unclassified scope is never
recorded; only a positively classified normal scope can create
developer-history records. Password, URL, email, and digit scopes are always
excluded.

Records are DPAPI-protected for the current Windows user, length- and
checksum-framed, written through a bounded non-blocking queue, and capped at
30 days and 64 MiB. Queue drops and write failures are counted. The running
engine exposes flush, clear, and live statistics operations, and the settings
CLI provides `history show`, `history export`, `history clear`, and `history
stats` so development data can be inspected, diagnosed, or removed without
touching learning data. The TSF bridge publishes the focused range's
`ITfInputScope` before admitting each key; a missing or unknown classification
is fail-closed.

The saved preference and the running service state are reported separately:
developer-mode changes take effect at the next engine start, and the CLI never
claims that a saved value has already changed a live process. Live statistics
include `active`, queue drops, persistence failures, and aggregate exclusion
counts for unclassified, sensitive, and test-only admission. These counters
contain no input content and reset with the engine process.

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
SetInputScope(session_id, scope)
ClearInputHistory / FlushInputHistory / InputHistoryStats
DeleteSession
```

`InputHistoryStats` carries the live `active` bit plus dropped, failed, and
privacy-exclusion counters; protocol versioning rejects older layouts rather
than interpreting a short payload as a valid zero-valued response.

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
by the last few committed inputs. Four bounded mechanisms cooperate:

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
- **Cross-commit lexical bridge.** Carrying only the final right ID cannot
  recover a dictionary path whose useful analysis begins before an explicit
  commit. For one complete system-dictionary commit, the engine therefore
  retains the exact final raw edge captured before bunsetsu display fusion:
  its bounded reading/surface tail, the typed right context before that edge,
  and the selected edge's connection-plus-word cost. The next classified
  `Normal` conversion first produces the ordinary current-only list, then may
  replay `tail + current` system-dictionary-only in the same converter slot.
  A replay can only lower an already-reachable direct system candidate with
  the same current surface and terminal right ID; it never manufactures a
  candidate, changes segmentation/provenance, or feeds another bridge. The
  combined surface must contain the exact committed surface prefix and its raw
  path must either span the commit boundary or expose a typed frontier there.
  Its score is normalized by subtracting the selected tail cost; negative or
  overflowing deltas, incomplete search, mismatched evidence, and all budget
  failures retain the complete current-only ordering.

  The retained tail is at least two characters and at most 48 UTF-8 bytes
  (surface 96 bytes); the current reading is at most 96 bytes. Replay is capped
  at 4,096 lattice nodes and 8,192 search states, and candidate projection is
  `O(K²)` for the already bounded candidate count. Scratch is converter-owned,
  so the warmed hot path allocates nothing and never acquires a second worker
  slot. A 2026-08-22 release-build run on an AMD Ryzen 7 9700X (500 warmups,
  5,000 samples) measured target full-conversion p99 at 0.238 ms, paired bridge
  increment p99 at 0.176 ms, and the maximum 48-byte-tail + 96-byte-current
  case at 1.053 ms p99, all within the 20 ms conversion budget. A conservative
  orthographic transfer may copy a measured contextual
  gain from the exact kana reading to another existing system candidate only
  when both have exact full-context paths, the same terminal class, and a
  shared majority suffix of at least two hiragana characters. This rule is
  lexical-string agnostic: `検討 + しますか` exercises the same path as
  `考慮漏れ + ないか`; unrelated `内科`/`内か` forms do not inherit it.

  The bridge is one-session, memory-only state. Exact user-dictionary entries
  bypass replay. Learned and cached selections remain authoritative when replay
  yields no improved direct candidate; once replay does yield such evidence,
  those preferences may select only another bridge-supported direct candidate
  and cannot transplant an unrelated homophone across the proven boundary.
  This authority rule depends only on candidate provenance, not on particular
  readings or surfaces. Repaired, generated, fallback, user, exact-synthetic,
  or already bridge-rescored paths cannot seed a later bridge. The retained
  tail is never sent to the neural/long-text worker, history, learning, logs,
  or disk. TSF captures the exact committed `ITfRange` after
  the applied write and, before the next real key, synchronously re-proves that
  the range text is unchanged and its end equals the collapsed caret. Focus or
  context replacement, selection/caret movement, host edits, undo,
  reconversion, mode changes, punctuation/whitespace boundaries, sensitive or
  unclassified scopes, and an unavailable read fail closed. The protocol-v19
  `ResetDocumentContext` request then clears carry, bridge, commit recency,
  exact commit-undo, raw provenance, and prediction cache while preserving the
  user's explicit input mode; a refused or uncertain reset retires the link.
  The bridge is evidence, not a fixed bonus: for example, an atomic
  `記載漏れ` analysis improves the grammatical candidates but can still leave
  `内科` first when its retained reanalysis cost does not justify promotion.
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
  we compile the pinned Mozc OSS dictionary data with our own `dictc`.
  Mozc is a mixed-license data set: Google-authored portions are
  BSD-3-Clause, IPAdic/ICOT portions retain their upstream conditions, and
  the Okinawa dictionary portion is Public Domain. `data/SOURCES.lock`
  pins the revision and paths, and `THIRD_PARTY_LICENSES` bundles the exact
  notices. No MeCab binaries or external dictionary formats exist at
  runtime; the license gate rejects any unlisted source marker.
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
  - readings: LOUDS trie keyed by kana. Format v2 stores each incoming Unicode
    scalar in the existing 16-byte `NODE` record and removes the separate
    `LABL` table,
  - values: 16-byte `ENTR` records containing `surface_ref`, connection ids,
    losslessly range-checked word/prediction costs, and flags. Sparse candidate
    annotations live in an entry-ordinal-keyed `AIDX` side table rather than
    reserving an annotation id in every entry,
  - surfaces: front-coded string pool with one `SOFF` restart offset per 16
    surfaces; lookup scans at most one bounded restart block,
  - connection matrix: exact row-mode-plus-sorted-exceptions encoding for
    2,672 frozen classes (§5.2),
  - annotations: side tables keyed by exact final ENTR ordinal.
- The release gate remains ≤ 128 MiB and cold-start to first conversion remains
  ≤ 150 ms. Issue #109's same-source measurement reduced the default image from
  47,561,532 to 37,381,940 bytes (21.403%) without changing entry semantics.
- Update mechanism (deliberately boring): dictionary updates ship
  with releases. The installer runs `regtool --stop` (the engine exits,
  releasing its mmap), replaces `system.dic` atomically, and the
  watchdog restarts the engine — no reboot needed for the dictionary, no
  host-app involvement, and no live hot-swap machinery to build. Only
  the in-use TSF DLL itself needs the reboot path (§12.3).

### 6.2 IT-term overlay dictionary (the specialization asset)

The IT vocabulary is split into a generated in-repo TSV
(`data/it-terms.tsv`) and a small project-authored complement
(`data/curated-terms.tsv`). The generated layer follows the pinned glossary;
the curated layer owns canonical casing and important Shift+ASCII gaps. This is the
product's moat and is expected to grow forever.

**Primary seed: the smile-chat glossary.** The pinned glossary under
`frontend/public/glossaries` contains 9,653 Japanese terms and is governed
by its nearest license boundary, `frontend/public/LICENSE` (MIT). The
one-shot importer currently emits 14,627 deduplicated term/alias surfaces,
matches 1,977 of them to Mozc ids/costs, applies explicit shape defaults to
12,650, and records all 1,554 missing-reading gaps. `term` + `reading` →
dictionary entries,
`normalizedTerms` English aliases → English-surface candidates
(くらうど→cloud), `domain` → the `IT` tag plus finer facets, and
definition texts → candidate annotations (§8). The pinned SHA, license
path, counts, and gap list are retained in machine-readable reports.

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

- **Candidate window** (`sakura_renderer.exe`): borderless Win32 popup window,
  GDI text (`CreateFontW` / `DrawTextW`), positioned from the composition's screen rect
  (received via the DLL → engine → renderer), never steals focus,
  per-monitor DPI aware (including DPI changes mid-composition, §4.2).
  Shows: shortcut digits, primary candidate surfaces, an annotation column,
  a quiet kind/page footer, and a passive page-position rail. The window is
  exposed to UI Automation (screen readers announce candidates), alongside
  the `ITfUIElement` data path for UI-less hosts.
- **Mode indicator**: small floating "あ/A" near the caret on mode change,
  plus the Windows 11 focused `GUID_LBI_INPUTMODE` taskbar item. The taskbar
  item exists only while an editable caret is focused; its menu offers the
  safe input-mode choices, one-shot restore, and Japanese-input on/off.
- **Settings**: key binding editor (presets: `ms-ime` default / `atok`,
  per-key overrides), romaji table editor, dictionary
  manager (user dict CRUD, import/export ATOK/Mozc/MS-IME formats),
  learning data viewer/reset, diagnostics (IPC timeouts, engine version).
- All UI text localized ja/en.

### 8.1 Candidate popup presentation (Issue #27 — automated and normal-light real-screen verification complete)

The renderer owns the top-level Win32 candidate popup. It remains non-activating,
caret-following, and per-monitor DPI-aware. Deliberate left clicks on visible
candidate rows are revision-stamped and queued for the candidate-owning TSF
session; every other point remains click-through. The renderer never edits the
document or changes candidate state itself. TSF revalidates focus, context,
ordinary-text scope, and its write journal before an engine commit, then applies
the resulting output through the normal edit-session path. Compact prediction
and expanded conversion presentations otherwise retain their engine semantics
and keyboard commands; the visual layer does not invent hierarchy or definitions.

The Sakura presentation uses low-contrast warm-neutral light and dark palettes,
Yu Gothic UI, and 28 logical-pixel rows. Candidate number, surface, and
annotation form stable columns; candidate text is the primary hierarchy, while
annotations, a quiet kind/page footer, and a passive page-position rail are secondary.
A muted sakura 2 logical-pixel selection rail identifies the selected row.
Content determines a 260–480 logical-pixel width, rather than a fixed wide
panel. This keeps short prediction lists quiet while allowing an expanded table
to expose useful annotations without obscuring the editor.

High-contrast mode substitutes the relevant Windows system colors and preserves
legible selection state. The same candidates remain exposed through UI
Automation for screen readers and through `ITfUIElement` data for UI-less hosts;
those paths must not depend on the popup being visible. The popup uses Win32 GDI
(`CreateFontW`, `DrawTextW`, and native brushes) rather than a layered-window or
DirectWrite rendering path. Its implementation neither requires regenerated
raster assets nor consumes the separately managed mode-indicator assets.

Automated unit and real-process integration coverage now verifies compact and
expanded candidate semantics, bounded content-aware layout, DPI changes,
non-activation, caret following, paging, digit selection, and UI Automation
exposure. A screenshot from the latest reinstalled build verifies the normal-light
popup's primary candidate text, right-hand annotation column, subtle selected-row
surface and sakura rail, prediction footer, and `1–9/9` page display. Real-screen
inspection of the dark and Windows high-contrast palettes remains outstanding.
The Issue #27 verification record confirms that the popup change did not
regenerate or alter mode-indicator assets.

### 8.2 Selected-candidate dictionary detail (Issue #28)

The renderer may show a non-interactive dictionary-detail panel only for the
currently selected candidate, within the same Sakura-owned HWND as the candidate
popup. It keeps the popup non-activating; the detail area remains click-through and uses the same
low-contrast palette and GDI boundary, and must not alter candidate selection,
ordering, paging, or TSF semantics. Candidate width and annotation-column
geometry are derived from the complete current page, even in compact presentation,
so selection changes cannot move the list. Long dictionary prose is absent from
the inline annotation column; short state labels such as history remain there.
The fixed-width detail panel wraps every available definition line and ellipsizes
only when the monitor work area is physically too short. Placement attempts the
candidate popup's right, then left, then below; when no placement fits the monitor
work area, the detail panel is absent.

The compiled image keeps optional detail records keyed by the exact final ENTR
ordinal, not by surface text. A detail is therefore omitted (fail closed) for a
compound candidate, an unresolved candidate, a stale/mismatched ordinal, malformed
detail data, or an old image without the optional tables. The source definition in
the dictionary is not subject to a display-length limit. At the engine/protocol
boundary, the producer derives an explicit UTF-8-safe bounded preview and carries
a `definition_truncated` flag; neither encoding nor decoding silently shortens
source text. UI Automation announces the selected detail and explicitly retains
that truncation state when only a preview is available.

Aliases, related words, synonyms, and antonyms are direct, manifest-pinned links
from a source-backed import or an explicitly reviewed Sakura release. The compiler
and renderer must not infer any of them from spelling,
embedding similarity, category membership, or transitive graph traversal. Each
group is optional and shows at most three direct terms. Acceptance must use
fixed-seed generated tests for malformed detail tables, Unicode preview boundaries,
duplicate/self/cyclic relations, exact-ordinal collisions, compound-candidate
omission, wire-frame limits, DPI/work-area placement, and UI Automation exposure.
The reproducible release build combines pinned smile-chat definitions, aliases,
and resolved related terms with pinned Japanese WordNet 1.1 definitions and
same-synset similar terms. That full-source merge contains 36,606 exact-entry
details. A separate offline release gate may add Sakura-authored definitions and
typed relations only
from a manifest-pinned target batch and a matching reviewed release directory;
drafts, stale dictionary identities, malformed provenance, and duplicate
normalized `(surface, reading)` pairs fail closed. Release 000010 was generated
against only the reproducible default dictionary identities: 242 targets were
reviewed, 236 terms were approved into 246 exact-entry details, and six were held.
Every approved record has a related term; the release additionally contains 43
similar terms and 16 antonyms. One earlier candidate, `終わり`, was held before
target creation because the default dictionary does not provide a unique safe
identity. The semantic review used a separate prompt pass with the same model, not
an independent model, because the user requested no delegated agent. Entries
without an unambiguous meaning remain detail-free; these counts are not a claim
that every dictionary entry has a definition. The measured default build contains
29,229 details in 472,825 entries; it is distinct from the full-source build above.

### 8.3 Notation-style presets (Issue #97)

The width and punctuation settings are individually correct and collectively
hard to aim. A reader who has been told "half-width comma and period, half
space at the Japanese/Latin boundary" has to translate that house rule into
seven controls spread over two settings pages, and has no way to check the
result afterwards. The preset is that translation, written down once.

- A `NotationStyle` names a whole combination: the three width channels, the
  two punctuation roles, the bracket style, and the space width. Four ship —
  標準（日本語）, 日本語技術論文（半角句読点）, 学術（全角コンマ・ピリオド）,
  公用文 — and they are pairwise distinct on those seven values.
- It is a **shortcut, not a setting**. There is no config key for it and
  nothing in `Preferences` records which style was picked: the seven leaf
  values remain the single source of truth, and the config file of someone
  who has never opened the control is byte-identical to before. Choosing a
  style writes the seven controls; editing any of the seven re-derives the
  preset, falling back to `カスタム` for a combination no style produces.
  Round-tripping through `apply_to`/`of` is what makes those two directions
  agree.
- Adding a style is adding a row to `NotationStyle::ALL`. It cannot change an
  existing style's meaning, because each style writes its seven values out in
  full rather than inheriting them.
- One of the seven, space width, lives on a different settings page than the
  preset. Applying a style says so in the status line rather than changing a
  page the reader is not looking at without a word.
- An `AppProfile` stores a `Normalizer` but no space width, so the per-app
  form of the control pins five of the seven values. The four normalizers are
  distinct on their own, which is what lets a profile read its style back.

### 8.4 The punctuation family in the candidate list (Issue #99)

A punctuation setting that also removed the other marks from the
candidate list made one quoted sentence a trip to the settings window.
ATOK is the reference here: the setting picks the default, the list
stays exhaustive.

- Converting a reading that is **itself a single punctuation mark**
  offers all four members of that role's family — `、` `､` `，` `,` for
  the comma role, `。` `｡` `．` `.` for the period role — ordered with
  the configured glyph first and the rest in a fixed table order. Any
  longer reading, and any reading that merely contains a mark, is
  untouched.
- The four rows differ only in a character the §5.6 choke point claims,
  so a correct list still renders as one glyph four times unless the
  rows bypass normalization. Each carries `synthetic_exact`, which the
  display path and both commit-only surface paths already honour ahead
  of `normalize_into`. This is a candidate-level fact, not a normalizer
  change: Rule 4 still owns exactly four code points, ASCII `,`/`.` are
  still emitted without being claimed, and the two half-width kana marks
  are offerable without being claimed either.
- `synthetic_exact` also suppresses learning and the exact cache for
  these rows, which is what the feature wants: reaching for `、` inside
  one quotation must not train the ranker to override the configured
  mark on every later comma. The setting stays the only durable
  preference.
- The configured glyph is also **selected**, not merely listed first.
  Two ranking rules would otherwise move off it, and both bite hardest
  under the shipped `、`/`。` style, where the configured mark *is* the
  reading: `preferred_candidate_index` refuses any row whose text equals
  the reading, and a surface learned while a different mark was
  configured outranks the top row outright. A reader whose settings
  window said `、` got `､` on a clean profile and `，` on one with
  history. The dispatcher therefore pins the initial selection the way
  an exact literal is pinned, which also keeps the optional reranker off
  a four-row list it has no context for. `PunctuationStyle::family_reading`
  is the single admission test shared by the appender and this pin, so
  the two cannot drift apart.
- Replacing TOP-1 is safe precisely because the reading is a single
  mark: the top row already *rendered* as the configured glyph, so the
  substitution is byte-identical on screen. Every other post-conversion
  appender still sits above the ranked ceiling.
- The style is read off the session, not the dispatcher, so a per-app
  notation profile (§8.3) reaches the converter the same way its width
  policy already reaches the choke point.

---

## 9. Security and privacy

- IME sees everything the user types — treat the whole codebase as
  security-sensitive. No network capability in DLL/engine/renderer
  (enforced: no networking crates linked; CI check).
- Sensitive fields (`IS_PASSWORD`, URL, email, and digit input scopes): direct
  input mode forced, no prediction, no learning, no developer input history,
  and the recent-context buffer (§5.8) is neither read nor written.
- The recent-context buffer is memory-only and dies with the session; it
  is never written to disk.
- Ordinary logs contain events and timings, never text. The separately named
  developer input-history store is an explicit local-development exception;
  it is DPAPI-protected, bounded, exportable, and never records sensitive
  scopes.
- The optional neural reranker is local-only. Its child-process IPC carries an
  eligible candidate snapshot solely for scoring; it has no network transport,
  and the worker's standard error is not retained by the engine. Input text is
  not added to ordinary diagnostics. Sensitive, unknown, unclassified, and
  `test_only` inputs do not cross this boundary.
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
| Optional neural worker private working set / model and runtime size | Working set not yet measured and excluded from the engine budget; 2026-08-10 x64 artifacts: worker 0.39 MiB, ORT DLL 15.08 MiB, model 40.37 MiB |
| Renderer private working set                  | ≤ 10 MB |
| Heap allocations per keystroke (steady state, kana + conversion + prediction hand-off) | 0 |
| Dictionary image on disk                      | ≤ 35 MB |
| — of which exact compressed connection matrix (2,672 classes, §5.2) | ≤ 4 MB |
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
6. **Quality measurement system** (`eval/`, runner `tools/ime-eval`):
   mechanical contracts (state, TSF, timeout, literal-token preservation,
   artifact identity) are deterministic oracles. Japanese meaning quality is
   a blind A/B Judge (`gpt-5.6-luna`, reasoning `max`) that never sees
   expected surfaces, issue numbers, or baseline/candidate labels. Luna Max
   itself is calibrated by a held-out human set and is never evidence for
   TSF/UI correctness. See `eval/README.md`.

---

## 12. Packaging, registration, updates

### 12.1 Installer: Inno Setup + a tiny Rust registration helper

The full-scratch rule (§3.1) covers runtime components, not packaging
tooling. The installer is a standard Inno Setup script
(`installer/setup.iss`) producing `sakura_setup.exe`. Inno provides the
commodity machinery — file copy with rollback, upgrade detection
(AppId), ARP entry, versioned file copy, silent
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

Install (`[Files]` + `[Run]`, in order): check the CPU (below) → copy the
x64 payload into `versions/<version>-<build-id>` → register the explicit
`versions/<version>-<build-id>\sakura_tsf.dll` with
`regtool --register --dll ... --no-wow64` → `regtool --enable-profile`.
The final command preserves or creates the stable logon task, enables the
signed-in user's profile, and waits for `sakura_logon.exe` to bootstrap the
newly active engine and renderer in the current session. This is required on an
upgrade because `PrepareToInstall` stopped the old engine and a logon trigger
will not fire again until the next sign-in.
The root `sakura_regtool.exe`, `sakura_logon.exe`, and `sakura_settings.exe`
files are stable bootstraps; the latter dispatches to the versioned settings
payload. The active COM registration is the single version-selection pointer.

The CPU check comes first because it is the one precondition that cannot
be repaired after the fact. Setup calls
`IsProcessorFeaturePresent` for both `PF_AVX_INSTRUCTIONS_AVAILABLE` and
`PF_SSSE3_INSTRUCTIONS_AVAILABLE`, and refuses to install on a machine
without the AVX + SSSE3 baseline (§3.2), naming the reason. The
alternative — installing and letting the DLL fault on its first
instruction inside every process that loads it — would present as the
user's applications crashing, with nothing pointing at the IME.
`MinVersion=10.0.22000` in `[Setup]` enforces the Windows 11 floor the
same way, and there is no x86 or ARM64 payload to install at all.

Uninstall ordering is safety-critical — a stale TSF registration
pointing at a deleted DLL bricks text input. The latest uninstall script runs
its critical boundary directly from `CurUninstallStepChanged(usUninstall)`,
where Inno documents that `Abort` stops Uninstall before file removal:

1. Remove the SYSTEM payload-cleanup task so it cannot race file deletion.
2. `regtool --unregister` (TSF profile + categories first). The
   uninstaller **halts on a nonzero exit code** instead of continuing to
   file removal; it first restores the cleanup task as compensation so a
   failed attempt leaves the installed system maintainable.
3. `regtool --stop`.
4. Inno removes files. A loaded versioned DLL is left in place when Windows
   refuses deletion; it is already unreachable because registration was
   removed, and no delete-on-reboot request is created.
5. User data under `%LOCALAPPDATA%` is kept unless the user opts into
   purge (uninstall page checkbox / `/PURGE=1`).

CI exercises this path on VM snapshots every release: install → type →
uninstall → verify typing still works.

### 12.3 Upgrade & auto-update

- Side-by-side upgrade: same AppId; `PrepareToInstall` runs
  `regtool --stop`, then the installer copies every runtime payload into a
  new `versions/<version>-<build-id>` directory. The active COM registration
  is switched only after the copy succeeds, using an explicit DLL path. A host
  process that has the old TSF DLL loaded keeps using that old image safely;
  no mapped file is overwritten and **a normal update exits successfully
  without a Windows reboot**. New host processes load the newly registered
  version, while `sakura_logon.exe` resolves engine/renderer from the active
  registration rather than from a mutable root path.
- The per-user logon task and the SYSTEM cleanup task both target stable root
  bootstraps and remain registered throughout an upgrade. Existing logon tasks
  are treated as already configured rather than rewritten. Therefore an update
  canceled before activation does not remove either the next-login startup path
  or the cleanup retry path.
- The root tools are stable across updates. `sakura_settings.exe` is a
  bootstrap that launches the versioned settings payload, so the settings UI
  can update independently of the image currently executing it.
- After activation, Setup removes obsolete version directories when their
  files are free. A still-mapped old TSF DLL keeps its directory temporarily;
  it is already unregistered, no reboot rename is queued. Setup also installs
  a separate hidden `Sakura Input Maintenance\Payload Cleanup` task as
  `SYSTEM`; it retries the cleanup at every interactive logon without
  elevating the normal-integrity engine task or prompting the user with UAC.
  Locked generations remain until a later logon can remove them.
- Updates require elevation (machine-wide install; §1 non-goals): the
  auto-updater launches Setup normally and lets Inno perform the UAC transition,
  retaining the pre-UAC token needed by `runasoriginaluser`; it triggers one
  UAC prompt. Non-admin users get a "new
  version available" notice to hand to their admin instead of a
  silently failing update.
- Auto-update (M4): unless explicitly disabled, the settings app checks GitHub
  Releases over WinHTTP at startup. If a newer release is available, it asks
  for confirmation before downloading, verifying the Authenticode/application
  trust policy + hash, and running the installer silently. Authenticode-signed
  releases require WinVerifyTrust; an explicitly `unsigned` release requires
  the canonical update-signing v2 manifest and detached Sakura public-key
  signature, and a valid Authenticode result is rejected for that policy. The
  exact-file guard remains held through application-signature verification,
  WinVerifyTrust, and ShellExecuteExW. The v1.0.33 bridge is manual for older
  schema-1 updaters. The user can opt out, and network code exists *only* in
  the settings/updater component, so the §9 no-network rule for DLL, engine,
  and renderer is unaffected. The frozen fields, keyring, sequence, rotation,
  and recovery rules live in `verification/update-signing-v2.md`.

### 12.4 Silent operation & distribution

- Standard Inno flags: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART
  /RESTARTEXITCODE=3010 /LOG=<path>`, plus `/PURGE=1` on uninstall. A normal
  side-by-side update returns `0`; `3010` remains recognized only for a
  legacy installer or an unrelated cleanup that genuinely needs a reboot.
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
| Dictionary licensing | Pin every source and nearest license boundary; gate source markers; bundle Mozc's mixed BSD/IPAdic-ICOT/Public-Domain notice and smile-chat's MIT notice |
| Conversion quality plateau below expectations | Costs/matrix inherited from Mozc data give a strong floor; learning layer is where we differentiate, and it's independent of base quality |
| Stale TSF registration breaks user's typing | Uninstall/rollback tested in CI VM snapshots; registration is idempotent and versioned |
| Scope creep toward cloud/NN features | Non-goals list; NN reranker explicitly deferred until after M4, and only as an offline-trained, on-device reranker of N-best |
| TSF-in-Rust has little public prior art (references are C++) | M0 exists solely to retire this; COM classes via the `windows` crate's `implement`; Mozc/SampleIME patterns ported by reading |
| Hand-rolling parsers/codecs/tries enlarges the bug surface | Every hand-rolled format gets round-trip property tests + a fuzzer (§11); formats are fixed-layout and boring by design |
| Trimmed lexicon degrades general Japanese | Accuracy corpus keeps a general slice with a hard floor (§11); learning + user dictionary recover the long tail per user |
| An unsupported CPU turns the compile-time AVX + SSSE3 baseline into an illegal-instruction fault inside the user's applications | Setup refuses to install without both requirements (§12.2) and the engine re-checks once at startup (§3.2); the fault can only be reached by copying files past both gates |
| A vector kernel disagreeing with the scalar one silently corrupts the user's text | Every kernel is differential-tested against the scalar reference over exhaustive ASCII and a fuzz corpus, on whichever selected strategy the test machine can run; CI runs at least AVX2 (§3.2) |
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
