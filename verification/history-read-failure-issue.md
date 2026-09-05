# Preserve developer history when a complete frame cannot be decrypted or decoded

Baseline: f48d8f55fcab3f9abb210aa2a442cd8b217df1f1 (v1.0.35).
Tracking ID H5, priority P1, initial confidence CODE_CONFIRMED; developer-mode opt-in only.

## Contract and current evidence

`crates/sakura-engine/src/input_history.rs`: `scan_frames`, `repair_file`, `compact_file`, `InputHistoryService::open`.
With a complete, CRC-valid frame whose DPAPI payload cannot be decrypted or whose plaintext record cannot be decoded, scan_frames breaks and returns the previous offset as success. repair_file then truncates the original file there. A read/crypto/schema failure must instead return an error and preserve all original bytes. This is not evidence of a production DPAPI outage.

Minimal reproduction: synthetic valid record, CRC-framed invalid DPAPI blob or DPAPI-protected unsupported record, then another valid record; invoke repair/open and compare original bytes. Expected: error, byte-identical original. Baseline semantic regression tests will establish CONFIRMED status.

Impact: optional developer-history loss; normal input must continue through existing history-start failure handling. No evidence that normal learning data is affected.

## Scope and alternatives

Propagate decrypt/decode failures without converting them into a repair boundary. Preserve raw OS errors for crypto/I/O and InvalidData for unsupported decoded content. Structural incomplete/CRC-damaged tails retain their existing repair contract and receive a separate negative control. No new dependencies, format/wire changes, recovery generation, replacement primitive or timeout increase in this patch. H1–H4/H6 remain independent work; #105 tracks compaction in export control flow.

Alternative rejected: treating any unreadable ciphertext as torn tail, because checksum-valid opaque data does not establish recoverable corruption. Automatically emptying or skipping unknown records loses data and ID evidence.

Acceptance: old implementation fails regression assertions; fixed implementation errors without mutation for decrypt/decode/future-file-version failures; valid legacy/current records and actual torn-tail repair continue working. Test processes exit and temporary synthetic files are removed. Windows power-loss/real-TSF tests are outside this minimal read-failure patch, not PASS by implication.
