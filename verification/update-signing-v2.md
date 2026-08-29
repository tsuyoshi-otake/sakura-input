# Sakura Input update-signing v2 contract

Status: frozen for Issue #90. The initial bridge release is v1.0.33 and the
initial `release_sequence` is 1. This document defines the bytes signed by a
release signer and the checks performed by a v2 updater. It does not contain
private key material; only the public keyring in
`data/update-signing/public-keys-v1.txt` is committed.

## Scope and bridge

GitHub Releases remain the transport. Existing HTTPS, repository and redirect
allow-lists, response-size limits, downloaded-file SHA-256 check, exact-file
identity guard, and installer process controls remain mandatory. The Sakura
signature authenticates the update manifest; it does not replace transport
security, hash verification, Authenticode policy, or Windows SmartScreen.

Schema-1 updaters cannot verify this contract. Therefore v1.0.33 is a manual
bridge release: users install it manually once, and only a v2-capable updater
may start automatic v2 updates. A schema-1 updater must not guess, ignore, or
reinterpret a v2 manifest or envelope.

The signed object is the canonical manifest. Its detached signature envelope
is named `release-manifest.sig` and is fetched from the same trusted release
asset set. A release is eligible only after manifest parsing, envelope
verification, downloaded-asset hash/size checks, and the platform trust policy
all succeed.

## Canonical manifest

The manifest is UTF-8 without a BOM, uses LF (`0x0a`) line endings, contains
exactly one terminal LF, and has no blank lines, comments, whitespace padding,
duplicate keys, or unknown keys. The exact field order is:

```text
schema
product
repository
channel
platform
trust_epoch
release_sequence
version
tag
source_commit
asset_name
installer_url
sha256
size
authenticode
minimum_updater_version
expires_unix
```

Each line is `key=value` followed by LF. Values are validated as follows:

| Field | Contract |
| --- | --- |
| `schema` | Exactly `2`. |
| `product` | Exactly `sakura-input`. |
| `repository` | Exactly `https://github.com/tsuyoshi-otake/sakura-input`. |
| `channel` | Exactly `stable` for the production channel. |
| `platform` | Exactly `windows-x64` for this updater. |
| `trust_epoch` | Decimal unsigned integer equal to the accepted keyring epoch. |
| `release_sequence` | Decimal unsigned integer, never reused, and greater than the installed accepted sequence. |
| `version` | Canonical dotted numeric semver; it must agree with `tag`. |
| `tag` | Exactly `v` followed by `version`. |
| `source_commit` | Exactly 40 lowercase hexadecimal characters. |
| `asset_name` | The allow-listed installer basename, exactly `sakura_setup.exe` for this channel. |
| `installer_url` | The canonical GitHub release URL for this repository, tag, and asset; no alternate host or query is accepted. |
| `sha256` | Exactly 64 lowercase hexadecimal characters. |
| `size` | Decimal unsigned byte count, within the updater's bounded download limit. |
| `authenticode` | Exactly `required` or `unsigned`; no omitted or third state is valid. |
| `minimum_updater_version` | Canonical dotted numeric semver no newer than the running updater. |
| `expires_unix` | Decimal Unix timestamp strictly later than the verification time. |

The parser compares the original bytes with its canonical serialization before
trusting any field. It must reject a reordered manifest, duplicate or missing
field, unknown field, BOM, CRLF, non-UTF-8 byte, extra whitespace, or missing
terminal LF. The exact bytes are retained through hashing and signature
verification; a parsed-and-reformatted equivalent is not the signed object.

## Domain separation and manifest digest

Let `M` be the exact canonical manifest byte sequence, including its one
terminal LF. The signature digest is:

```text
SHA256(
  UTF8("Sakura Input update manifest v2\0") ||
  UTF8("ecdsa-p256-sha256-p1363\0") ||
  M
)
```

The manifest's `manifest_sha256` is lowercase hexadecimal SHA-256 of `M`
alone. The positive fixture in
`verification/fixtures/update-signing-v2/manifest-positive.txt` has
`manifest_sha256=dea0acba514b2325a4c779cd270b8468f63ca8664fd1a1a6e5e3bc64d5594264`.
The fixture intentionally has no signed envelope because the release signer
generates that envelope after the manifest is frozen.

## Detached signature envelope

The envelope is UTF-8 without BOM, LF-only, exactly one terminal LF, and has
no extra fields:

```text
schema=1
algorithm=ecdsa-p256-sha256-p1363
manifest_sha256=<64 lowercase hex characters>
signature_count=<decimal integer from 1 through 3>
signature.0=<64 lowercase hex key id>:<128 lowercase hex characters>
signature.1=<64 lowercase hex key id>:<128 lowercase hex characters>
...
```

There are exactly `signature_count` records, numbered consecutively from zero.
Key IDs are strictly ascending lexicographically by their lowercase hex
bytes. Each signature is raw IEEE P-1363 `r || s` (32-byte `r` followed by
32-byte `s`), represented as exactly 128 lowercase hex characters. DER,
base64, uppercase hex, duplicate IDs, missing records, and extra records are
rejected.

