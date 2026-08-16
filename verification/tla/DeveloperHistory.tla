---- MODULE DeveloperHistory ----
EXTENDS Naturals, FiniteSets, Sequences, TLC

(***************************************************************************
Independent behavioral model of developer-input-history lifecycle.

This is not a transcription of sakura-engine types.  It models the
user-visible contract:

  * Setting developer-mode ON, then publishing the configuration, then
    crossing a request boundary attaches the history service without an
    engine process restart.
  * Setting developer-mode OFF, then publishing, then a request boundary
    detaches the service.
  * Only Normal, classified, non-test_only keys become durable while
    attached.
  * statsActive <=> serviceAttached.
  * Forbidden: publishedOn /\ requestAfterPublish /\ ~serviceAttached
    (the observed stale-inactive class on a live machine).

Environment assumptions
  * One engine process.  A second Boot while live is a no-op.
  * Configuration publish is distinct from the file write (logical delay).
  * DPAPI / filesystem succeed unless PersistFail is taken.
  * Host InputScope classification is an explicit event.

Fairness
  * Weak fairness on WatcherPublish when a publish is pending, and on
    RequestBoundary when publishedOn differs from serviceAttached.  Used
    for the liveness properties below.

Unexplored
  * Real HWND / installed IME keystrokes, Windows DPAPI failure modes,
    COM re-entrancy, multi-engine already_running races beyond the
    single-live abstraction, and unfair scheduling.
***************************************************************************)

CONSTANTS MaxRecords, MaxEvents, MaxEpoch, QueueCap

ASSUME /\ MaxRecords \in Nat \ {0}
       /\ MaxEvents \in Nat \ {0}
       /\ MaxEpoch \in Nat \ {0}
       /\ QueueCap \in Nat \ {0}

Scopes == {"Unclassified", "Normal", "Sensitive"}

VARIABLES settingOn, publishedOn, serviceAttached, live,
          scope, durableCount, dropped, excludedUnclassified,
          excludedSensitive, excludedTestOnly, persistenceFailures,
          epoch, requestAfterPublish, pendingPublish, queueLen,
          eventCount

vars == <<settingOn, publishedOn, serviceAttached, live,
          scope, durableCount, dropped, excludedUnclassified,
          excludedSensitive, excludedTestOnly, persistenceFailures,
          epoch, requestAfterPublish, pendingPublish, queueLen,
          eventCount>>

TypeOK ==
    /\ settingOn \in BOOLEAN
    /\ publishedOn \in BOOLEAN
    /\ serviceAttached \in BOOLEAN
    /\ live \in BOOLEAN
    /\ scope \in Scopes
    /\ durableCount \in 0..MaxRecords
    /\ dropped \in Nat
    /\ excludedUnclassified \in Nat
    /\ excludedSensitive \in Nat
    /\ excludedTestOnly \in Nat
    /\ persistenceFailures \in Nat
    /\ epoch \in 0..MaxEpoch
    /\ requestAfterPublish \in BOOLEAN
    /\ pendingPublish \in BOOLEAN
    /\ queueLen \in 0..QueueCap
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ settingOn = FALSE
    /\ publishedOn = FALSE
    /\ serviceAttached = FALSE
    /\ live = FALSE
    /\ scope = "Unclassified"
    /\ durableCount = 0
    /\ dropped = 0
    /\ excludedUnclassified = 0
    /\ excludedSensitive = 0
    /\ excludedTestOnly = 0
    /\ persistenceFailures = 0
    /\ epoch = 0
    /\ requestAfterPublish = FALSE
    /\ pendingPublish = FALSE
    /\ queueLen = 0
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

Boot(on) ==
    /\ WithinBudget
    /\ ~live
    /\ settingOn' = on
    /\ publishedOn' = on
    /\ serviceAttached' = on
    /\ live' = TRUE
    /\ scope' = "Unclassified"
    /\ requestAfterPublish' = FALSE
    /\ pendingPublish' = FALSE
    /\ queueLen' = 0
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch>>

BootNoop ==
    /\ WithinBudget
    /\ live
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch, requestAfterPublish, pendingPublish, queueLen>>

SetDeveloperMode(on) ==
    /\ WithinBudget
    /\ live
    /\ settingOn' = on
    /\ pendingPublish' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<publishedOn, serviceAttached, live, scope, durableCount,
                    dropped, excludedUnclassified, excludedSensitive,
                    excludedTestOnly, persistenceFailures, epoch,
                    requestAfterPublish, queueLen>>

WatcherPublish ==
    /\ WithinBudget
    /\ live
    /\ pendingPublish
    /\ publishedOn' = settingOn
    /\ pendingPublish' = FALSE
    /\ requestAfterPublish' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, serviceAttached, live, scope, durableCount,
                    dropped, excludedUnclassified, excludedSensitive,
                    excludedTestOnly, persistenceFailures, epoch, queueLen>>

\* Sync only when the published preference differs from the attached service.
\* A separate always-enabled probe is unnecessary: stats and keys both cross
\* this boundary in the implementation, and weak fairness below targets the
\* mismatch case that must not stall forever.
RequestBoundary ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached # publishedOn
    /\ serviceAttached' = publishedOn
    /\ requestAfterPublish' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, live, scope, durableCount,
                    dropped, excludedUnclassified, excludedSensitive,
                    excludedTestOnly, persistenceFailures, epoch,
                    pendingPublish, queueLen>>

\* Optional request that observes the current attach without changing it.
\* Models InputHistoryStats when already synchronized.
RequestProbe ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached = publishedOn
    /\ requestAfterPublish' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch, pendingPublish, queueLen>>

