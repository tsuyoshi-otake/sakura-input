# Sakura Pad crypto and session worker (#269)

The renderer now calls the sibling `sakura_pad_session` executable for
whole-Pad password protection. Existing Pad files remain in the v2
current-user DPAPI format until the user explicitly enables protection; that
transaction publishes a protected v3 store. The installer inventory includes
the session executable, but no user installation or real-data migration has
been performed by this work. The older one-request `sakura_pad_worker`
executable remains an isolated prototype and is not installed. Recovery-key
and per-memo primitives in this crate are not connected to the Pad UI yet.
The [product plan](../../docs/plans/sakura-pad-protection.md) owns the
remaining flows.

## Current contract

The worker reads exactly one request from stdin, requires EOF, writes exactly
one response to stdout, and exits. It never reads/writes files, credentials,
the clipboard, network, IME history, or settings. It rejects command-line
arguments. Passwords and content must never be passed in arguments or the
environment. Its fixed watchdog exits after 30 seconds, including time spent
waiting for input; expiry yields no successful response. The owning caller
must concurrently drain stdout, enforce its own deadline, cancel/reap the
exact child, and invalidate the request generation before accepting a result.
This one-request path is a protocol prototype; the persistent session below
owns the unlocked keys for the renderer's whole-Pad password path.

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
`unlock` authenticates once and returns zeroizing plaintext with an opaque
`UnlockedEnvelope`. Its `reseal` method supports repeated changed saves without
reentering the password or repeating Argon2id. The session retains a zeroizing
data key and password-derived wrapping key, plus the authenticated header;
it does not retain the password or expose raw keys. Dropping the session clears
its owned keys. The v1 envelope is used by the persistent session below.
The envelope has an 89-byte header, a 48-byte wrapped random data key, and
ciphertext with a 16-byte authentication tag. The header binds format/cipher,
scope, exact KDF profile, salt, both nonces and plaintext length. Distinct AAD
purposes separate key wrapping from content encryption. There is no DPAPI or
weak-key alternative path. This envelope is not the future Pad document format.
Each `reseal` generates independent CNG wrap and payload nonces and rewraps the
same data key because the v1 wrapping AAD authenticates the full header,
including the payload nonce and length. The scope, KDF profile, and salt stay
bound to the original authenticated envelope.

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

## Persistent session

`sakura_pad_session` is a separate stdio binary launched by the renderer. It accepts
length-prefixed frames defined by the wire-only `sakura-pad-session-proto` crate,
authenticates an encrypted envelope once with `Unlock`, or enrolls a new one
with `Create`, then uses the opaque `UnlockedEnvelope` to `Reseal` changed plaintext without
another password. `Lock` drops the active keys and `Shutdown` drops them and
exits after a success response. Clean stdin EOF also ends the process.
The renderer owns the process lifetime, requests and save epochs, while
`PadStore` owns durable migration and protected publication.

The request body is `SKRPSS01`, nonzero u64 request ID, nonzero u64 generation,
u8 operation (1 Unlock, 2 Reseal, 3 Lock, 4 Shutdown, 5 Verify, 6 Create,
7 CreateRecoverable, 8 UnlockPasswordV2, 9 UnlockRecoveryV2), 16-byte vault ID,
u64 memo ID, u16 password length, u32 payload length, password, payload. A u32 body
length precedes it. Unlock and Create supply vault/memo/password; Unlock's
payload is an existing encrypted envelope, while Create's payload is initial
plaintext and its response is an encrypted envelope. Create seals and opens
once to retain an authenticated session; enrollment performs the KDF twice.
Reseal supplies plaintext. Verify supplies an
encrypted envelope and returns authenticated plaintext using the active keys.
Lock and Shutdown
carry no payload. Request IDs strictly increase for the child lifetime, and a
new Unlock generation must exceed every earlier generation. Failed Unlock
consumes its generation. A stale generation or repeated request ID receives a
content-free `Stale` response. Reseal without active keys receives `Locked`.
The response body is `SKRPSR01`, echoed ID and generation, u8 status, u32
payload length, payload, also length-prefixed. A malformed, truncated, or
oversized frame gets one `InvalidRequest` response with zero IDs and payload;
the worker then exits. All failure statuses have empty payloads. Password and
plaintext frame copies are zeroized on drop. These IDs provide correlation and
replay rejection within one trusted child, not peer authentication.

Frames are limited before allocation (8 MiB plaintext, at most 8 MiB plus 153
bytes of v1 envelope overhead or 214 bytes of v2 envelope overhead, and at
most 1024 password bytes). The recoverable creation response contains one
bounded, length-delimited envelope and a canonical 77-byte display key; the
protocol parser rejects truncated, extra, or malformed data. The v2 format
explicitly permits password **or** recovery-key unlock, with separately
authenticated data-key wraps. This is a recovery route, not a two-factor
policy. The UI currently uses only v1; v2 creation, key display/confirmation,
and recovery unlock are not available to users. A watchdog
terminates the child after 60 seconds without a completed response or five
minutes of total life, even with a blocked pipe. The parent must still own the
exact child, drain responses, enforce its own deadline, and reap it on cancel.
Watchdog expiry exits with code 124 and cannot guarantee a response frame.

Before shipping the complete protection plan: connect and verify a recovery
registration flow, independent per-memo keys and locked entries, YubiKey PRF,
optional local TOTP confirmation, idle/sleep behavior, UI Automation and
clipboard checks, and end-to-end failure recovery. This draft's renderer
already provides the password-based whole-Pad path, an authenticated protected
store cutover, locked native surface, explicit save epochs, session-lock
masking, and installer inventory. YubiKey PRF and TOTP are not implemented.

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
