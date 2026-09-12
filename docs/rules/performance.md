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
