# Context Prediction — Phase 5A Public Source Pin

Status: source pin and local verifier only for Issue #34. No dump, extracted
article, generated dataset, checkpoint, or model artifact is committed or
distributed by this change.

## Pinned source

The offline data lane is pinned to the Wikimedia Japanese Wikipedia
`20260801` pages-articles multistream snapshot. The checked-in manifest records
the exact official URLs, byte lengths, and published SHA-1 values for the
article dump and its multistream index. It also pins the official SHA-1 manifest
itself by independently measured byte length and SHA-256.

| Role | Bytes | Digest |
|---|---:|---|
| Articles multistream | 4,827,732,824 | SHA-1 `6c917b51d6f6b53a34eaebcb2a675c0769054343` |
| Multistream index | 31,277,924 | SHA-1 `81443fd2f4e4c462464b965a4b0d2704c659cfc0` |
| Official `sha1sums.txt` | 24,743 | SHA-256 `ef06c6fd50e598f07cdaeaff8dd2f0e6fdee1e0b6f6d3a594793c3fc6810747a` |

The official directory and file headers were re-read on 2026-08-12. The source
is intentionally immutable by dated snapshot rather than a `latest` alias.

## Storage and verification boundary

`scripts/verify-context-prediction-source.ps1` validates the manifest schema,
exact role set, dated Wikimedia URL prefix, safe plain file names, byte lengths,
and the declared SHA-1/SHA-256 values. It never downloads data. The operator
must supply an external source directory containing all three files.

The checked-in manifest can be schema-checked without the 4.8 GB source using
`-ManifestOnly`; this does not claim that the dump files themselves were
downloaded or verified locally.

Example:

```powershell
rtk proxy powershell -NoProfile -File scripts/verify-context-prediction-source.ps1 `
  -SourceDirectory "$env:USERPROFILE\tmp\sakura-context-data\sources\jawiki-20260801"
```

The dump, index, checksum manifest, extracted records, train/tuning/held-out
files, audit samples, and checkpoints must remain outside Git. A verifier
self-test creates only bounded synthetic files beneath the OS temporary
directory, confirms a valid set, proves tampering is rejected, and deletes its
exact temporary directory.

## Licensing boundary

The manifest records Wikimedia's dump legal reference and marks license review
as required before distributing any dataset or derived model. This phase is an
offline research source pin, not a conclusion that a generated dataset or model
may be redistributed. Attribution, content-license, database-right, and derived
artifact obligations must be reviewed against the exact transformation and
distribution plan before any artifact leaves the private build environment.

## Remaining Phase 5 gates

- Generate negatives from actual Sakura candidate snapshots, not random words.
- Implement the actual offline Sakura replay adapter against the bounded schema.
- Run the stable-id, article-split, exact/near-deduplication, hash-bound manifest,
  and Tier A/B/C audit gate now defined by `context-dataset` on that replay.
- Meet the Issue #34 audit and label-precision gates before model claims.
