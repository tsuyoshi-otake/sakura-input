--------------------------- MODULE EngineRecovery ---------------------------
EXTENDS Integers, FiniteSets

(***************************************************************************
This is a behavioral model of the user-visible recovery contract.  It does
not mirror the Rust types or callback implementation.  Actors issue requests;
an engine response may arrive or time out; one document-wide recovery fence
may own an asynchronous finalizer; host keys, lifecycle changes, duplicate
callbacks, and callbacks completed out of order can then interleave.
***************************************************************************)

CONSTANTS Actors, MaxClock, MaxVersion, MaxToken, QueueCap, MaxEvents

ASSUME /\ Actors # {}
       /\ MaxClock \in Nat
       /\ MaxVersion \in Nat
       /\ MaxToken \in Nat \ {0}
       /\ QueueCap \in Nat \ {0}
       /\ MaxEvents \in Nat \ {0}

Tokens == 1..MaxToken
RequestStates == {"Idle", "Waiting"}
Outcomes == {"None", "Applied", "Rejected", "Cancelled"}

VARIABLES
    clock,
    request,
    startedAt,
    hostVersion,
    pending,
    pendingActor,
    pendingVersion,
    queue,
    nextToken,
    terminalOutcome,
    terminalCount,
    appliedVersion,
    consumedKeys,
    deduplicated,
    duplicateCallbacks,
    staleApplied

vars == <<clock, request, startedAt, hostVersion, pending, pendingActor,
          pendingVersion, queue, nextToken, terminalOutcome, terminalCount,
          appliedVersion, consumedKeys, deduplicated, duplicateCallbacks,
          staleApplied>>

Init ==
    /\ clock = 0
    /\ request = [a \in Actors |-> "Idle"]
    /\ startedAt = [a \in Actors |-> 0]
    /\ hostVersion = 0
    /\ pending = 0
    /\ pendingActor \in Actors
    /\ pendingVersion = 0
    /\ queue = {}
    /\ nextToken = 1
    /\ terminalOutcome = [t \in Tokens |-> "None"]
    /\ terminalCount = [t \in Tokens |-> 0]
    /\ appliedVersion = [t \in Tokens |-> -1]
    /\ consumedKeys = [a \in Actors |-> 0]
    /\ deduplicated = [a \in Actors |-> 0]
    /\ duplicateCallbacks = [t \in Tokens |-> 0]
    /\ staleApplied = FALSE

StartRequest(a) ==
    /\ request[a] = "Idle"
    /\ request' = [request EXCEPT ![a] = "Waiting"]
    /\ startedAt' = [startedAt EXCEPT ![a] = clock]
    /\ UNCHANGED <<clock, hostVersion, pending, pendingActor, pendingVersion,
                    queue, nextToken, terminalOutcome, terminalCount,
                    appliedVersion, consumedKeys, deduplicated,
                    duplicateCallbacks, staleApplied>>

FastReply(a) ==
    /\ request[a] = "Waiting"
    /\ request' = [request EXCEPT ![a] = "Idle"]
    /\ UNCHANGED <<clock, startedAt, hostVersion, pending, pendingActor,
                    pendingVersion, queue, nextToken, terminalOutcome,
                    terminalCount, appliedVersion, consumedKeys,
                    deduplicated, duplicateCallbacks, staleApplied>>

Tick ==
    /\ clock < MaxClock
    /\ clock' = clock + 1
    /\ UNCHANGED <<request, startedAt, hostVersion, pending, pendingActor,
                    pendingVersion, queue, nextToken, terminalOutcome,
                    terminalCount, appliedVersion, consumedKeys,
                    deduplicated, duplicateCallbacks, staleApplied>>

