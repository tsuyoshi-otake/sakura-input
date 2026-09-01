# data/

Source data and provenance records that ship with Sakura Input. The compiled
form is produced by `dictc` and is never committed—if a `.dic` file appears in
a diff, something has gone wrong.

| File | Phase | Purpose |
|------|-------|---------|
| `romaji.toml` | 1 | Romaji→kana table driving the input FSM (DESIGN 5.1) |
| `keymap-ms-ime.toml` | 1 | Default key bindings, Microsoft IME conventions (DESIGN 2) |
| `keymap-atok.toml` | 1 | Alternative ATOK-style bindings |
| `SOURCES.lock` | 2 | Pinned upstream revisions, paths, license boundaries, and trim policy |
| `mozc-trim.report.json` | 2 | Machine-readable result of the pinned Mozc trim |
| `non-initial-boundary-policy.tsv` | 2 | Exhaustive review of every reading/surface pair that the raw allomorph classifier would otherwise hide completely at an initial conversion boundary |
| `it-terms.tsv` | 2 | Generated MIT-licensed IT overlay from the pinned smile-chat glossary, including deterministic ASCII readings for Shift+English conversion |
| `it-terms.report.json` | 2 | Import counts, ASCII-only term counts, and the explicit missing-reading gap list |
| `curated-terms.tsv` | 2 | Project-authored MIT overlay for canonical casing and high-value terms missing from the generated glossary |
| `conversion-priorities.tsv` | 2 | Project-authored calibration overlay for context-free top-1 conversion; re-prices existing lattice edges and may add missing IT/business compounds such as Issue #62's 機能紹介 |

The Mozc trim admits a row on `max_word_cost` alone. `max_surfaces_per_reading`
is `none` by default, so a reading keeps every affordable homophone instead of
losing its more expensive ones to a positional cap; the former default of twelve
surfaces per reading dropped 10,781 surfaces across 963 readings, including
ordinary words such as `気管`, `旗艦`, `季刊`, `関`, `光`, and `行` (Issue #94).
Selected surfaces retain their exact rows so contextual connection costs remain
available. `legacy_row_evidence_cap` is a frozen provenance boundary and not an
admission rule: it records which rows the former row-based cap had already
shipped so allomorph classification cannot change retroactively as coverage
grows, which is why it has no command-line flag. The schema-3 trim report
records the surface cap (`null` when absent), the frozen evidence cap, and the
entries/surfaces rescued from the legacy row cap; this keeps general-Japanese
coverage auditable without hiding it behind a small curated overlay.

`non-initial-boundary-policy.tsv` is pinned to the same Mozc revision and must
exactly cover every `(reading, surface)` pair whose selected rows would all be
`non-initial`; missing and obsolete rows fail the build. The current review
uses exact BCCWJ short-unit evidence, or at least 1,000 conservative BCCWJ
long-unit endings for readings of two or more kana, plus the reported `ずみ →
済` form. One-kana long-unit suffix matches are not sufficient evidence. For
an approved pair, the compiler derives an initial-safe identity from the
corresponding unvoiced independent base when available and leaves the original
suffix/inflection identities protected; if the selected identity is already
initial-safe, it releases only that one row. Thus common forms such as
`済み`, `振り`, `会社`, `頃`, `分`, `作り`, and `不足` remain available after
a user-visible composition boundary, while `ずかい → 使い／遣い` remains
suppressed. The schema-4 trim report records the raw classifier totals, all 287
audited blocked pairs, the 60 approved pairs, the retained pairs, the released
and generated initial-entry counts, and the policy SHA-256.

The dictionary compiler also expands Mozc 基本形 verbs and i-adjectives into fused conjugations (`来て`, `書いて`, `行って`, `高くて`, …) at build time. Sakura stores static lattice edges and does not inflect at runtime, so the Mozc trim can keep `来る` while dropping `来て`. `inflection-expand` reads the trimmed system TSV plus pinned `id.def`, emits only missing `(reading, surface)` pairs, and keeps Mozc connection ids. The generated overlay is `LicenseRef-Mozc-Dictionary` and is not checked in; `scripts/build-dictionary.ps1` rebuilds it every pass.

The Sakura system dictionary is maintained as fourteen canonical category
dictionaries (grammar/function words, inflections, general vocabulary, fixed
expressions, numeric/time/unit terms, people, places, organizations/products,
loanwords, abbreviations/ASCII, IT/engineering, specialist domains,
symbols/emoji, and orthographic variants). They are the main system dictionary,
not a user dictionary or a separate candidate layer. `category-split`
accepts them with `--system-category`; `--supplement` is retained only as a
backward-compatible command-line alias. The PowerShell pipeline names the input
directory `-SystemCategoryDirectory` (the old `-SupplementLexiconDirectory`
name is accepted only for compatibility). Place-name import drops the address
layer: postal-code readings (`001`, `001-0000`), placeholders such as
`(そのた)`, and prefecture-qualified municipal surfaces (`北海道厚岸郡浜中町横浜`).
Short toponyms (`東京`, `渋谷`, `横浜`, `渋谷区`) remain. The source TSV is
left unchanged; the filter runs at compile time.

Run `scripts/build-dictionary.ps1` to fetch the pinned Mozc tree, read the
vendored smile-chat MIT glossary under `third_party/smile-chat-public`,
regenerate every intermediate, compile `system.dic`, repeat the build, and
compare SHA-256 digests. Provide the canonical fourteen-category source
directory with `-SystemCategoryDirectory` when generating the full Sakura
dictionary. Generated artifacts go under `~/tmp/` by default. The checked-in
overlay can only be replaced when the script is given `-UpdateCheckedInData`.
CI does not use a private-repository token for smile-chat.

For ASCII glossary surfaces, the importer adds lower-case readings that can be
typed as a continuous ASCII run begun with Shift. Multi-word surfaces also receive a separator-free
reading and a first-word reading, so `CLAUDE` can select both `Claude` and
`Claude Code`. Terms with no kana reading but a safe ASCII surface are tracked
as `ascii_only_terms` rather than silently dropped.

`curated-terms.tsv` is a small, reviewable complement to that generated data.
Its lower-case ASCII readings let one continuous ASCII run begun with Shift convert forms such
as `OPENAI`, `GITLAB`, `PYTORCH`, or `MICROSOFTTEAMS` to canonical surfaces.
`dictc` merges the generated layers and rejects duplicate edges; the deterministic
build report records the curated and conversion-priority source hashes.

`conversion-priorities.tsv` is intentionally separate from the curated casing
layer. Its rows are calibrated against the checked-in corpus probes and usually
reuse an existing `(reading, surface, left_id, right_id)` edge. Issue #62 may
introduce missing IT or business compounds that the generated glossary never
emitted; it still must not retune `昨日` itself. The annotation column is the
candidate note the user sees, so both this overlay and `curated-terms.tsv`
keep it empty; developer tags such as `[calibration]` stay in `#` comments.
User-specific preferences remain in the user dictionary and learning store.

The Mozc dictionary is a mixed-license work. Its Google-authored portions use
BSD-3-Clause, its IPAdic/ICOT portions retain their upstream conditions, and
the Okinawa dictionary portion is Public Domain. The exact pinned notice is
bundled as `THIRD_PARTY_LICENSES/mozc-dictionary.txt`. The smile-chat glossary
is governed by the nearest license boundary, `frontend/public/LICENSE`, which
is MIT; that notice is bundled separately.

All source formats are parsed by hand-written code: the full-scratch rule
(DESIGN 3.1) covers data formats too, which is why configuration uses a
deliberately small TOML subset rather than TOML proper.