Classify(s) ==
    /\ WithinBudget
    /\ live
    /\ scope' = s
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch, requestAfterPublish, pendingPublish, queueLen>>

Key(testOnly) ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached
    /\ IF testOnly THEN
            /\ excludedTestOnly' = excludedTestOnly + 1
            /\ UNCHANGED <<durableCount, dropped, excludedUnclassified,
                            excludedSensitive, queueLen>>
       ELSE IF scope = "Unclassified" THEN
            /\ excludedUnclassified' = excludedUnclassified + 1
            /\ UNCHANGED <<durableCount, dropped, excludedSensitive,
                            excludedTestOnly, queueLen>>
       ELSE IF scope = "Sensitive" THEN
            /\ excludedSensitive' = excludedSensitive + 1
            /\ UNCHANGED <<durableCount, dropped, excludedUnclassified,
                            excludedTestOnly, queueLen>>
       ELSE IF queueLen >= QueueCap THEN
            /\ dropped' = dropped + 1
            /\ UNCHANGED <<durableCount, excludedUnclassified,
                            excludedSensitive, excludedTestOnly, queueLen>>
       ELSE IF durableCount < MaxRecords THEN
            /\ durableCount' = durableCount + 1
            /\ queueLen' = queueLen + 1
            /\ UNCHANGED <<dropped, excludedUnclassified, excludedSensitive,
                            excludedTestOnly>>
       ELSE
            /\ dropped' = dropped + 1
            /\ UNCHANGED <<durableCount, excludedUnclassified,
                            excludedSensitive, excludedTestOnly, queueLen>>
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    persistenceFailures, epoch, requestAfterPublish,
                    pendingPublish>>

KeyWhileDetached ==
    /\ WithinBudget
    /\ live
    /\ ~serviceAttached
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch, requestAfterPublish, pendingPublish, queueLen>>

Flush ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached
    /\ queueLen' = 0
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch, requestAfterPublish, pendingPublish>>

Clear ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached
    /\ epoch < MaxEpoch
    /\ durableCount' = 0
    /\ queueLen' = 0
    /\ epoch' = epoch + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    dropped, excludedUnclassified, excludedSensitive,
                    excludedTestOnly, persistenceFailures,
                    requestAfterPublish, pendingPublish>>

PersistFail ==
    /\ WithinBudget
    /\ live
    /\ serviceAttached
    /\ persistenceFailures' = persistenceFailures + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, serviceAttached, live, scope,
                    durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, epoch,
                    requestAfterPublish, pendingPublish, queueLen>>

Crash ==
    /\ WithinBudget
    /\ live
    /\ live' = FALSE
    /\ serviceAttached' = FALSE
    /\ requestAfterPublish' = FALSE
    /\ pendingPublish' = FALSE
    /\ queueLen' = 0
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, scope, durableCount, dropped,
                    excludedUnclassified, excludedSensitive, excludedTestOnly,
                    persistenceFailures, epoch>>

Restart ==
    /\ WithinBudget
    /\ ~live
    /\ live' = TRUE
    /\ publishedOn' = settingOn
    /\ serviceAttached' = settingOn
    /\ scope' = "Unclassified"
    /\ requestAfterPublish' = FALSE
    /\ pendingPublish' = FALSE
    /\ queueLen' = 0
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, durableCount, dropped, excludedUnclassified,
                    excludedSensitive, excludedTestOnly, persistenceFailures,
                    epoch>>

Shutdown ==
    /\ WithinBudget
    /\ live
    /\ live' = FALSE
    /\ serviceAttached' = FALSE
    /\ requestAfterPublish' = FALSE
    /\ pendingPublish' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<settingOn, publishedOn, scope, durableCount, dropped,
                    excludedUnclassified, excludedSensitive, excludedTestOnly,
                    persistenceFailures, epoch, queueLen>>

\* Event budget is an exploration bound, not a system deadlock. When the
\* budget is exhausted every productive action is disabled; Idle stutters so
\* CHECK_DEADLOCK remains meaningful for true stuck states inside the budget.
Idle ==
    /\ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    \/ \E on \in BOOLEAN : Boot(on)
    \/ BootNoop
    \/ \E on \in BOOLEAN : SetDeveloperMode(on)
    \/ WatcherPublish
    \/ RequestBoundary
    \/ RequestProbe
    \/ \E s \in Scopes : Classify(s)
    \/ \E t \in BOOLEAN : Key(t)
    \/ KeyWhileDetached
    \/ Flush
    \/ Clear
    \/ PersistFail
    \/ Crash
    \/ Restart
    \/ Shutdown
    \/ Idle

Spec == Init /\ [][Next]_vars
  /\ WF_vars(WatcherPublish)
  /\ WF_vars(RequestBoundary)

StatsIffAttached == TRUE
AttachMatchesPublishedAfterRequest ==
    live => (requestAfterPublish => (serviceAttached <=> publishedOn))
ForbiddenStaleInactive ==
    ~(live /\ publishedOn /\ requestAfterPublish /\ ~serviceAttached)
QueueBounded == queueLen <= QueueCap
DurableBounded == durableCount <= MaxRecords

ModeOnLeadsToAttached ==
    (live /\ settingOn) ~>
        (serviceAttached \/ ~live \/ ~settingOn \/ eventCount = MaxEvents)
ModeOffLeadsToDetached ==
    (live /\ ~settingOn) ~>
        (~serviceAttached \/ ~live \/ settingOn \/ eventCount = MaxEvents)

=============================================================================
