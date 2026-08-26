# Sakura Input candidate-limit sweep

An offline, non-shipping evaluator for Issue #95. It answers one question:
what does raising `ConversionOptions::max_candidates` actually cost?

The limit is not a display cap. It is passed straight into `search_n_best` as
the n-best bound, so there is no larger candidate pool to page through — the
limit decides how much searching happens. This tool measures conversion
latency, lattice/state consumption, the search terminal, and the resulting
candidate count for every (reading, limit) pair, against a real dictionary
image.

The directory is a nested Cargo workspace with no registry dependencies, so
`Cargo.lock` holds only the two path packages and nothing enters the shipping
crates.

```powershell
cargo generate-lockfile --offline --manifest-path tools/candidate-sweep/Cargo.toml
cargo build --release --offline --features wide `
  --manifest-path tools/candidate-sweep/Cargo.toml
```

`--features wide` enables `sakura-core/research-wide-candidates`, which raises
`MAX_CONVERSION_CANDIDATES` to 512 for the sweep only. Without it the tool
refuses any limit above the shipping bound, because `ConversionOptions`
validation would reject it. Shipping targets never enable either feature.

```powershell
.\tools\candidate-sweep\target\x86_64-pc-windows-msvc\release\sakura-candidate-sweep.exe `
  --dictionary artifacts\release\system.dic `
  --readings eval\corpus\behavioral\candidate-limit-issue95\readings.txt `
  --limits 9,18,27,36,54,72,108,162,256,512 `
  --repeats 25 --warmups 5 --it-bias on `
  --output sweep.tsv
```

Every flag except `--repeats`, `--warmups` and `--output` is required;
`--it-bias on|off` is explicit so a capture never silently mixes the two
option identities. Input repair is disabled for every run: it is a separate
bounded pass with its own candidate budget and would blur what is measured.

The TSV carries one row per (reading, limit):

```text
reading  chars  limit  candidates  single_char  lattice_nodes  states_pushed
         terminal  min_us  median_us  p95_us  top1
```

`terminal` distinguishes a search that ran out of dictionary (`exhausted`)
from one the limit or a budget cut short — a reading whose row says
`exhausted` cannot produce more candidates at any limit, so widening the cap
will not help it. A per-limit roll-up goes to stderr.

The committed corpus at
`eval/corpus/behavioral/candidate-limit-issue95/readings.txt` groups readings
by length, because the measurement showed cost tracks reading length rather
than the limit: a one-mora reading stays flat at roughly 20 microseconds from
limit 9 through 512, while a sentence reading pays for every extra candidate.
