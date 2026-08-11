# Context Prediction — Phase 5C Streaming Article Extraction

Status: offline namespace-zero extraction contract and synthetic verification
for Issue #34. The pinned Wikipedia dump has not been downloaded or extracted.

## Boundary

`context-dataset extract` consumes a decompressed MediaWiki XML stream without
building a DOM. It accepts either an external `--xml` file or stdin, allowing a
separately controlled bzip2 decompressor to stream the pinned multistream dump
without materializing the much larger XML file.

This repository does not add a bzip2 crate or silently invoke an unpinned
external executable. The decompression operator remains responsible for
verifying the compressed source with
`verify-context-prediction-source.ps1`, recording the decompressor identity,
and passing only the corresponding XML stream. The extraction manifest binds
the exact decompressed input stream by SHA-256 in addition to the checked-in
source-manifest SHA-256; it does not by itself prove the transformation between
those two byte streams.

## Streaming rules

The extractor writes only namespace-zero, non-redirect pages with exactly one
complete current revision, nonzero article/revision ids, a title, and nonempty
text. XML entities and CDATA are decoded. Other namespaces, redirects,
oversized pages, and incomplete pages reach separate counted terminal states.

Memory is bounded to the XML reader buffer plus one page. Titles are limited to
4 KiB and article text to 8 MiB. An oversized page is discarded and counted;
it never creates a partial record. Output is LF-canonical UTF-8 JSONL with
source id, article id, revision id, title, and text.

The immutable external directory contains `articles.jsonl` and a
`manifest.json` commit marker. The manifest records the pinned source and
snapshot, compressed-source manifest hash, decompressed XML hash, extractor
hash, algorithm/limit versions, terminal-state counts, output size, record
count, and artifact SHA-256. Verification reparses all records, rejects
duplicate article ids, and proves both hash and terminal-state accounting.

## Invocation

With a separately decompressed external XML file:

```powershell
rtk cargo run -p dictc --bin context-dataset -- extract `
  --xml C:\context-data\sources\jawiki-20260801.xml `
  --source-manifest corpus\context-prediction\source-manifest.json `
  --output-dir C:\context-data\extracted\jawiki-20260801-run-001 `
  --extractor-sha256 <64-lowercase-hex>

rtk cargo run -p dictc --bin context-dataset -- verify-extraction `
  --extraction-dir C:\context-data\extracted\jawiki-20260801-run-001
```

Omit `--xml` to read decompressed XML from stdin. Raw XML and extracted JSONL
must remain outside Git. The CLI rejects external-input/output paths beneath
the repository and refuses to overwrite an existing output directory.

## Verified synthetic scope and next work

Bounded tests cover namespace filtering, redirects, incomplete and multiple
revision pages, XML entities, malformed/truncated XML, input/output hashing,
manifest verification, and artifact tampering. They do not establish real dump
counts, extraction throughput, replay coverage, label precision, or licensing
permission for derived distribution.

The next independent step is an offline adapter that turns these records into
actual Sakura prediction candidate snapshots and the Phase 5B replay schema.

