---- MODULE TsfPredictingSpace ----
EXTENDS Naturals, TLC

(***************************************************************************
Independent behavioral model of Space while prediction suggestions are
visible.

This is not a transcription of sakura-tsf control flow. It models the
user-visible contract after 1.0.12 already claims OnTestKeyDown:

  * A live reading plus visible suggestions still owns Space.
  * OnTestKeyDown reports eaten, so the host must not insert a document
    space.
  * OnKeyDown must convert. Chromium may confirm the underlined reading
    if Space is eaten and the composition string does not change.
  * RetargetLive = TRUE is the product fix: KeyDown still converts when
    the callback ITfContext is not the reading's document, because a
    stored live context (composition, suggestion layout, or queued
    candidate payload) remains.
  * RetargetLive = FALSE is the defect: KeyDown absorbs without asking
    the engine, and the host confirms the reading (e.g. よそく).

Environment
  * One user, one composition, totally ordered keystrokes.
  * Suggestions may appear before Space.
  * Logical time is eventCount. There is no wall clock.

Unexplored
  * Dual TSF delivery, dictionary ranking (低地 vs ていち), COM
    re-entrancy, idle fullwidth Space, and Ctrl+Space.
***************************************************************************)

CONSTANTS RetargetLive, MaxEvents

ASSUME /\ RetargetLive \in BOOLEAN
       /\ MaxEvents \in Nat \ {0}

VARIABLES phase, suggestionsVisible, testEaten, converted,
          hostConfirmedReading, eventCount

vars == <<phase, suggestionsVisible, testEaten, converted,
          hostConfirmedReading, eventCount>>

Phases == {"Idle", "Typed", "Suggested", "Tested", "Absorbed", "Converted", "Confirmed"}

TypeOK ==
    /\ phase \in Phases
    /\ suggestionsVisible \in BOOLEAN
    /\ testEaten \in BOOLEAN
    /\ converted \in BOOLEAN
    /\ hostConfirmedReading \in BOOLEAN
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ phase = "Idle"
    /\ suggestionsVisible = FALSE
    /\ testEaten = FALSE
    /\ converted = FALSE
    /\ hostConfirmedReading = FALSE
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

TypeReading ==
    /\ WithinBudget
    /\ phase = "Idle"
    /\ phase' = "Typed"
    /\ suggestionsVisible' = FALSE
    /\ testEaten' = FALSE
    /\ converted' = FALSE
    /\ hostConfirmedReading' = FALSE
    /\ eventCount' = eventCount + 1

ShowSuggestions ==
    /\ WithinBudget
    /\ phase = "Typed"
    /\ phase' = "Suggested"
    /\ suggestionsVisible' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<testEaten, converted, hostConfirmedReading>>

TestKeyDown ==
    /\ WithinBudget
    /\ phase = "Suggested"
    /\ suggestionsVisible
    /\ testEaten' = TRUE
    /\ phase' = "Tested"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<suggestionsVisible, converted, hostConfirmedReading>>

KeyDownConvert ==
    /\ WithinBudget
    /\ phase = "Tested"
    /\ testEaten
    /\ RetargetLive
    /\ converted' = TRUE
    /\ suggestionsVisible' = FALSE
    /\ phase' = "Converted"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<testEaten, hostConfirmedReading>>

KeyDownAbsorb ==
    /\ WithinBudget
    /\ phase = "Tested"
    /\ testEaten
    /\ ~RetargetLive
    /\ converted' = FALSE
    /\ phase' = "Absorbed"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<suggestionsVisible, testEaten, hostConfirmedReading>>

HostConfirmReading ==
    /\ WithinBudget
    /\ phase = "Absorbed"
    /\ testEaten
    /\ ~converted
    /\ ~RetargetLive
    /\ hostConfirmedReading' = TRUE
    /\ suggestionsVisible' = FALSE
    /\ phase' = "Confirmed"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<testEaten, converted>>

Done ==
    /\ phase \in {"Converted", "Confirmed"} \/ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    TypeReading \/ ShowSuggestions \/ TestKeyDown \/ KeyDownConvert
    \/ KeyDownAbsorb \/ HostConfirmReading \/ Done

Spec == Init /\ [][Next]_vars

NoHostConfirmAfterConvert ==
    converted => ~hostConfirmedReading

NoHostConfirmWhileSuggestionsVisible ==
    suggestionsVisible => ~hostConfirmedReading

EatenSpaceConvertsWhenRetargeted ==
    RetargetLive => ~hostConfirmedReading

SuggestionsSpaceDoesNotConfirmReading ==
    (phase = "Converted" \/ phase = "Confirmed") =>
        (converted /\ ~hostConfirmedReading)

\* Reachability: a dedicated bug config expects this to be violated.
NeverHostConfirmsReading == ~hostConfirmedReading

NeverConverts == ~converted

=============================================================================
