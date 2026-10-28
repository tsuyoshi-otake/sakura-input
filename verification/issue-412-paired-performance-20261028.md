# Issue #412: paired conversion measurement after batch 14

## Verification

- Verify: reconstruct both batch 14 images from the current input, omitting the last 39 curated rows only in a task-local copy for the baseline. Expect: exact SHA-256 values recorded in the batch 14 report. Result: baseline `B6232786A0B69763AED768E33904E1DAF8B5A15867937A361E64D9009F3788DD` and current `373317D41E0EE540EC8D4D07C7405BA91CB157DBB4351291797E3D2D37B46925` matched.
- Verify: open both images in one process, warm up 40 conversions, then measure 400 alternating conversions over four readings. Reverse the image order for each set of four readings, and assert that each paired result has the same candidate count and leader. Expect: quantify differences with less ordering bias and detect those result changes. Result: first run baseline/current p50 1.140/1.145 ms and p99 3.092/3.053 ms; repeat p50 1.077/1.082 ms and p99 1.971/2.293 ms. All paired count and leader checks passed. This comparison did not reproduce the roughly 28–40% median difference in separate batch 14 runs. Four readings and 400 samples do not establish a global performance bound or explain all tail differences.
- Verify: release benchmark and its clippy check; expect both pass and no test processes remain. Result: passed twice and process check clean. Clippy initially rejected a manual modulo expression; replacing it with `is_multiple_of` made it pass.

The conversion candidate construction path reads surface and annotation, while selected-candidate details are retrieved separately; dictionary loading validates detail tables. This motivates distinguishing conversion, load and detail-display measurements. The exact reason for the earlier separate-run difference remains unproven; the paired result limits the evidence for a broad median regression on these readings.

## Ownership and scope

The benchmark owns measurement ordering and its result checks, without changing product behavior or dictionary state. Investigation expanded into dictionary construction, conversion and detail-display boundaries because previous runs gave conflicting timing results. Future batches can run the paired benchmark directly but still need representative workload and tail-latency interpretation. Coverage remains 79,435 / 661,428 (12.01%), below the 30% milestone.
