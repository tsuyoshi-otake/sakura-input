# Pre-extraction learning v3 fixture

`learning-v3-pre-extraction.hex` contains the exact binary bytes produced by the old engine writer, encoded as hexadecimal for review. Decoded SHA-256: `7da8cb554c501bffe551e1f94a7501f1bb94e57cae9664a09d81494d3a0bc249`.

The original local writer checkout was `bfa5eeb8a9a564f5d3401e7eec9321a2bca105d1`. Its learning source blob is `9abadc46f7885103c9a664811d2f2030dad8bef1`, also reachable at merged PR #190 revision `ca19035d4784f4ec2cd91421933acc8d8d4a4220:crates/sakura-engine/src/learning.rs`. The local checkout name is provenance, not a dependency on an unpublished commit.

The producer called `LearningService::open`, `learn("synthetic-reading", "Synthetic\tSurface", 3, 4)`, `suppress_repair_reading("synthetic-suppression")`, `learn("second-reading", "Second\nSurface", 5, 6)`, and `maintain`, then dropped the service. All contents are synthetic; day 20708 was supplied by the original writer's clock. The companion expected TSV is the old `LearningSnapshot::to_tsv` output (LF, UTF-8), SHA-256 `7fdd5700feebc5a08ff7ce0caa93ba065fcbe4e2225312fda01fce67956288b8`.

The store integration test checks every decoded field and exact decode/encode byte identity, including the intervening repair-suppression marker. Parent readback through the new engine and settings additionally checked sequence 1/3, exact TSV, suppression replay, no ignored tail, and unchanged original file hash. Do not regenerate expected bytes using the extracted writer.
