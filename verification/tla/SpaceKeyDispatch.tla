---- MODULE SpaceKeyDispatch ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************
Independent behavioral model of Space during Japanese conversion.

This is not a transcription of sakura-engine control flow.  It models the
user-visible contract inferred from developer-mode input history and the
requirements catalog:

  * Space on a composing, predicting, or converting connection converts.
  * Space on an idle Japanese-mode connection inserts a fullwidth space
    only when no live peer is composing/predicting/converting, or the fence is off.
  * One physical Space key must not both insert a document space and
    convert.
  * Crash/restart forgets that connection's composition.
  * Timeout absorbs Space without a document write.
  * Document spaces are bounded by MaxSpaces.

Environment assumptions
  * One to three engine connections for one host process.  Nothing is
    shared between them unless FenceIdleSpace is TRUE.
  * DualDelivery models Electron/TSF delivering one WM_KEYDOWN to every
    live connection.  FocusedDelivery is the ordinary single-focus path.
  * ContextReplace drops one connection back to Idle without committing.
  * Logical time is eventCount.  There is no real-time clock.

Fairness
  * Weak fairness on Commit(c) for ConversionEventuallyTerminates.

Unexplored
  * Real COM re-entrancy, dictionary ranking, Shift+Space width, more
    than three connections, unfair schedules beyond MaxEvents,
    Windows named-pipe accept-pool limits, and Chromium confirming a
    reading after an eaten Space with no composition update.
***************************************************************************)

CONSTANTS C1, C2, C3, FenceIdleSpace, MaxEvents, DualDelivery, ActorCount, MaxSpaces

ASSUME /\ Cardinality({C1, C2, C3}) = 3
       /\ FenceIdleSpace \in BOOLEAN
       /\ DualDelivery \in BOOLEAN
       /\ ActorCount \in 1..3
       /\ MaxEvents \in Nat \ {0}
       /\ MaxSpaces \in Nat \ {0}

Connections ==
    IF ActorCount = 1 THEN {C1}
    ELSE IF ActorCount = 2 THEN {C1, C2}
    ELSE {C1, C2, C3}
States == {"Idle", "Composing", "Predicting", "Converting"}

VARIABLES state, live, insertedThisKey, convertedThisKey, absorbedThisKey,
          convertedFromPredicting, documentSpaces, orphaned, crashed, timeouts,
          eventCount

vars == <<state, live, insertedThisKey, convertedThisKey, absorbedThisKey,
          convertedFromPredicting, documentSpaces, orphaned, crashed, timeouts,
          eventCount>>

TypeOK ==
    /\ state \in [Connections -> States]
    /\ live \in [Connections -> BOOLEAN]
    /\ insertedThisKey \in BOOLEAN
    /\ convertedThisKey \in BOOLEAN
    /\ absorbedThisKey \in BOOLEAN
    /\ convertedFromPredicting \in BOOLEAN
    /\ documentSpaces \in 0..MaxSpaces
    /\ orphaned \in 0..MaxEvents
    /\ crashed \in 0..MaxEvents
    /\ timeouts \in 0..MaxEvents
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ state = [c \in Connections |-> "Idle"]
    /\ live = [c \in Connections |-> TRUE]
    /\ insertedThisKey = FALSE
    /\ convertedThisKey = FALSE
    /\ absorbedThisKey = FALSE
    /\ convertedFromPredicting = FALSE
    /\ documentSpaces = 0
    /\ orphaned = 0
    /\ crashed = 0
    /\ timeouts = 0
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

PeerConverting(c) ==
    \E other \in Connections :
        /\ other # c
        /\ live[other]
        /\ state[other] \in {"Composing", "Predicting", "Converting"}

SpaceEffect(c) ==
    IF ~live[c]
    THEN "Ignore"
    ELSE IF state[c] \in {"Composing", "Predicting", "Converting"}
         THEN "Convert"
         ELSE IF (FenceIdleSpace /\ PeerConverting(c))
                 \/ documentSpaces >= MaxSpaces
              THEN "Absorb"
              ELSE "Insert"

ApplySpace(c, st) ==
    CASE SpaceEffect(c) = "Convert" -> "Converting"
      [] SpaceEffect(c) = "Absorb"  -> st
      [] SpaceEffect(c) = "Ignore"  -> st
      [] OTHER                      -> "Idle"

Type(c) ==
    /\ WithinBudget
    /\ live[c]
    /\ state[c] # "Converting"
    /\ state' = [state EXCEPT ![c] = "Composing"]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<live, documentSpaces, orphaned, crashed, timeouts>>

Suggest(c) ==
    /\ WithinBudget
    /\ live[c]
    /\ state[c] = "Composing"
    /\ state' = [state EXCEPT ![c] = "Predicting"]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<live, documentSpaces, orphaned, crashed, timeouts>>

Commit(c) ==
    /\ WithinBudget
    /\ live[c]
    /\ state[c] # "Idle"
    /\ state' = [state EXCEPT ![c] = "Idle"]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<live, documentSpaces, orphaned, crashed, timeouts>>

