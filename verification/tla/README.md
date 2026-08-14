# Engine timeout recovery model

`EngineRecovery.tla` is an implementation-independent behavioral model for the
document-visible contract around an ambiguous engine timeout. It models actors,
engine waits, logical time, a bounded callback queue, a one-owner recovery
fence, host document versions, lifecycle cancellation, external changes,
retries, duplicate callbacks, and callbacks delivered out of order.

Run all checked configurations from the repository root:

```powershell
rtk proxy powershell -NoProfile -ExecutionPolicy Bypass -File scripts/verify-engine-recovery-tlc.ps1 -JarPath <path-to-tla2tools.jar>
```

The script uses one TLC worker, seed `20260814`, fingerprint index 0, action
coverage, a per-configuration timeout, a fresh metadata directory, and an exact
Java-process survivor check. TLC's default deadlock check remains enabled.

## Checked properties

Safety invariants:

- `TypeOK`
- `QueueBounded`
- `PendingBackedByCallback`
- `PendingVersionIsCurrent`
- `PendingIsNotTerminal`
- `TerminalAtMostOnce`
- `NoStaleReplay`

Liveness properties under the weak-fairness assumptions in `Spec`:

- `RecoveryEventuallyClears`
- `EngineWaitEventuallyTerminates`

## Recorded TLC 2.19 run (2026-08-14)

The run used the TLA+ tools 1.7.4 distribution (`tla2tools.jar` SHA-256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`),
Java 11.0.31, `-workers 1 -coverage 1 -fp 0 -seed 20260814`.

| Configuration | Actors | Clock/version | Tokens/queue | Generated | Distinct | Depth |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `small` | 1 | 1 / 1 | 2 / 1 | 4,029 | 1,156 | 16 |
| `concurrent` | 2 | 1 / 1 | 1 / 1 | 28,234 | 5,680 | 16 |
| `reordered` | 1 | 1 / 1 | 3 / 2 | 37,453 | 8,516 | 21 |
| `boundary` | 1 | 2 / 2 | 2 / 1 | 12,878 | 3,474 | 18 |

All four searches completed with zero states left on the queue, no invariant or
liveness error, and no deadlock. The aggregate is 82,594 generated and 18,826
distinct states; configurations overlap, so the aggregate is not a count of
globally unique states. TLC action coverage reached every timeout allocation
branch across the suite: retry deduplication, token exhaustion, queue-capacity
rejection, and successful enqueue. Host key consumption, cancellation,
external change, completion, and duplicate completion were also all reached.

## Bounds and unexplored space

The model deliberately abstracts text to a monotonically changing document
version. It does not model Unicode contents, COM reference lifetimes, Windows'
actual scheduler, process crashes, renderer behavior, or engine IPC bytes. It
checks at most two actors, three recovery tokens, queue capacity two, document
version two, and logical clock two. Weak fairness assumes an enabled engine
resolution and an enabled current recovery terminal action are not postponed
forever.

Two larger exploratory configurations hit the script's 45-second bound and
have no pass conclusion: two actors / three tokens / queue two after saturated
clock timeouts were enabled, and two actors / clock two / version two / two
tokens / queue one / event bound two. Their state spaces, all larger actor
counts, larger queues and counters, unfair schedules, and real-time duration
remain unexplored.
