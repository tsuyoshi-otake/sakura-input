# Update-signing v2 fixture matrix

These fixtures exercise the parser and the trust-policy boundary. Every
manifest and envelope is parsed as exact UTF-8 bytes; the parser must reject
anything that is not the canonical representation described in
`verification/update-signing-v2.md`.

| Fixture | Mutation | Expected result |
| --- | --- | --- |
| `manifest-positive.txt` + `signature-positive.txt` | Canonical 17-field unsigned manifest and active-key P-1363 signature | Accept parsing, pinned-key verification, and digest binding |
| `manifest-tampered.txt` | Changes only `size` from 56 to 57 | Reject the envelope/digest binding; never verify or launch the installer |
| missing field | Remove `expires_unix` | Reject: missing required field |
| duplicate field | Add a second `size` line | Reject: duplicate key |
| reordered fields | Swap two manifest lines | Reject: non-canonical field order and changed digest |
| unknown field | Add `comment=...` | Reject: unknown key |
| non-canonical bytes | BOM, CRLF, padding, or missing terminal LF | Reject: byte-level canonicalization failure |
| envelope count | Set `signature_count` to 0 or 4 | Reject: count must be 1 through 3 |
| envelope ordering | Put signature records in descending key-ID order | Reject: records are not strictly key-ID ascending |
| key binding | Use an unknown 64-hex key ID | Reject: key is not in the pinned keyring |
| signature encoding | DER, base64, uppercase, or non-128-hex signature | Reject: signature must be lowercase P-1363 `r || s` |
| policy downgrade | Change `authenticode=required` to `unsigned` without a newly valid app signature | Reject: manifest policy is part of the signed object |

The checked-in positive envelope was generated once by the offline active key
after the canonical bytes were frozen. It is a public verification vector, not
private key material. Its raw manifest SHA-256 is recorded in
`manifest-positive.expected`; the ECDSA digest is independently recomputed
from the two domain-separation strings and the exact manifest bytes.
