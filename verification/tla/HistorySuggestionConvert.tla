---- MODULE HistorySuggestionConvert ----
EXTENDS Naturals, TLC

(***************************************************************************
Independent behavioral model of Space while a 履歴 (learning-history)
suggestion list is visible.

This is not a transcription of sakura-engine or sakura-tsf. It models the
user-visible contract the developer log showed for にほんご:

  * Typing can show a 履歴 list, including an identity pair
    (reading = surface, e.g. にほんご → にほんご).
  * Space must convert with the dictionary surface (日本語), keep a
    conversion list visible, and must not confirm the underlined reading.
  * An idle Dual TSF peer must not Hide that list.

PreferIdentity = TRUE
  Conversion prefers the identity 履歴 surface. The composition string
  does not change, so the host confirms the reading.

PreferIdentity = FALSE
  Identity 履歴 is ignored for conversion ranking. Space converts to the
  dictionary surface and shows conversion candidates.

GuardForeignEnd = FALSE
  Idle peer Hide ends the live list (shipped Dual TSF Hide).

GuardForeignEnd = TRUE
  Idle peer Hide is ignored while a sibling owns the reading.

Unexplored
  Dictionary costs beyond identity vs dictionary, COM re-entrancy,
  Ctrl+Space, idle fullwidth Space without a reading.
***************************************************************************)

CONSTANTS PreferIdentity, GuardForeignEnd, MaxEvents

ASSUME /\ PreferIdentity \in BOOLEAN
       /\ GuardForeignEnd \in BOOLEAN
       /\ MaxEvents \in Nat \ {0}

VARIABLES
    phase,
    historyVisible,
    conversionVisible,
    converted,
    topIdentity,
    hostConfirmed,
    peerHit,
    eventCount

vars == <<phase, historyVisible, conversionVisible, converted,
          topIdentity, hostConfirmed, peerHit, eventCount>>

Phases == {"Idle", "Typed", "Suggested", "Converted", "Confirmed"}

TypeOK ==
    /\ phase \in Phases
    /\ historyVisible \in BOOLEAN
    /\ conversionVisible \in BOOLEAN
    /\ converted \in BOOLEAN
    /\ topIdentity \in BOOLEAN
    /\ hostConfirmed \in BOOLEAN
    /\ peerHit \in BOOLEAN
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ phase = "Idle"
    /\ historyVisible = FALSE
    /\ conversionVisible = FALSE
    /\ converted = FALSE
    /\ topIdentity = FALSE
    /\ hostConfirmed = FALSE
    /\ peerHit = FALSE
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

TypeReading ==
    /\ WithinBudget
    /\ phase = "Idle"
    /\ phase' = "Typed"
    /\ historyVisible' = FALSE
    /\ conversionVisible' = FALSE
    /\ converted' = FALSE
    /\ topIdentity' = FALSE
    /\ hostConfirmed' = FALSE
    /\ peerHit' = FALSE
    /\ eventCount' = eventCount + 1

ShowHistory ==
    /\ WithinBudget
    /\ phase = "Typed"
    /\ phase' = "Suggested"
    /\ historyVisible' = TRUE
    /\ conversionVisible' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<converted, topIdentity, hostConfirmed, peerHit>>

PeerHide ==
    /\ WithinBudget
    /\ phase \in {"Suggested", "Converted"}
    /\ peerHit' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<phase, converted, topIdentity, hostConfirmed>>
    /\ IF GuardForeignEnd
       THEN UNCHANGED <<historyVisible, conversionVisible>>
       ELSE /\ historyVisible' = FALSE
            /\ conversionVisible' = FALSE

SpaceConvert ==
    /\ WithinBudget
    /\ phase = "Suggested"
    /\ historyVisible
    /\ converted' = TRUE
    /\ topIdentity' = PreferIdentity
    /\ historyVisible' = FALSE
    /\ conversionVisible' = ~PreferIdentity
    /\ phase' = "Converted"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<hostConfirmed, peerHit>>

HostConfirm ==
    /\ WithinBudget
    /\ phase = "Converted"
    /\ converted
    /\ topIdentity
    /\ PreferIdentity
    /\ hostConfirmed' = TRUE
    /\ conversionVisible' = FALSE
    /\ historyVisible' = FALSE
    /\ phase' = "Confirmed"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<converted, topIdentity, peerHit>>

Done ==
    /\ phase \in {"Converted", "Confirmed"} \/ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    TypeReading \/ ShowHistory \/ PeerHide \/ SpaceConvert \/ HostConfirm \/ Done

Spec == Init /\ [][Next]_vars

HistoryListUntilConvert ==
    phase = "Suggested" => (historyVisible /\ ~conversionVisible /\ ~hostConfirmed)

ConvertedDictionaryIsVisible ==
    (phase = "Converted" /\ ~PreferIdentity) =>
        (converted /\ conversionVisible /\ ~topIdentity /\ ~hostConfirmed)

NoHostConfirmWithoutIdentity ==
    ~PreferIdentity => ~hostConfirmed

IdentityDoesNotConfirmWhenGuarded ==
    (~PreferIdentity /\ GuardForeignEnd) =>
        (phase = "Converted" => (conversionVisible \/ ~converted))

PeerCannotHideLiveList ==
    GuardForeignEnd =>
        (peerHit /\ phase = "Suggested" => historyVisible)

Safety ==
    /\ HistoryListUntilConvert
    /\ ConvertedDictionaryIsVisible
    /\ NoHostConfirmWithoutIdentity
    /\ PeerCannotHideLiveList

\* Reachability / negative configs.
NeverHostConfirms == ~hostConfirmed
NeverIdentityTop == ~topIdentity
NeverInvisibleConversion == ~(converted /\ ~PreferIdentity /\ ~conversionVisible)

=============================================================================
