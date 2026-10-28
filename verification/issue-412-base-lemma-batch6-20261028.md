# Issue #412: base lemma detail batch 6

## Scope and acceptance

Add reliable definitions for previously undescribed base-form entries, inherit them to eligible inflections, preserve existing descriptions and conversion data, and compare ranking and performance against the immediately preceding dictionary. Candidate draft definitions were individually selected; ambiguous readings, incorrect meanings and circular descriptions were excluded. The curated file adds 171 base-form rows; one row maps to more than one existing entry.

## Coverage and integrity

- Total entries: 661,428, unchanged.
- Detail records: 66,413 → 68,904 (+2,491), or 10.04% → 10.42%.
- Increase: 172 curated entry details, 1,836 generated inflection details, and 483 native inflection details.
- Remaining to the 30% intermediate milestone: 129,525 records; the overall objective remains 100%.
- All 14 category TSVs were byte-identical. Only detail sections DIDX, DREC, DREL, DTOF, and DTXT differed in the dictionary image. Every previous detail, including its relations, remained identical at its entry ordinal.
- Native inflection example: 植え acquired `「植える」の活用形。基本形の意味：植物を土に植え付ける。` The image test verifies that this entry had no detail in the baseline.
- Final dictionary was built twice with identical output. SHA-256: baseline `8825F3E9923B1EF9B0DE809EB4553228707989DA4FF41432EAB5DEA873E4A996`; final `6BDF66C4A20383E62388B59EA8CE7C125128E1A16C72F53ADA1BB656332E0A78`.

## Verification

- `cargo test --locked -p dictc-core -p dictc`, formatting check, inflection report validation, and clippy for the affected packages passed.
- Release ranking tests for issues 94, 348, 390, and 392 passed on both images. This exercises selected scenarios; identical non-detail sections provide the broader structural evidence for preserving candidate identity, costs, and ordering inputs.
- The release full-conversion benchmark ran 5,000 samples per case. Initial baseline current-only p50/p99: 0.985/2.162 ms; final: 0.838/1.441 ms. A repeat baseline after the final measurement was 0.829/1.447 ms. Target-bridge p50/p99: initial baseline 1.641/3.142 ms; final 1.383/2.283 ms; repeat baseline 1.374/2.305 ms. The initial run was slower for both cases; final and repeat baseline are close, so these samples do not establish a causal speed improvement or regression.
- The separate image benchmark (31 samples) measured candidate search/presentation p50/p99 at 1.139/2.007 ms baseline and 1.125/2.146 ms final. Open-and-validate p50/p99 was 60.241/67.828 ms baseline and 58.948/63.497 ms final. These small samples are sensitive to system noise.
- Repository-scoped test processes were checked and none remained.

## Ownership and residual risk

Definitions are owned by the curated base-lemma data; the existing dictionary compiler owns inheritance and preserves explicit details. No conversion ranking or runtime state contract was changed. Full-image comparison was needed because detail IDs can be rebuilt even when old meanings remain intact. This report lets the next batch reuse the measurement and integrity criteria; it must still review new definitions and compare its own baseline. Automatic semantic correctness cannot be inferred from structural checks, and coverage remains far below the overall goal.