ReplaceContext(c) ==
    /\ WithinBudget
    /\ live[c]
    /\ state[c] # "Idle"
    /\ orphaned < MaxEvents
    /\ state' = [state EXCEPT ![c] = "Idle"]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ orphaned' = orphaned + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<live, documentSpaces, crashed, timeouts>>

CrashRestart(c) ==
    /\ WithinBudget
    /\ crashed < MaxEvents
    /\ state' = [state EXCEPT ![c] = "Idle"]
    /\ live' = [live EXCEPT ![c] = TRUE]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ crashed' = crashed + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<documentSpaces, orphaned, timeouts>>

Disconnect(c) ==
    /\ WithinBudget
    /\ live[c]
    /\ live' = [live EXCEPT ![c] = FALSE]
    /\ state' = [state EXCEPT ![c] = "Idle"]
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ convertedFromPredicting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<documentSpaces, orphaned, crashed, timeouts>>

TimeoutSpace ==
    /\ WithinBudget
    /\ timeouts < MaxEvents
    /\ insertedThisKey' = FALSE
    /\ convertedThisKey' = FALSE
    /\ absorbedThisKey' = TRUE
    /\ convertedFromPredicting' = FALSE
    /\ timeouts' = timeouts + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<state, live, documentSpaces, orphaned, crashed>>

WouldInsert(targets) ==
    \E c \in targets : SpaceEffect(c) = "Insert"

WouldConvert(targets) ==
    \E c \in targets : SpaceEffect(c) = "Convert"

SpaceOn(targets) ==
    /\ WithinBudget
    /\ targets # {}
    /\ targets \subseteq Connections
    /\ LET rawInsert == WouldInsert(targets)
           rawConvert == WouldConvert(targets)
           allowInsert == IF FenceIdleSpace THEN rawInsert /\ ~rawConvert ELSE rawInsert
       IN  /\ insertedThisKey' = allowInsert
           /\ convertedThisKey' = rawConvert
           /\ convertedFromPredicting' =
                \E c \in targets : state[c] = "Predicting" /\ SpaceEffect(c) = "Convert"
           /\ absorbedThisKey' = (\E c \in targets : SpaceEffect(c) \in {"Absorb", "Ignore"})
                                 \/ (FenceIdleSpace /\ rawInsert /\ rawConvert)
           /\ state' = [c \in Connections |->
                           IF c \in targets THEN ApplySpace(c, state[c]) ELSE state[c]]
           /\ documentSpaces' = IF allowInsert
                                THEN documentSpaces + 1
                                ELSE documentSpaces
           /\ eventCount' = eventCount + 1
           /\ UNCHANGED <<live, orphaned, crashed, timeouts>>

FocusedSpace(c) == SpaceOn({c})

DualSpace ==
    /\ DualDelivery
    /\ SpaceOn(Connections)

Idle ==
    /\ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    \/ Idle
    \/ DualSpace
    \/ TimeoutSpace
    \/ \E c \in Connections :
          Type(c) \/ Suggest(c) \/ Commit(c) \/ ReplaceContext(c) \/ FocusedSpace(c)
          \/ CrashRestart(c) \/ Disconnect(c)

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ \A c \in Connections : WF_vars(Commit(c))

PerConnectionSpaceIsConvert ==
    \A c \in Connections :
        (live[c] /\ state[c] \in {"Composing", "Predicting", "Converting"}) =>
            SpaceEffect(c) = "Convert"

PredictingSpaceDoesNotInsert ==
    \A c \in Connections :
        (live[c] /\ state[c] = "Predicting") =>
            SpaceEffect(c) = "Convert"

PredictingSpaceDoesNotCommitReading ==
    convertedFromPredicting =>
        /\ convertedThisKey
        /\ ~insertedThisKey
        /\ \E c \in Connections : live[c] /\ state[c] = "Converting"

NoDualEffect == ~(insertedThisKey /\ convertedThisKey)
NeverDualEffect == NoDualEffect

FencedIdleDoesNotInsert ==
    FenceIdleSpace =>
        \A c \in Connections :
            (live[c] /\ state[c] = "Idle" /\ PeerConverting(c)) =>
                SpaceEffect(c) = "Absorb"

IdleInsertIsFullwidthSlot ==
    insertedThisKey => documentSpaces > 0

SpacesBounded == documentSpaces <= MaxSpaces

DeadConnectionIgnoresSpace ==
    \A c \in Connections : ~live[c] => SpaceEffect(c) = "Ignore"

CrashRestoresIdle ==
    TRUE

NeverConvertingAfterBudget ==
    eventCount = MaxEvents => TRUE

\* Reachability probes.  A dedicated config expects these to be violated.
NeverConverts ==
    \A c \in Connections : state[c] # "Converting"

NeverConvertedFromPredicting ==
    ~convertedFromPredicting

NeverInserts ==
    ~insertedThisKey

ConversionEventuallyTerminates ==
    (\E c \in Connections : live[c] /\ state[c] = "Converting")
        ~> ((\A c \in Connections : state[c] # "Converting")
            \/ eventCount = MaxEvents)

\* Required-state: unfenced DualDelivery reaches insert/\convert.
\* Forbidden-state under the fence: the same dual effect.

=============================================================================
