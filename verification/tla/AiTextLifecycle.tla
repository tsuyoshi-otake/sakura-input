---- MODULE AiTextLifecycle ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Actors, NoActor, Deadline, MaxClock, MaxRevision, MaxRequests, MaxPresses

Phases == {"Idle", "Running", "Complete", "Detached"}
Results == {"None", "Success", "Failure", "Missing"}

VARIABLES phase, owner, sourceRevision, focusGeneration, scopeNormal,
          capturedSource, capturedFocus, startedAt, result, latch,
          requestCount, physicalPressCount, appliedCount, badApply, clock

vars == <<phase, owner, sourceRevision, focusGeneration, scopeNormal,
          capturedSource, capturedFocus, startedAt, result, latch,
          requestCount, physicalPressCount, appliedCount, badApply, clock>>

Init ==
  /\ phase = [a \in Actors |-> "Idle"]
  /\ owner = NoActor
  /\ sourceRevision = 0
  /\ focusGeneration = 0
  /\ scopeNormal = TRUE
  /\ capturedSource = [a \in Actors |-> 0]
  /\ capturedFocus = [a \in Actors |-> 0]
  /\ startedAt = [a \in Actors |-> 0]
  /\ result = [a \in Actors |-> "None"]
  /\ latch = [a \in Actors |-> FALSE]
  /\ requestCount = 0
  /\ physicalPressCount = 0
  /\ appliedCount = 0
  /\ badApply = FALSE
  /\ clock = 0

Start(a) ==
  /\ phase[a] = "Idle"
  /\ owner = NoActor
  /\ scopeNormal
  /\ ~latch[a]
  /\ requestCount < MaxRequests
  /\ physicalPressCount < MaxPresses
  /\ phase' = [phase EXCEPT ![a] = "Running"]
  /\ owner' = a
  /\ capturedSource' = [capturedSource EXCEPT ![a] = sourceRevision]
  /\ capturedFocus' = [capturedFocus EXCEPT ![a] = focusGeneration]
  /\ startedAt' = [startedAt EXCEPT ![a] = clock]
  /\ result' = [result EXCEPT ![a] = "None"]
  /\ latch' = [latch EXCEPT ![a] = TRUE]
  /\ requestCount' = requestCount + 1
  /\ physicalPressCount' = physicalPressCount + 1
  /\ UNCHANGED <<sourceRevision, focusGeneration, scopeNormal,
                  appliedCount, badApply, clock>>

RepeatPress(a) ==
  /\ latch[a]
  /\ physicalPressCount < MaxPresses
  /\ physicalPressCount' = physicalPressCount + 1
  /\ UNCHANGED <<phase, owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, result, latch,
                  requestCount, appliedCount, badApply, clock>>

KeyUp(a) ==
  /\ latch[a]
  /\ latch' = [latch EXCEPT ![a] = FALSE]
  /\ UNCHANGED <<phase, owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, result,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

Complete(a, value) ==
  /\ phase[a] = "Running"
  /\ owner = a
  /\ value \in {"Success", "Failure", "Missing"}
  /\ phase' = [phase EXCEPT ![a] = "Complete"]
  /\ result' = [result EXCEPT ![a] = value]
  /\ UNCHANGED <<owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, latch,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

ValidForApply(a) ==
  /\ result[a] = "Success"
  /\ capturedSource[a] = sourceRevision
  /\ capturedFocus[a] = focusGeneration
  /\ scopeNormal

PollApply(a) ==
  /\ phase[a] = "Complete"
  /\ owner = a
  /\ ValidForApply(a)
  /\ phase' = [phase EXCEPT ![a] = "Idle"]
  /\ owner' = NoActor
  /\ result' = [result EXCEPT ![a] = "None"]
  /\ appliedCount' = appliedCount + 1
  /\ badApply' = badApply \/ ~ValidForApply(a)
  /\ UNCHANGED <<sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, latch,
                  requestCount, physicalPressCount, clock>>

