## Performance

- **Do not ship a performance claim a benchmark has not made.** The SIMD run
  scanner in `sakura-core::width` was written, reviewed and committed with a
  confident doc table before anything measured it. The benchmark, written
  afterwards, showed it made Japanese prose — the single most common input
  this IME will ever see — **34 % slower**. Write the benchmark first, or at
  minimum before the doc comment.

- **`&src[at..]` re-validates a UTF-8 character boundary on every index.** On
  text where most characters take the slow branch (i.e. all Japanese), that
  check costs more than a vectorized run scan saves. Walk a `Chars` iterator
  and take `chars.as_str()` for the remainder instead. This one change moved
  Japanese prose from 0.66× to 0.93× against the scalar loop.

- **Below one vector block there is nothing to amortize a scanner's setup
  over.** `normalize_into` short-circuits `src.len() < MIN_VECTOR_BYTES` to a
  plain per-character loop in a body small enough to inline, so a single
  keystroke — the overwhelmingly common call — never pays for a function call
  into the run scanner.

- **Microbenchmarks here report the best of N runs, never the mean.**
  Everything that makes a run slower (preemption, a frequency dip, a cache
  eviction) is additive noise on top of a fixed cost, so the minimum is the
  estimator that converges. Measured with the mean, one unchanged loop
  reported 53 ns and then 100 ns on consecutive runs; best-of-7 is stable to
  within a few percent. See `crates/sakura-core/tests/width_bench.rs`.

- **Compare grown data with the last released build, not only with the
  previous batch.** The Issue #412 descriptions grew from 31,301 to 269,339
  entries over dozens of batches, and every batch note compared only with its
  predecessor (open and validate p50 46.5 → 46.4 ms, 55.0 → 51.8 ms,
  71.4 → 69.3 ms: unchanged within noise). At release time the same bench on
  the final image against the 2.0.24 one showed 27-45 ms more to open and
  validate it, with the per-key conversion unchanged (1.82 and 1.81 ms). Bench
  the final data against the last released build before tagging, and state the
  difference in the release notes.

- **`dictionary_image_bench` depends on which image is measured first.** One
  pair gave 79.2 → 123.8 ms, a repeat 77.5 → 115.0 ms, and with the roles
  swapped 87.6 (new) against 60.6 ms (old). Quote the range over both orders,
  never one run. The machine state also moves absolute values by tens of
  milliseconds (a 38.8 MB image opened in 46 ms in the batch notes and in
  60-79 ms at release time), so only a difference measured in the same run
  counts.

- **Check a shared item once, not once per reference.** `validate_details`
  UTF-8-checked the description and display text of every detail record and
  every relation text. Records share texts (inflected forms reuse their
  lemma's description, and display equals description in all 269,339 records
  of the 2.0.25 image), so 600,878 references re-read 58.9 MB to cover 4.95 MB
  of distinct text (11.9×): 21.4 ms for that loop, against 1.4 ms for one pass
  over the table (the whole parse is now 29.0 ms). The table is now checked
  once (valid UTF-8, every text starting on a character boundary); a table
  failing either test takes the per-reference check, which decides as before,
  so a bad text is still rejected when the image is opened and one nothing
  refers to is still not examined. `dictionary_image_bench`, four interleaved
  rounds per image order: the 2.0.25 image's added open time went from +22.4
  and +21.7 ms to +3.1 and +3.1 ms (65.3 → 42.4 ms). The cost now follows
  records and distinct text bytes (about 1 ns per record and 0.29 ms per MB),
  not references. When open time grows with the data, count references against
  distinct items before blaming file size.
