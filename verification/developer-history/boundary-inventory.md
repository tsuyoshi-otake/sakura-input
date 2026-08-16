# Developer-history persistence / external boundary inventory

Scope: hot-enable/disable of the developer input-history service and the
durable key path. Conversion ranking is out of scope.

| Boundary | Mechanism | Failure modes | Contract / injection |
|---|---|---|---|
| Settings file (`config.toml`) | `ConfigurationWatcher` fingerprint poll (~500 ms) | Corrupt TOML keeps last-good; publish delayed | Pipe e2e waits across poll; CLI retries stats |
| Named pipe admin | `InputHistoryStats` / `FlushInputHistory` / `ClearInputHistory` | Engine offline → offline path; timeout 2 s CLI | `pipe_round_trip` + proto roundtrip |
| History file (`%LOCALAPPDATA%\SakuraInput\history\input.bin`) | `SKIH` header, CRC frames, DPAPI, 64 MiB, repair truncate | Torn tail ignored; open repairs; size bound | `input_history` unit tests repair/compact |
| Writer thread | `sync_channel(1024)`, epoch-tagged Clear | Queue full → `dropped_events`; persist fail counter | Failure injection below + oracle `QueueFull`/`PersistFail` |
| Engine lifetime | Cold `main` open when `developer_mode`; hot `DynamicRuntimes` | Crash mid-write; restart restores durable + attach from setting | Oracle Crash/Restart; TLC crash cfg |
| Request boundary | `serve()` applies `runtime_services` then dispatch | Publish without request leaves detached (allowed) | Forbidden only after request: stale-inactive |
| Scope admission | Normal ∧ classified ∧ ¬test_only | Unclassified / sensitive / test_only excluded | Dispatcher + oracle concrete examples |

## Observed defect (forbidden)

Engine booted with history off → user set `developer-mode` ON after boot →
watcher published → `history stats` still `active=false` forever. That class is
`published_on ∧ request_after_publish ∧ ¬service_attached` and is forbidden by
the oracle, TLC Safety, and the hot-enable fix.
