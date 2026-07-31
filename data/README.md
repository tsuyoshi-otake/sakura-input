# data/

Source data that ships with Sakura Input, in the human-editable form. The
compiled form is produced by `dictc` at build time and is never committed —
if a `.dic` file appears in a diff, something has gone wrong.

| File | Phase | Purpose |
|------|-------|---------|
| `romaji.toml` | 1 | Romaji→kana table driving the input FSM (DESIGN 5.1) |
| `keymap-ms-ime.toml` | 1 | Default key bindings, Microsoft IME conventions (DESIGN 2) |
| `keymap-atok.toml` | 1 | Alternative ATOK-style bindings |
| `system.dic.txt` | 2 | Base Japanese dictionary source |
| `it.dic.txt` | 2 | IT-domain terms — the reason this IME exists (DESIGN 6) |
| `connection.txt` | 2 | Frozen connection classes for the Viterbi cost model (DESIGN 5.2) |

Everything is parsed by hand-written code: the full-scratch rule (DESIGN 3.1)
covers data formats too, which is why the config files use a deliberately small
TOML subset rather than TOML proper.
