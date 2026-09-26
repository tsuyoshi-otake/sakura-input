# Sakura Pad crypto worker — experimental foundation (#269)

This executable is not installed and the renderer does not call it. Existing
Pad files still use the v2 current-user DPAPI format. This crate alone does
not supply Pad passwords, unlocked sessions, recovery, or memo protection.
The [product plan](../../docs/plans/sakura-pad-protection.md) owns those flows.

## Current contract

The worker reads exactly one request from stdin, requires EOF, writes exactly
one response to stdout, and exits. It never reads/writes files, credentials,
the clipboard, network, IME history, or settings. It rejects command-line
arguments. Passwords and content must never be passed in arguments or the
environment. Its fixed watchdog exits after 30 seconds, including time spent
waiting for input; expiry yields no successful response. The owning caller
must concurrently drain stdout, enforce its own deadline, cancel/reap the
exact child, and invalidate the request generation before accepting a result.
This is a protocol prototype, not the future unlocked-key session service.

`protocol.rs` owns framing/limits and content-free terminal statuses. Integers
are little-endian. Request: `SKRPWR01`, u64 nonzero request ID, u8 operation
(1 seal / 2 open), 16-byte nonzero vault ID, u64 memo ID (0 = Pad), u16 password
byte length, u32 payload byte length, then password and payload. Passwords
are 1–1024 bytes, payloads at most 24 MiB, and trailing bytes are rejected.
The UI must separately enforce password enrollment strength and UTF-8 limits.
Response: `SKRPWS01`, u64 echoed request ID (0 for malformed request), u8
status, u32 payload length, then payload. Failed responses have zero payload.
There are no automatic retries. The ID is correlation, not authorization.

`envelope.rs` owns the experimental encrypted envelope. `seal` and `open`
authenticate a nonzero 16-byte vault ID and an optional nonzero memo ID.
The envelope has an 89-byte header, a 48-byte wrapped random data key, and
ciphertext with a 16-byte authentication tag. The header binds format/cipher,
scope, exact KDF profile, salt, both nonces and plaintext length. Distinct AAD
purposes separate key wrapping from content encryption. There is no DPAPI or
weak-key alternative path. This envelope is not the future Pad document format.

The password KDF is Argon2id v1.3, 64 MiB / 3 passes / 1 lane. Unknown profiles
and oversized inputs are rejected before KDF allocation. AES-256-GCM uses a
new random 32-byte data key, 16-byte salt, and two random 12-byte nonces on
every seal. Windows `BCryptGenRandom` is the only entropy source and failure
is terminal. Content is bounded to 8 MiB. The fixed profile is an initial
choice; the release tests exercise it but do not constitute hardware-wide
latency or memory-budget qualification.

## Secret lifetime and remaining integration work

Password/frame buffers, derived keys, unwrapped keys, KDF workspace, and
returned plaintext use `Zeroizing`. AES, GHASH and POLYVAL available clearing
features are enabled explicitly; `aes-gcm/zeroize` alone does not propagate
all of them. Clearing is best effort: the pinned POLYVAL autodetect wrapper
contains `ManuallyDrop` backend storage without its own Drop implementation,
so enabling its feature does not establish whole-cipher-state erasure. Stack,
register, allocator, pipe, OS and library copies are not claimed erased.
The one-request child lifetime bounds retention in this preparatory worker.

Before shipping: define the long-lived unlocked-key owner; independent
per-memo key access and recovery; authenticated policy and document schema;
atomic v1/v2 migration including backup/temp handling and fault injection;
renderer locking/masking/IME-input-scope behavior; trusted worker launch,
save/cancel epochs, idle/sleep/session-lock behavior, clipboard policy, and
installer inventory. YubiKey PRF and TOTP are not implemented here.

## Verification

Run the repo's `ci/run-test-quiet.ps1` wrapper around
`cargo test -p sakura-pad-worker -- --test-threads=1`; retain test output
through TKW and run `ci/check-process-clean.ps1` afterward. Use
`cargo clippy -p sakura-pad-worker --all-targets -- -D warnings`, the
dependency-policy gate and `cargo audit --file Cargo.lock` too.

The tests cover the RFC 9106 §5.3 independent Argon2id vector, actual CNG
randomness, round trips, wrong credentials/scope, malformed bounds, field
tampering, payload substitution, secret-free failure frames and real-process
stdio/cancellation. They are not UI, durable migration, YubiKey, full-memory
erasure, or production-authentication evidence.