For every record, the updater selects the public P-256 point by key ID and
verifies ECDSA-SHA256 over the domain-separated digest above. The key must be
in the pinned keyring, have the manifest's `trust_epoch`, and have a sequence
window containing the manifest's `release_sequence`. One valid signature is
enough for normal operation; 2-of-3 threshold policy is not implied by the
envelope's maximum of three records. During a planned rotation, dual signing
is recommended and both signatures must independently satisfy all checks.

## Public key IDs and sequence state

The keyring is `data/update-signing/public-keys-v1.txt`. Its key ID is derived
by the ceremony's fixed rule:

```text
lower_hex(SHA256(
  UTF8("Sakura Input update key v1\0") || X_32_bytes || Y_32_bytes
))
```

`X` and `Y` are the uncompressed P-256 affine coordinates, each exactly 32
big-endian bytes. The committed v1 keyring has trust epoch 1, an active key
and a standby key, both starting at sequence 1 and with no finite upper bound.
The file preserves the ceremony's exact records; envelope records still sort
by key ID rather than by role or file position.

The trust state of a key is explicit: `active` may authorize releases,
`standby` is pinned and may authorize only when the updater policy permits an
overlap or emergency use, `retired` is rejected for sequences outside its
closed window, and `revoked` is rejected for every sequence. An unknown key ID
is never upgraded into any trusted state. State changes are shipped in a new
updater/trust epoch; a release cannot change a key's state by modifying its
manifest.

`data/update-signing/release-sequence.txt` contains the initial reservation,
`1`. A signer reserves each sequence once, never reuses it, and increments it
monotonically even when a release is withdrawn. The updater stores the highest
accepted sequence per product/channel/platform and rejects rollback. Sequence
state is separate from version comparison: a numerically newer version with a
reused or lower sequence is still rejected.

## Authenticode and application-signature policy

The manifest's `authenticode` value is signed and therefore cannot be changed
after verification. WinVerifyTrust remains fail-closed and is invoked for both
policy values; this contract does not globally disable it.

| Manifest value | Sakura application signature | WinVerifyTrust result | Decision |
| --- | --- | --- | --- |
| `required` | Valid signature over the canonical manifest, accepted key/epoch/sequence | Valid for the configured Sakura publisher policy | Eligible, subject to all other guards |
| `required` | Missing, invalid, stale, unknown-key, or malformed | Any result | Reject |
| `unsigned` | Valid signature over the canonical manifest, accepted key/epoch/sequence | Not valid as the required Authenticode policy (for example unsigned) | Eligible, subject to all other guards |
| `unsigned` | Missing, invalid, stale, unknown-key, or malformed | Any result | Reject |
| either | Valid or invalid | Valid Authenticode when manifest says `unsigned` | Reject: the artifact contradicts its signed policy |
| either | Any | Unknown/error, unexpected publisher, or policy mismatch | Reject |

The `required` row refers to the configured Sakura publisher trust policy; this
contract does not invent a certificate subject or SPKI anchor. Those concrete
values remain deployment input. An unsigned row is an explicit owner-approved
manual-release policy, not a WinVerifyTrust bypass. In particular, a valid
Authenticode signature cannot silently downgrade or upgrade the signed policy.

The exact downloaded file identity guard is held continuously through asset
hash/size verification, application-signature verification, WinVerifyTrust,
and `ShellExecuteExW`. No path replacement, symlink/reparse redirection, or
TOCTOU gap may be introduced by the v2 path.

## Rotation, recovery, and manual operations

Rotation is forward-only. Publish a v2 updater containing the new public key
before signing a release with it; overlap the old and new keys, use dual
signatures while both are accepted, then retire the old key by a later trust
epoch and sequence window. A key ID is never reused for different coordinates.
The keyring is shipped with the updater, not learned from the release server.

There is no online revocation or automatic recovery from a lost private key.
If signing material is suspected compromised or unavailable, stop automatic
updates, publish a manually installed updater carrying a new trust epoch and
keyring, and resume only after a new sequence is reserved. Never recover,
copy, or log private key or DPAPI material in the repository or CI artifacts.
Schema-1/v1.0.33 users follow the same manual bridge until a v2-capable updater
is installed.

Every rejection (malformed bytes, stale sequence, expiry, key mismatch,
signature failure, hash/size mismatch, Authenticode policy failure, path
identity failure, or process-launch failure) is an explicit terminal state
with no installer launch. Retry and polling paths must remain bounded and
must not create an O(N) scan of the release history or keyring; the fixed
maximum of three envelope records and the small pinned keyring are constant
work per release.

## Fixtures and verification

`verification/fixtures/update-signing-v2/manifest-positive.txt` is the
canonical positive manifest and `manifest-positive.expected` records its
manifest digest. `manifest-tampered.txt` changes only `size` and must fail the
digest binding. `fixture-matrix.md` defines malformed, reordered, envelope,
key-binding, encoding, and policy-downgrade cases. The implementation must
run these cases before accepting a release and must preserve the exact-file
guard on every successful path.
