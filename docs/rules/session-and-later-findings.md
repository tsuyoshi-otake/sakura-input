## Session state that describes a composition

- **A flag that qualifies a composition must not be able to outlive one.**
  `Session::shifted_ascii` (the temporary English composition) was cleared
  only by `Session::reset` and by receiving a non-ASCII character. Erasing the
  composition with Backspace or forward Delete reached neither, and the
  resulting `Idle`-with-latch state was invisible, sticky (every romaji
  keystroke is ASCII), and unrecoverable by any key (#51). Restore such an
  invariant at one point per key in `apply_key`, before prediction and
  rendering — not by adding a clear to each erase path, which is the
  list-of-cases shape `Session::reset`'s doc comment already argues against.

- **`is_composing()` is not a safe proxy for "this flag is still meaningful".**
  It deliberately ignores `shifted_ascii` once `raw_input` is empty, and
  `commit_pending` returns early on `!is_composing()`. Any state that both
  gates on `is_composing()` and is only cleared inside it can strand itself.

- **Measure a "nothing works" report instead of reasoning about it.** Pressing
  each plausible recovery key from the stuck state and recording consumed /
  resulting preedit / flag turned an unfalsifiable severity claim into a table
  that named the two keys that *would* have recovered (Escape, Enter) and why
  neither could fire. That table is what proved the root cause was the whole
  cause and not one of several.

- **Key-map modifiers match exactly, so a held Shift turns Backspace into a
  different key.** `[composing] backspace = delete_back` does not consume
  Shift+Backspace. Holding Shift to type English therefore leaked Backspace
  (and Left/Right) to the host unless those chords were bound. Verified by
  `KeyMap::lookup(State::Composing, shift+backspace)` and the AIUEO repair
  tests in `shift_latin_order_tests`.

- **When visible English text is `raw_input`, the caret must be a raw-input
  index.** `render_preedit` used to pin the caret at `raw_input.len()` while
  Backspace popped the last raw byte and deleted `preedit[cursor-1]`. After
  Left, that pair produced AIUEO → AIUOE / AIUOEO. Verified by
  `production_left_then_backspace_deletes_the_character_before_the_caret`
  and by a mutant that restored end-append (`AIUOE`).

- **`resync_shifted_ascii_from_raw` is what makes later English conversion
  possible, even though the user sees `raw_input`.** `begin_conversion`
  beeps when `preedit` is empty. A no-op resync leaves `preedit` empty after
  Shift+Latin typing, so Space never reaches the IT-flag dictionary path.
  Visible-text tests cannot kill that mutant; a convert-after-CLAUDE test
  can. Verified by cargo-mutants 27.1.0
  (`replace resync_shifted_ascii_from_raw -> Ok(())` caught) and
  `resync_is_required_for_shifted_ascii_dictionary_conversion`.

- **`WriteCoordinator::attach` refuses a plan whose `before` is not the
  journal tail.** After engine plans commit `AIUEO`, a host-stolen
  `AIUE`→`AIUOEO` attach is `ProjectionMismatch`. This is the strongest
  COM-free stand-in for “Shift+Backspace must not be applied by the host
  while the engine still owns the key.” Verified by
  `shift_latin_backspace_retype_plans_commit_in_order_and_never_aiuoeo`.

- **Whole-function llvm-cov of `feed_character` / `apply_backspace` /
  `render_preedit` is not Shift-Latin coverage.** Each function returns
  early on `shifted_ascii`; the remaining regions are kana / pending-romaji
  / CJK normalize. Measure the early-return arm line ranges separately
  (2278–2326, 3502–3512, 4165–4193) and list the rest as out of scope.
  Verified 2026-08-15: `shift_latin` filter, 45 tests, arm 98.5 / 90.0 /
  75.5 while whole-function backspace stayed 25.0% (20/80). `mcdc_records`
  were 0.

- **Escape after Convert is not convert-cancel.** Production Escape clears
  the English buffer; converting Backspace cancels conversion without
  deleting a letter. A coverage PBT that maps `Convert` to Space after
  punctuation also diverges: `decide_shift_ascii_convert` inserts a literal
  U+0020. Verified by the first fail of
  `production_convert_cancel_then_home_backspace_keeps_press_order` (got
  `"X"` vs `"XAIUEO"`) and coverage-neighbor case 1 (`"AIUEO--IEUA "` vs
  `"AIUEO--IEUA"`).

- **`sakura_tsf_test_host` / `e2e-host` is the installed language profile,
  not a Sakura-only HWND.** The next automated layer that does not touch
  the installed IME is a process-local EDIT plus `checked_host_call` /
  `plan_from_visible`. A live `ITfContext` / `ITfRange` still cannot be
  constructed in this crate. Verified by
  `shift_latin_settext_payloads_reach_a_process_local_edit_hwnd_and_never_aiuoeo`
  and the recovery-test comment in `text_service.rs`.

- **Process-leak assertions must identify the owned artifact, not only the
  executable name.** The capture verifier initially counted the user's
  installed `sakura_engine.exe` as a leak; scoping by the repository debug
  path plus `--test-pipe` distinguished it from owned children. Verified by
  the missing-dictionary capture check: exit 2, no output, identical private
  process sets.

- **Serialize real engine child lifetimes within a Windows integration test
  binary when parallel startup is load-sensitive.** `space_key_dispatch_pipe`
  reproduced `STATUS_ACCESS_VIOLATION` under repeated `--test-threads=2` runs
  while five serial runs passed; a lifetime mutex in the shared harness was
  followed by ten successful parallel-thread repetitions and a green
  workspace run. The mutex owns no protected data, so recover its poison after
  a failed assertion; otherwise one failed child test suppresses the terminal
  results of every later integration test in the process.

- **Pass a fixed diagnostic payload as a record, not positional scalar
  arguments.** Converting `debug_trace` to `TraceEvent` preserved its
  content-free wire row and made the evaluation dependency graph pass strict
  `cargo clippy -- -D warnings` without an allow-list.

- **Every workflow sparse-checkout that runs the dictionary builder must include
  every pinned input it validates, not only the obvious dictionary directory.** `build-dictionary`
  consumes Mozc's `src/data/rules/segmenter.def` in addition to
  `src/data/dictionary_oss`; omitting it made both release and ordinary
  installer workflows fail before compilation. Verified by complete two-pass
  dictionary builds after adding that exact file to each workflow checkout.

- **Do not generate a numeric surface already supplied by an exact dictionary
  edge.** N-best deduplicates by rendered surface, so a cheaper generated `一日`
  can otherwise hide the lexical entry's cost, ordinal, and detail provenance.
  Skip only the identical generated surface, then rank the remaining numeric
  spellings behind the exact lexical form. Verified by the synthetic core test
  and all 19 shipped-dictionary ranking tests for 1.0.18.

- **Pin line endings for every text file whose raw SHA-256 is a release
  contract.** The reranker research manifest had the reviewed LF hash locally,
  but a Windows Actions checkout converted it to CRLF and the installer failed
  closed after all earlier gates passed. An exact `.gitattributes` `eol=lf`
  rule keeps the reviewed manifest bytes identical across checkouts.

- **Write those files as bytes, not as text.** The `.gitattributes` pin above
  keeps git from changing them; it cannot stop the tool that creates them.
  Verified 2026-09-09 (#148): bumping `data/update-signing/release-sequence.txt`
  with Python's `Path.write_text` produced `7\r\n` under Windows' default
  newline translation, and because the file is embedded with `include_bytes!`
  and compared to `format!("{floor}\n")`, fourteen `sakura-settings`
  `update_trust` and `updater` tests failed with "embedded release sequence
  contains CR or NUL". Write with explicit bytes (`printf '7\n' >`, or
  `newline=""`) and confirm with `xxd` before running anything else — the
  failure surfaces far from the write, in tests that never mention the file.

- **A voiced suffix needs an attested independent unvoiced base before it may
  be marked non-initial.** Same-surface suffix, prefix, or non-independent
  evidence is insufficient: Mozc assigns both `ばん` and `はん` → `版` its
  generic suffix class, while `び` → `日` was incorrectly hidden because
  `ひ` → `日` existed only as suffix and non-independent nouns. The fixed-source
  rule first reduced 532 to 500 marked entries by rejecting suffix-only evidence,
  then to 494 by rejecting prefix/non-independent evidence; the second step
  changed only six entries across five readings and made `日` top-ranked for
  `び` and `ぴ`, while all four `ずかい` → `使い` / `遣い` identities stayed
  non-initial. Do not substitute a word-cost dominance rule: the measured
  version restored 325 entries and changed 229 readings, including obvious
  lexical fragments.

- **Keep per-candidate provenance bit-packed inside the fixed conversion
  arena.** Four repair counters pushed the Windows test harness over its stack
  boundary at 32-way parallelism even though serial tests passed. A one-byte
  repair-kind mask preserved the required evidence and restored the 32-thread
  test plus the 128 KiB worker-stack checks.

- **Measure stack *usage*, never stack *headroom*, and read it out of the
  object file.** Three journal entries recorded the raw-repair overflow as
  "falsified by measurement" and one session withdrew the correct
  `#[inline(never)]` fix on the strength of a padding probe: `black_box([0u8;
  16384])` still passed, so the frame looked to have 16 KiB spare. A padding
  probe only reports that N more bytes fit wherever the compiler put them; it
  cannot say which frame is live at peak depth. Disassembly answered it in one
  pass. `ConversionCandidate` is 4,152 bytes, an unoptimised build gives every
  by-value move and temporary its own slot, and
  `convert_input_with_raw_repair_plans` carried ten of them in a 41,456-byte
  frame that stayed live across the corrected pass's whole conversion subtree
  -- 136 KiB required against 128 KiB reserved. Extracting the two
  candidate-moving blocks into `admit_repair_pass` and `merge_repair_scratch`
  moved those slots into siblings that are dead while anything deep runs,
  taking the orchestrating frame to 7,576 bytes and the requirement under
  68 KiB. Total bytes did not shrink; their lifetime did. Procedure:
  `cargo rustc -p <crate> --lib --profile dev -- --emit=obj=<path> -C
  codegen-units=1`, then `llvm-objdump -d -C` from the toolchain's
  `lib/rustlib/<target>/bin/`. The linked `.exe` has no COFF symbol table, so
  it must be the object file. Match **both** prologue forms: `subq $0xN, %rsp`
  and the MSVC big-frame `movl $0xN, %eax` ... `callq` ... `subq %rax, %rsp`.
  The second has no `__chkstk` string in the disassembly because the call goes
  through a relocation, and skipping it hides exactly the largest frames.

- **A sweep that passes in every condition is a claim about the harness before
  it is a claim about the code.** `cargo test ... -- --exact <filter>` with a
  filter that is not the full test path runs zero tests and exits 0. The
  earlier "48 of 48 stack/limit combinations passed, 256 candidates fit in
  64 KiB" has that shape, and the same false green appeared again today.
  Read the `running N tests` line, not the exit code. Near a resource
  boundary a single run is probabilistic -- this overflow reproduced about 30%
  of the time -- so run several trials per point (20 here) before calling a
  size passing or failing.

- **Raising a ceiling constant changes space as well as time, so audit inline
  array dimensions for it.** #94 and #95 tied
  `MAX_DICTIONARY_SURFACES_PER_READING` to `MAX_CONVERSION_CANDIDATES`, which
  read as a candidate-count change but grew `DictionaryEdgeBudget`'s inline
  `[u32; N]` from 64 to 1,040 bytes while it stayed `Copy` and while
  `build_lattice` constructed a fresh one per reading start. The conversion
  path has two invariants at once and satisfying either alone is wrong: a
  per-call `Vec::with_capacity` fits the stack guard but breaks
  `conversion_into_reused_candidate_buffers_allocates_nothing` and
  `cross_commit_bridge_conversion_allocates_nothing`. Scratch that must be
  both large and allocation-free belongs in the `Converter` arena, reset per
  use.

- **A branch with no upstream has no CI, and `ci.yml` is not all of CI.**
  `feat/95-single-kanji-and-candidate-cap` accumulated nine code commits that
  no workflow ever saw, and four gates were red. Three were in `ci.yml` and
  reproduce locally: `cargo test --workspace` (the raw-repair stack overflow),
  `cargo clippy --workspace --all-targets -- -D warnings`
  (`field_reassign_with_default` in two committed #99 tests), and
  `cargo fmt --all -- --check`. Running those three and declaring the push
  green still shipped a red `installer.yml`, because #95 added
  `src/data/single_kanji` to `build-dictionary.ps1`'s own `$SparsePaths` and
  its required-file checks but not to the `sparse-checkout` in `installer.yml`
  or `release.yml` — and `release.yml` runs only on a tag, so that half would
  have surfaced at release time. Before a push that lands accumulated work,
  enumerate every workflow the push triggers (`ls .github/workflows`) rather
  than the subset that is convenient to run locally, and treat a green local
  suite on a branch CI has never seen as unverified.

- **A path list duplicated between a script and a workflow will drift, so make
  the two comparable and say which is authoritative.** `Resolve-PinnedSource`
  re-asserts its sparse profile only for the clone it manages itself; when
  `-MozcSource` names a directory, as both workflows do, it verifies the
  revision and returns, so anything missing from the workflow's list reaches
  the build as a hard failure with no repair path. The lists are now literal
  matches — `src/data/rules` rather than `src/data/rules/segmenter.def`, which
  cone mode resolves identically because a listed file pulls in its ancestors'
  immediate entries — and both steps carry a comment naming the script as the
  source of truth.

- **A glossary supplies each term's meaning, not the reading a user types.**
  `data/it-terms.tsv` is generated from a term glossary, so `ACM` arrived with
  the reading `えーだぶりゅーえすさーてぃふぃけーとまねーじゃー` and nothing
  else, and 2,315 of 9,687 ASCII IT surfaces (23%) had no kana reading at all.
  Coverage counted from the source therefore overstates reachability: the row
  exists and the term is still untypeable. Audit a generated dictionary by the
  reading a user would actually press, not by the entry count. The same
  mismatch produces compound-only heads -- the glossary lists `RFC 7807` and
  `PR Review` but never `RFC` or `PR` -- so check every multi-token surface's
  head for a standalone entry.

- **An IT reading may take rank two, never rank one, from a word that already
  owned that reading.** Dump the pre-overlay dictionary and intersect it with
  the readings being added: of the overlay's 754 kana readings 32 landed on a
  reading that already had a dictionary entry, and eleven of those led the
  existing word until they were re-priced to yield (cost 9000, or 16000 where
  9000 was not enough -- `IA` and `ACID` both needed the higher price). The
  measurement is cheap and it is the only way to keep the project's "no
  general-Japanese regression for an IT gain" rule honest. Do not try to
  express this as a blanket assertion over candidate lists -- most "Japanese
  candidates" are the reading's own hiragana echo or lattice-assembled
  fragments, so an absolute guard fails on 500 harmless rows. Pin the measured
  collisions instead.

- **Comparing rank one is not enough: an exact entry prunes the whole fuzzy
  expansion beneath it.** Adding one row collapses the reading's candidate list
  -- `じーぴーゆー` went from 108 candidates to two -- because the engine stops
  expanding a reading fuzzily once it matches exactly. Words the fuzzy list used
  to carry disappear from ranks well below the leader, where a leader-only diff
  never looks. That is how `ぐろっく`/Grok silently removed `クロック` *and*
  `黒く`, and `ぴんぐ`/ping removed `ピンク` while the leader check stayed
  green. Diff the **whole** candidate list against a dictionary built from HEAD,
  and widen the probe window first: at 8 candidates the window truncated 718 of
  754 lists and the audit saw almost nothing. The invariant to hold is that
  every pruned word is still reachable from the reading that is actually its
  own (`炙ろう` from `あぶろう`, not from `あぶろ`); where it is not, drop the
  row rather than re-price it, and leave the term to its ASCII reading.

- **Give a kana reading prediction only when it cannot crowd a prefix.** A
  Shift+ASCII reading competes only with other Latin runs, so it stays
  predictive; a kana reading shares its prefixes with ordinary words, and 470
  acronyms reachable from `え` would trade a general-Japanese regression for an
  IT gain. Curated kana rows are conversion-only (`prediction_cost = -`).

- **A user-deletable record that is merely weaker than what the binary already
  enforces must never fail harder than its own absence.** (Bytes that are not a
  well-formed record are a different case and stay terminal.)
  Verified 2026-09-09 (#150): `%LOCALAPPDATA%\SakuraInput\update\trust-state.txt`
  is written only by updater-driven checks, so a machine that installs by hand
  keeps whatever sequence its last check saw (4 / 1.0.36 here) while every new
  build embeds a higher floor (7 in 1.0.39). `TrustState::parse` rejected a
  state below that floor as a terminal error, so the settings app answered every
  update check with "update trust state is below the embedded trust floor" and
  had no way back -- while an attacker who disliked the file could simply delete
  it and get the accepted no-state path. The bound that actually holds is the
  one embedded in the binary. A stored bound weaker than the embedded one is
  *no bound*, not corruption: fold it into the absent case and rewrite it from
  the signature-verified input. Keep genuinely malformed bytes terminal, and
  put the file's full path in that message so the user can recover.