Timeout(a) ==
    /\ request[a] = "Waiting"
    /\ (clock > startedAt[a] \/ clock = MaxClock)
    /\ request' = [request EXCEPT ![a] = "Idle"]
    /\ IF pending # 0
          THEN /\ deduplicated' =
                       [deduplicated EXCEPT ![a] =
                           IF @ < MaxEvents THEN @ + 1 ELSE @]
               /\ UNCHANGED <<pending, pendingActor, pendingVersion, queue,
                               nextToken, terminalOutcome, terminalCount>>
          ELSE IF nextToken > MaxToken
          THEN /\ UNCHANGED <<pending, pendingActor, pendingVersion, queue,
                               nextToken, terminalOutcome, terminalCount,
                               deduplicated>>
          ELSE IF Cardinality(queue) >= QueueCap
          THEN /\ pending' = 0
               /\ UNCHANGED <<pendingActor, pendingVersion, queue>>
               /\ terminalOutcome' =
                       [terminalOutcome EXCEPT ![nextToken] = "Rejected"]
               /\ terminalCount' =
                       [terminalCount EXCEPT ![nextToken] = 1]
               /\ nextToken' = nextToken + 1
               /\ UNCHANGED deduplicated
          ELSE /\ pending' = nextToken
               /\ pendingActor' = a
               /\ pendingVersion' = hostVersion
               /\ queue' = queue \cup {nextToken}
               /\ nextToken' = nextToken + 1
               /\ UNCHANGED <<terminalOutcome, terminalCount, deduplicated>>
    /\ UNCHANGED <<clock, startedAt, hostVersion, appliedVersion,
                    consumedKeys, duplicateCallbacks, staleApplied>>

HostKey(a) ==
    \/ /\ pending = 0
       /\ hostVersion < MaxVersion
       /\ hostVersion' = hostVersion + 1
       /\ UNCHANGED consumedKeys
    \/ /\ pending # 0
       /\ consumedKeys[a] < MaxEvents
       /\ consumedKeys' =
               [consumedKeys EXCEPT ![a] = @ + 1]
       /\ UNCHANGED hostVersion
    /\ UNCHANGED <<clock, request, startedAt, pending, pendingActor,
                    pendingVersion, queue, nextToken, terminalOutcome,
                    terminalCount, appliedVersion, deduplicated,
                    duplicateCallbacks, staleApplied>>

CancelPending ==
    /\ pending # 0
    /\ pending' = 0
    /\ terminalOutcome' =
            [terminalOutcome EXCEPT ![pending] =
                IF @ = "None" THEN "Cancelled" ELSE @]
    /\ terminalCount' =
            [terminalCount EXCEPT ![pending] =
                IF terminalOutcome[pending] = "None" THEN 1 ELSE @]
    /\ UNCHANGED <<clock, request, startedAt, hostVersion, pendingActor,
                    pendingVersion, queue, nextToken, appliedVersion,
                    consumedKeys, deduplicated, duplicateCallbacks,
                    staleApplied>>

ExternalChange ==
    /\ hostVersion < MaxVersion
    /\ hostVersion' = hostVersion + 1
    /\ IF pending # 0
          THEN /\ pending' = 0
               /\ terminalOutcome' =
                       [terminalOutcome EXCEPT ![pending] =
                           IF @ = "None" THEN "Cancelled" ELSE @]
               /\ terminalCount' =
                       [terminalCount EXCEPT ![pending] =
                           IF terminalOutcome[pending] = "None" THEN 1 ELSE @]
          ELSE /\ UNCHANGED <<pending, terminalOutcome, terminalCount>>
    /\ UNCHANGED <<clock, request, startedAt, pendingActor, pendingVersion,
                    queue, nextToken, appliedVersion, consumedKeys,
                    deduplicated, duplicateCallbacks, staleApplied>>

