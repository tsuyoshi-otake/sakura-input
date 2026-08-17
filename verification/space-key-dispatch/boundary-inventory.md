# Boundary inventory — Space key dispatch

## Path classification

Idle Space insertion and Convert are **in-engine**, per named-pipe connection.
The defect is **cross-connection**: two workers, one physical Space.

## States

Idle, Composing, Converting (Predicting out of scope). Live vs disconnected.

## Events

Type, Space (focused or dual), Commit, Cancel, ReplaceContext, CrashRestart,
Disconnect, TimeoutSpace, DropSpace.

## Persistence / external

| Boundary | On this path? | Test |
|---|---|---|
| Engine IPC SendKey | Yes | `space_key_dispatch_pipe.rs` |
| Two pipe clients | Yes | `fail_dual_delivery_two_clients_*` |
| TSF write journal / HWND | No live COM | excluded |
| History DPAPI | No | excluded |
| Dictionary ranking | No | excluded |

## Failure-injection

See `requirements.md` FAIL-SPACE-* and `failure-injection/`.
Crash/restart kills the owned engine process and opens a new isolated process.
Exception injection is not used as a substitute.
