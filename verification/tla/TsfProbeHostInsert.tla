---- MODULE TsfProbeHostInsert ----
EXTENDS Naturals, TLC

(***************************************************************************
Independent behavioral model of OnTestKeyDown Space ownership.

This is not a transcription of sakura-tsf control flow. It models the
user-visible TSF contract around a live hiragana reading:

  * If OnTestKeyDown reports eaten, the host must not insert Space and
    OnKeyDown may convert.
  * If OnTestKeyDown reports not eaten, a Chromium host may insert Space
    into the visible reading and may skip OnKeyDown.
  * Probe timeout must not mark the live engine session desynchronized.

LocalClaim = TRUE is the product fix: live composition + Space/Henkan is
IME-owned before any engine Probe.

LocalClaim = FALSE is the defect: Probe runs dictionary conversion, can
miss 50 ms, returns eaten=FALSE, and marks the link desynchronized.

Environment
  * One user, one composition, totally ordered keystrokes.
  * Logical time is eventCount. There is no wall clock.

Unexplored
  * Dual TSF delivery, write-journal epochs, COM re-entrancy, idle
    fullwidth Space, Ctrl+Space, and dictionary ranking.
***************************************************************************)

CONSTANTS LocalClaim, MaxEvents

ASSUME /\ LocalClaim \in BOOLEAN
       /\ MaxEvents \in Nat \ {0}

VARIABLES phase, liveComposition, testEaten, realEaten,
          probeTimedOut, hostInsertedSpace, engineDesynchronized, eventCount

vars == <<phase, liveComposition, testEaten, realEaten,
          probeTimedOut, hostInsertedSpace, engineDesynchronized, eventCount>>

Phases == {"Idle", "Typed", "Tested", "Hosted", "Realed"}

TypeOK ==
    /\ phase \in Phases
    /\ liveComposition \in BOOLEAN
    /\ testEaten \in BOOLEAN
    /\ realEaten \in BOOLEAN
    /\ probeTimedOut \in BOOLEAN
    /\ hostInsertedSpace \in BOOLEAN
    /\ engineDesynchronized \in BOOLEAN
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ phase = "Idle"
    /\ liveComposition = FALSE
    /\ testEaten = FALSE
    /\ realEaten = FALSE
    /\ probeTimedOut = FALSE
    /\ hostInsertedSpace = FALSE
    /\ engineDesynchronized = FALSE
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

TypeReading ==
    /\ WithinBudget
    /\ phase = "Idle"
    /\ phase' = "Typed"
    /\ liveComposition' = TRUE
    /\ testEaten' = FALSE
    /\ realEaten' = FALSE
    /\ probeTimedOut' = FALSE
    /\ hostInsertedSpace' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED engineDesynchronized

TestKeyDown ==
    /\ WithinBudget
    /\ phase = "Typed"
    /\ liveComposition
    /\ IF LocalClaim
       THEN /\ testEaten' = TRUE
            /\ probeTimedOut' = FALSE
            /\ engineDesynchronized' = engineDesynchronized
       ELSE /\ testEaten' = FALSE
            /\ probeTimedOut' = TRUE
            /\ engineDesynchronized' = TRUE
    /\ phase' = "Tested"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<liveComposition, realEaten, hostInsertedSpace>>

HostInsert ==
    /\ WithinBudget
    /\ phase = "Tested"
    /\ ~testEaten
    /\ hostInsertedSpace' = TRUE
    /\ phase' = "Hosted"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<liveComposition, testEaten, realEaten,
                  probeTimedOut, engineDesynchronized>>

KeyDown ==
    /\ WithinBudget
    /\ phase = "Tested"
    /\ testEaten
    /\ realEaten' = TRUE
    /\ phase' = "Realed"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<liveComposition, testEaten, probeTimedOut,
                  hostInsertedSpace, engineDesynchronized>>

Done ==
    /\ phase \in {"Hosted", "Realed"} \/ eventCount = MaxEvents
    /\ UNCHANGED vars

Next == TypeReading \/ TestKeyDown \/ HostInsert \/ KeyDown \/ Done

Spec == Init /\ [][Next]_vars

NoHostSpaceDuringLiveComposition ==
    liveComposition => ~hostInsertedSpace

ProbeTimeoutDoesNotDesynchronize ==
    probeTimedOut => ~engineDesynchronized

NoDualOwnership ==
    ~(hostInsertedSpace /\ realEaten)

\* Reachability: a dedicated bug config expects this to be violated.
NeverHostInserts == ~hostInsertedSpace

=============================================================================