CompleteAny(t) ==
    /\ t \in queue
    /\ queue' = queue \ {t}
    /\ IF t = pending
          THEN /\ pending' = 0
               /\ IF terminalOutcome[t] = "None" /\ hostVersion = pendingVersion
                     THEN /\ terminalOutcome' =
                                  [terminalOutcome EXCEPT ![t] = "Applied"]
                          /\ terminalCount' =
                                  [terminalCount EXCEPT ![t] = 1]
                          /\ appliedVersion' =
                                  [appliedVersion EXCEPT ![t] = hostVersion]
                          /\ UNCHANGED staleApplied
                     ELSE /\ terminalOutcome' =
                                  [terminalOutcome EXCEPT ![t] =
                                      IF @ = "None" THEN "Rejected" ELSE @]
                          /\ terminalCount' =
                                  [terminalCount EXCEPT ![t] =
                                      IF terminalOutcome[t] = "None" THEN 1 ELSE @]
                          /\ staleApplied' =
                                  staleApplied \/ (hostVersion # pendingVersion)
                          /\ UNCHANGED appliedVersion
          ELSE /\ UNCHANGED <<pending, terminalOutcome, terminalCount,
                               appliedVersion, staleApplied>>
    /\ UNCHANGED <<clock, request, startedAt, hostVersion, pendingActor,
                    pendingVersion, nextToken, consumedKeys, deduplicated,
                    duplicateCallbacks>>

DuplicateCompletion(t) ==
    /\ t \in Tokens
    /\ terminalOutcome[t] # "None"
    /\ duplicateCallbacks[t] < MaxEvents
    /\ duplicateCallbacks' =
            [duplicateCallbacks EXCEPT ![t] = @ + 1]
    /\ UNCHANGED <<clock, request, startedAt, hostVersion, pending,
                    pendingActor, pendingVersion, queue, nextToken,
                    terminalOutcome, terminalCount, appliedVersion,
                    consumedKeys, deduplicated, staleApplied>>

ResolveEngine(a) == FastReply(a) \/ Timeout(a)
ResolvePending == CancelPending \/ (\E t \in Tokens : CompleteAny(t))

Next ==
    \/ Tick
    \/ ExternalChange
    \/ CancelPending
    \/ \E a \in Actors :
           StartRequest(a) \/ FastReply(a) \/ Timeout(a) \/ HostKey(a)
    \/ \E t \in Tokens : CompleteAny(t) \/ DuplicateCompletion(t)

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A a \in Actors : WF_vars(ResolveEngine(a))
    /\ WF_vars(ResolvePending)

TypeOK ==
    /\ clock \in 0..MaxClock
    /\ request \in [Actors -> RequestStates]
    /\ startedAt \in [Actors -> 0..MaxClock]
    /\ hostVersion \in 0..MaxVersion
    /\ pending \in (Tokens \cup {0})
    /\ pendingActor \in Actors
    /\ pendingVersion \in 0..MaxVersion
    /\ queue \subseteq Tokens
    /\ nextToken \in 1..(MaxToken + 1)
    /\ terminalOutcome \in [Tokens -> Outcomes]
    /\ terminalCount \in [Tokens -> 0..1]
    /\ appliedVersion \in [Tokens -> (-1)..MaxVersion]
    /\ consumedKeys \in [Actors -> 0..MaxEvents]
    /\ deduplicated \in [Actors -> 0..MaxEvents]
    /\ duplicateCallbacks \in [Tokens -> 0..MaxEvents]
    /\ staleApplied \in BOOLEAN

QueueBounded == Cardinality(queue) <= QueueCap
PendingBackedByCallback == pending = 0 \/ pending \in queue
PendingVersionIsCurrent == pending = 0 \/ pendingVersion = hostVersion
PendingIsNotTerminal ==
    pending = 0 \/ terminalOutcome[pending] = "None"
TerminalAtMostOnce == \A t \in Tokens : terminalCount[t] <= 1
NoStaleReplay == staleApplied = FALSE

RecoveryEventuallyClears == pending # 0 ~> pending = 0
EngineWaitEventuallyTerminates ==
    \A a \in Actors : request[a] = "Waiting" ~> request[a] = "Idle"

=============================================================================
