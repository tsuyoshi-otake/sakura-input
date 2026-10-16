# corpus/accuracy/external/

The local layer of the accuracy corpus. Nothing here except this file is
committed: the sentences carry licences that need per-row attribution or have no
evaluation-data notice in `THIRD_PARTY_LICENSES/`.

| Source | Content | Licence | Committed |
| --- | --- | --- | --- |
| Tatoeba | Japanese sentences, about 1,000 conversion rows and about 1,000 prediction words derived from them | CC BY 2.0 FR, attribution per sentence author | No |
| Mozc evaluation set | `src/data/dictionary_oss/evaluation.tsv` of the pinned Mozc revision (the revision in `data/SOURCES.lock`, `scripts/build-dictionary.ps1` and the workflow `ref`) | Mozc BSD; `THIRD_PARTY_LICENSES/mozc-dictionary.txt` covers the dictionary data, not evaluation data | No |

The `.gitignore` of this directory keeps fetched files out of a commit. If the
owner later decides to commit part of either set, the licence notice and the
attribution are added in that change, not here.

## Record what you fetched

Keep a local `SOURCES.local.txt` (ignored) next to the files, one line per file,
so two people can tell whether they measured the same data:

```text
<file name>  <source and exact version or revision>  <retrieval date YYYY-MM-DD>  <SHA-256>
```

- Source and version: for Tatoeba, the export name and its date; for Mozc, the
  full 40-character revision.
- SHA-256: lowercase hexadecimal of the file's bytes
  (`Get-FileHash -Algorithm SHA256 <file>`).

A measurement that uses this layer quotes those lines and the SHA-256 of
`conversion.tsv`/`prediction.tsv` printed in the `accuracy-gate` header.

## Not in this directory yet

The fetch script, the pinned versions and the local baselines are tracked as a
separate Issue (#346). Until it lands, this layer is manual and its results are
not part of `--check`.
