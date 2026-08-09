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
| `it-terms.tsv` | 2 | Generated MIT-licensed IT overlay from the pinned smile-chat glossary, including deterministic ASCII readings for Shift+English conversion |
| `it-terms.report.json` | 2 | Import counts, ASCII-only term counts, and the explicit missing-reading gap list |
| `curated-terms.tsv` | 2 | Project-authored MIT overlay for canonical casing and high-value terms missing from the generated glossary |
| `conversion-priorities.tsv` | 2 | Project-authored calibration overlay for context-free top-1 conversion; replaces existing lattice edges without increasing cardinality |

The Sakura system dictionary is maintained as fourteen canonical category
dictionaries (grammar/function words, inflections, general vocabulary, fixed
expressions, numeric/time/unit terms, people, places, organizations/products,
loanwords, abbreviations/ASCII, IT/engineering, specialist domains,
symbols/emoji, and orthographic variants). They are the main system dictionary,
not a user dictionary or a separate candidate layer. `category-split`
accepts them with `--system-category`; `--supplement` is retained only as a
backward-compatible command-line alias. The PowerShell pipeline names the input
directory `-SystemCategoryDirectory` (the old `-SupplementLexiconDirectory`
name is accepted only for compatibility).

Run `scripts/build-dictionary.ps1` to fetch the pinned trees, regenerate every
intermediate, compile `system.dic`, repeat the build, and compare SHA-256
digests. Provide the canonical fourteen-category source directory with
`-SystemCategoryDirectory` when generating the full Sakura dictionary. Generated
artifacts go under `~/tmp/` by default. The checked-in overlay can only be
replaced when the script is given `-UpdateCheckedInData`.

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
layer. Its rows are calibrated against the checked-in corpus probes and must
reuse an existing `(reading, surface, left_id, right_id)` edge. User-specific
preferences remain in the user dictionary and learning store.

The Mozc dictionary is a mixed-license work. Its Google-authored portions use
BSD-3-Clause, its IPAdic/ICOT portions retain their upstream conditions, and
the Okinawa dictionary portion is Public Domain. The exact pinned notice is
bundled as `THIRD_PARTY_LICENSES/mozc-dictionary.txt`. The smile-chat glossary
is governed by the nearest license boundary, `frontend/public/LICENSE`, which
is MIT; that notice is bundled separately.

All source formats are parsed by hand-written code: the full-scratch rule
(DESIGN 3.1) covers data formats too, which is why configuration uses a
deliberately small TOML subset rather than TOML proper.
