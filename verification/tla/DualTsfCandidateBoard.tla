---- MODULE DualTsfCandidateBoard ----
EXTENDS Naturals, TLC

(***************************************************************************
Independent behavioral model of the shared candidate popup while Space
is delivered to two TSF contexts.

This is not a transcription of sakura-engine or sakura-tsf control flow.
It models the user-visible display contract after 1.0.14 already converts:

  * One physical Space can reach a live reading and an idle peer.
  * Suggestion and conversion lists share one engine board (owner, kind).
  * TSF candidate UI is a separate writer: output.candidates = None is
    CandidateEffect::Hide. End is allowed only when the instance owns the
    UiLease or still has a live reading (`ends_shared_candidate_ui`).
  * Protocol SessionId restarts at 1 per pipe worker. Board ownership is
    (connection, session).

IgnoreForeignEmpty
  Engine UiBoard: empty publish_output from a session that does not own
    the board is a no-op, including two connections that share session id 1.

GuardForeignCandidateEnd
  TSF Hide/End from a foreign or absorbed peer does not terminate the
  live CandidateUi / UiLease. Implemented by requiring owns_ui or
  local_live, and refusing End when peer_live && ~local_live.

RestoreCurrentPlacement
  After convert, republish the current live placement when a live
  UiLease is still authority, CandidateUi is active, the host allows an
  external renderer, and the callback is not stale.
  This is not `renderer_visible = true` inside publish_output.

Environment
  * Two contexts, one shared board, one TSF candidate UI, one user,
    totally ordered events.
  * Suggestions appear before Space. Compact conversion without a prior
    suggestion list is out of scope.
  * Logical time is eventCount. There is no wall clock.

Unexplored
  * Two TextService instances that do not share CandidateUi
  * Dictionary ranking, COM re-entrancy, delayed SetUiPlacement after
    layout lease rollover, more than two contexts, Chromium confirming
    a reading after an eaten Space, host Show(false)
***************************************************************************)

CONSTANTS
    IgnoreForeignEmpty,
    GuardForeignCandidateEnd,
    RestoreCurrentPlacement,
    MaxEvents

ASSUME /\ IgnoreForeignEmpty \in BOOLEAN
       /\ GuardForeignCandidateEnd \in BOOLEAN
       /\ RestoreCurrentPlacement \in BOOLEAN
       /\ MaxEvents \in Nat \ {0}

VARIABLES livePhase, kind, owner, visible, converted, peerHit, eventCount

vars == <<livePhase, kind, owner, visible, converted, peerHit, eventCount>>

Phases == {"Idle", "Typed", "Suggested", "Converted"}
Kinds == {"None", "Suggestion", "Conversion"}
Owners == {"None", "Live"}

TypeOK ==
    /\ livePhase \in Phases
    /\ kind \in Kinds
    /\ owner \in Owners
    /\ visible \in BOOLEAN
    /\ converted \in BOOLEAN
    /\ peerHit \in BOOLEAN
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ livePhase = "Idle"
    /\ kind = "None"
    /\ owner = "None"
    /\ visible = FALSE
    /\ converted = FALSE
    /\ peerHit = FALSE
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

TypeReading ==
    /\ WithinBudget
    /\ livePhase = "Idle"
    /\ livePhase' = "Typed"
    /\ kind' = "None"
    /\ owner' = "None"
    /\ visible' = FALSE
    /\ converted' = FALSE
    /\ peerHit' = FALSE
    /\ eventCount' = eventCount + 1

ShowSuggestions ==
    /\ WithinBudget
    /\ livePhase = "Typed"
    /\ livePhase' = "Suggested"
    /\ kind' = "Suggestion"
    /\ owner' = "Live"
    /\ visible' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<converted, peerHit>>

\* Idle peer published a candidate-free engine output onto the shared board.
PeerSpaceEmpty ==
    /\ WithinBudget
    /\ livePhase \in {"Suggested", "Converted"}
    /\ peerHit' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<livePhase, converted>>
    /\ IF IgnoreForeignEmpty /\ owner = "Live"
       THEN UNCHANGED <<kind, owner, visible>>
       ELSE /\ kind' = "None"
            /\ owner' = "None"
            /\ visible' = FALSE

\* Idle peer's TSF mapped candidates=None to CandidateEffect::Hide.
\* Engine board ownership is unchanged; the candidate UI is ended.
PeerCandidateEnd ==
    /\ WithinBudget
    /\ livePhase \in {"Suggested", "Converted"}
    /\ owner = "Live"
    /\ kind # "None"
    /\ peerHit' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<livePhase, converted, kind, owner>>
    /\ IF GuardForeignCandidateEnd
       THEN UNCHANGED visible
       ELSE visible' = FALSE

\* Live reading received Space. The engine converts. Placement is reused
\* only when RestoreCurrentPlacement is set, or when the UI is still shown.
LiveSpaceConvert ==
    /\ WithinBudget
    /\ livePhase = "Suggested"
    /\ livePhase' = "Converted"
    /\ converted' = TRUE
    /\ kind' = "Conversion"
    /\ owner' = "Live"
    /\ visible' = (RestoreCurrentPlacement \/ visible)
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED peerHit

Done ==
    /\ livePhase = "Converted" \/ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    TypeReading \/ ShowSuggestions \/ PeerSpaceEmpty
    \/ PeerCandidateEnd \/ LiveSpaceConvert \/ Done

Spec == Init /\ [][Next]_vars

SuggestedListUntilConvert ==
    livePhase = "Suggested" => (kind = "Suggestion" /\ visible /\ owner = "Live")

ConvertedListIsVisible ==
    livePhase = "Converted" => (kind = "Conversion" /\ visible /\ owner = "Live")

LiveOwnedListIsVisible ==
    (owner = "Live" /\ kind # "None") => visible

NoForeignUiMutation ==
    GuardForeignCandidateEnd =>
        (peerHit /\ owner = "Live" /\ kind # "None" => visible)

PeerEmptyDoesNotHideLive ==
    IgnoreForeignEmpty =>
        (peerHit /\ owner = "Live" => (kind # "None" /\ owner = "Live"))

ProposedFixHolds ==
    (IgnoreForeignEmpty /\ GuardForeignCandidateEnd) =>
        (SuggestedListUntilConvert /\ ConvertedListIsVisible /\ LiveOwnedListIsVisible)

\* Reachability / negative configs.
NeverPeerClearsSuggestedList ==
    ~(peerHit /\ livePhase = "Suggested" /\ kind = "None")

NeverForeignEndHidesLive ==
    ~(peerHit /\ owner = "Live" /\ kind # "None" /\ ~visible)

NeverInvisibleConversion ==
    ~(converted /\ (~visible \/ kind # "Conversion"))

=============================================================================
