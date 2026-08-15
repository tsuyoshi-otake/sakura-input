# Boundary inventory — Shift-Latin backspace / retype

## Path classification

The defect lives in the **in-engine composition state machine**.

A keystroke becomes a `sakura_proto::Request::SendKey` on the named pipe,
then `Dispatcher::dispatch` → `apply_key` → `feed_character` /
`apply_backspace` / `move_caret`. Visible English text is `Session.raw_input`.
No DPAPI history write, no AI worker, no neural worker, and no TSF write
journal is required for the character-order contract: the engine answers
synchronously with an `OutputBuf` preedit.

## States

| State | Meaning |
|---|---|
| Idle | empty buffers, `shifted_ascii = false` |
| EnglishComposing | latch set, `raw_input` is the visible buffer |
| Converting | candidate list owns keys; Backspace cancels |
| Predicting | suggestion list focused; Backspace still deletes |

## Events

`ShiftLatin`, `Latin`, `Backspace` (shift or not), `Delete`, caret moves,
`Convert`, `Cancel`, `Commit`. Shift+Backspace must resolve to `DeleteBack`
while composing, otherwise the host also edits the composition.

## Transitions

Idle + Shift+letter → EnglishComposing. Backspace at caret-1. Empty buffer
→ Idle (latch released, #51). Convert → Converting. Cancel/Commit → Idle.

## Persistence / external boundaries

| Boundary | On this path? | Evidence |
|---|---|---|
| Engine IPC (`SendKey` / `Output`) | Yes, protocol only | Contract tests in `shift_latin_order_tests::contract` |
| TSF write journal | Protocol + journal, not a live HWND | `shift_latin_then_shift_backspace_roundtrip_keeps_press_order` drives `Engine::send_key` over a fake pipe; `shift_latin_backspace_retype_plans_commit_in_order_and_never_aiuoeo` applies AIUEO→AIUE→AIUEO and rejects a host-stolen AIUOEO attach |
| History DPAPI | No | `test_only` / Normal-scope gates unchanged |
| AI / neural workers | No | `shifted_ascii` already excludes long conversion |

## Failure-injection coverage

| Mode | Applies? | How |
|---|---|---|
| Boundary values | Yes | empty Backspace; `MAX_PREEDIT_BYTES` insert |
| Partial failure | Yes | idle Shift+Backspace is not consumed; later keys still order |
| Retry | Yes | duplicate Backspace after empty is idempotent on the buffer |
| Duplicate | Yes | same as retry |
| Drop | Yes | unbound Ctrl chord is not consumed and does not reorder |
| Reorder | Yes | reordered events follow the oracle, not the original intent |
| Cancel | Yes | Escape then retype starts a fresh buffer |
| Timeout | N/A | `SendKey` is synchronous; documented in contract test |
| Crash/restart | Yes | new `Dispatcher` has an empty composition |
| Resource exhaustion | Yes | overflow must not punch a mid-buffer hole |
| Recovery | Yes | restart + first new Shift+letter is a fresh latch |