PollReject(a) ==
  /\ phase[a] = "Complete"
  /\ owner = a
  /\ ~ValidForApply(a)
  /\ phase' = [phase EXCEPT ![a] = "Idle"]
  /\ owner' = NoActor
  /\ result' = [result EXCEPT ![a] = "None"]
  /\ UNCHANGED <<sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, latch,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

PollTerminal(a) == PollApply(a) \/ PollReject(a)

Cancel(a) ==
  /\ phase[a] \in {"Running", "Complete"}
  /\ owner = a
  /\ phase' = [phase EXCEPT ![a] = "Detached"]
  /\ result' = [result EXCEPT ![a] = "None"]
  /\ UNCHANGED <<owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, latch,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

Timeout(a) ==
  /\ phase[a] = "Running"
  /\ owner = a
  /\ clock >= startedAt[a] + Deadline
  /\ phase' = [phase EXCEPT ![a] = "Detached"]
  /\ UNCHANGED <<owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, result, latch,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

WorkerExit(a) ==
  /\ phase[a] = "Detached"
  /\ owner = a
  /\ phase' = [phase EXCEPT ![a] = "Idle"]
  /\ owner' = NoActor
  /\ UNCHANGED <<sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, result, latch,
                  requestCount, physicalPressCount, appliedCount, badApply, clock>>

ChangeSource ==
  /\ sourceRevision < MaxRevision
  /\ sourceRevision' = sourceRevision + 1
  /\ UNCHANGED <<phase, owner, focusGeneration, scopeNormal, capturedSource,
                  capturedFocus, startedAt, result, latch, requestCount,
                  physicalPressCount, appliedCount, badApply, clock>>

ChangeFocus ==
  /\ focusGeneration < MaxRevision
  /\ focusGeneration' = focusGeneration + 1
  /\ UNCHANGED <<phase, owner, sourceRevision, scopeNormal, capturedSource,
                  capturedFocus, startedAt, result, latch, requestCount,
                  physicalPressCount, appliedCount, badApply, clock>>

ToggleScope ==
  /\ scopeNormal' = ~scopeNormal
  /\ UNCHANGED <<phase, owner, sourceRevision, focusGeneration, capturedSource,
                  capturedFocus, startedAt, result, latch, requestCount,
                  physicalPressCount, appliedCount, badApply, clock>>

Tick ==
  /\ clock < MaxClock
  /\ clock' = clock + 1
  /\ UNCHANGED <<phase, owner, sourceRevision, focusGeneration, scopeNormal,
                  capturedSource, capturedFocus, startedAt, result, latch,
                  requestCount, physicalPressCount, appliedCount, badApply>>

Next ==
  \/ \E a \in Actors : Start(a) \/ RepeatPress(a) \/ KeyUp(a)
  \/ \E a \in Actors, value \in {"Success", "Failure", "Missing"} : Complete(a, value)
  \/ \E a \in Actors : PollApply(a) \/ PollReject(a) \/ Cancel(a) \/ Timeout(a) \/ WorkerExit(a)
  \/ ChangeSource \/ ChangeFocus \/ ToggleScope \/ Tick

Spec == Init /\ [][Next]_vars
  /\ (\A a \in Actors : WF_vars(Complete(a, "Success")))
  /\ (\A a \in Actors : WF_vars(PollTerminal(a)))
  /\ (\A a \in Actors : WF_vars(WorkerExit(a)))

TypeOK ==
  /\ phase \in [Actors -> Phases]
  /\ owner \in Actors \cup {NoActor}
  /\ result \in [Actors -> Results]
  /\ latch \in [Actors -> BOOLEAN]

AtMostOneJob == Cardinality({a \in Actors : phase[a] /= "Idle"}) <= 1
OwnerMatches ==
  (owner = NoActor) <=> (\A a \in Actors : phase[a] = "Idle")
NoStaleApply == ~badApply
RequestAccounting == appliedCount <= requestCount /\ requestCount <= physicalPressCount
DetachedRetainsCapacity == \A a \in Actors : phase[a] = "Detached" => owner = a
EveryJobTerminates == \A a \in Actors : [](phase[a] /= "Idle" => <>(phase[a] = "Idle"))

=============================================================================
