# Dictionary format v2 verification (Issue #109)

Date: 2026-08-29

## Acceptance result

The format v2 reader and writer are accepted for the default shipped dictionary.
The reader remains compatible with format v1 and fails closed on malformed or
inconsistent v2 tables. The same pinned source inputs produce a dictionary that
is 10,179,592 bytes smaller (21.403%), while exhaustive v1/v2 comparison finds
no semantic difference across 624,205 entries and 822,995 trie nodes.

## Fixed implementation contract

- `NODE` retains its 16-byte width and stores the incoming Unicode scalar in the
  previously reserved final `u32`; v2 has no `LABL` payload.
- `ENTR` is 16 bytes in v2. Word and prediction costs are checked `u16`
  conversions; compilation fails instead of truncating an out-of-range value.
- Sparse annotations use `AIDX` records keyed by final entry ordinal. Detail
  tables keep the same exact final-ordinal identity.
- `SOFF` has one absolute restart offset per 16 surfaces. A lookup decodes only
  the selected restart block and remains bounded to 16 records.
- Required table sets are version-specific. Malformed sizes, order, ordinals,
  offsets, UTF-8, scalar values, and duplicate or missing required tables are
  rejected before lookup.

## Reproducible image evidence

Both images were built twice from the repository's pinned default inputs. Each
format was byte-for-byte deterministic across its two builds.

| Measurement | Format v1 | Format v2 | Delta |
| --- | ---: | ---: | ---: |
| Image bytes | 47,561,532 | 37,381,940 | -10,179,592 (-21.403%) |
| SHA-256 | `85d94aecd966a10f43aeb87b5109c3d0b92c6eade798cf7b553d4d5cb476d1eb` | `b07cb62d9b8820c3dfbff1fc77e92ecfe485ea823e20399b4e992fac34589014` | n/a |
| Entries | 624,205 | 624,205 | 0 |
| Details | 31,288 | 31,288 | 0 |
| Connection classes | 2,672 | 2,672 | 0 |

The entire size reduction is accounted for by the intended representation
changes:

| Table | Format v1 | Format v2 | Delta |
| --- | ---: | ---: | ---: |
| `ENTR` | 14,980,920 | 9,987,280 | -4,993,640 |
| `LABL` | 3,291,980 | 0 | -3,291,980 |
| `SOFF` | 2,020,240 | 126,268 | -1,893,972 |

The default source currently has no candidate annotations, so its `AIDX` is
empty. Synthetic writer/reader tests cover sparse annotation ordinals,
homographs, and absence of annotations.

## Exhaustive semantic comparison

A separate comparator checked every node's topology and incoming label, every
entry ordinal's surface, connection ids, word cost, prediction cost, flags, and
annotation, plus exact byte equality of all unchanged optional table payloads.
It compared 624,205 entries, 822,995 nodes, and 505,060 surfaces with zero
differences. The canonical semantic digest was:

`a3adf4c05cb4577dc45bc0eb4a0f18399a36a29cc0377799dc8941fa672a3015`

Reading equality follows from exact equality of trie topology and every node
label; entry meaning then follows from the ordinal-by-ordinal comparison.

## Runtime measurements

An external benchmark opened each image through the production Windows
read-only mmap path in a fresh process. It then performed 2,000 warm-up and
5,000 measured conversions over 20 fixed general-Japanese and IT readings.
Five alternating v1/v2 process pairs were summarized by medians.

| Metric | Format v1 | Format v2 | Delta |
| --- | ---: | ---: | ---: |
| First-process open | 25,416.2 us | 45,017.1 us | +77.12% |
| Open page faults | 10,518 | 9,207 | -12.464% |
| Open working set | 42,962,944 | 37,609,472 | -12.461% |
| Warm conversion p50 | 906.9 us | 911.7 us | +0.529% |
| Warm conversion p95 | 1,895.5 us | 1,899.4 us | +0.206% |
| Warm conversion p99 | 2,529.8 us | 2,643.3 us | +4.487% |

The warm p50/p95/p99 regression gate of less than 5% passes. First-process open
became slower because the strict v2 parser validates the front-coded `SURF`
stream up front, but the measured 45.0 ms remains below the 150 ms product
target. These are fresh-process first-touch measurements; the OS file cache was
not forcibly flushed, so they are not claimed as physical cold-disk results.

## Commands and results

- `cargo fmt --all -- --check`: passed.
- `cargo test --workspace --no-fail-fast`: passed, exit code 0.
- `cargo test -p dictc --test conversion real_dictionary_balances_non_initial_fragments_and_productive_bound_forms -- --ignored --exact --nocapture`: passed against v2.
- `cargo test -p sakura-engine --test shipped_dictionary_ranking -- --ignored --skip issue_83_cross_commit_bridge_release_percentiles --nocapture`: 34 passed, 1 failed.
- `cargo test -p sakura-engine --test shipped_dictionary_ranking issue_83_shipped_path_uses_a_costed_typed_frontier -- --ignored --exact --nocapture`: the same assertion fails against both the v1 and v2 images (`left: 6`, `right: 0`). It is therefore retained as an unrelated existing ranking-test risk, not hidden as a v2 pass.
- `git diff --check`: passed.

## Orchestration and residual risks

- Research used the requested Luna role.
- No Max role was available, so Sol at maximum reasoning was used for design and
  decomposition. This increased cost but did not narrow the requested artifact.
- No Priorit role was available, so Terra at high reasoning was used for
  falsification. Its conclusions were treated as hypotheses until the parent
  reproduced them with executable checks.
- Reader and writer changes were implemented in two isolated Codex worktrees by
  Sol High, with non-overlapping file ownership. The parent reviewed and
  integrated them serially.
- The existing ignored Issue #83 ranking assertion remains unresolved and should
  be investigated separately because it reproduces identically with v1.
- Physical cold-disk startup was not measured. The accepted claim is reduced
  mapped footprint and first-touch page faults with warm conversion latency
  within the fixed gate.
