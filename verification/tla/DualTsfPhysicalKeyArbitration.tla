---- MODULE DualTsfPhysicalKeyArbitration ----
EXTENDS Naturals, TLC

(***************************************************************************
Independent behavioral model of Dual TSF physical conversion-key
arbitration. This is not a transcription of sakura-tsf control flow.

One physical Space can be delivered to two TextService instances in the
same process. A live reading on A must not let idle B return that Space
to Chromium.

ThreeState = FALSE
  PhysicalKeyOwner is two-valued: local live composition => ApplyLocal,
  otherwise HostEligible. Idle B probes and, on Probe failure, is uneaten.

ThreeState = TRUE
  ConversionKeyDisposition is HostEligible / ApplyLocal / AbsorbPeer.
  AbsorbPeer eats without engine, UI, or document writes.

Claim tokens are numeric (instance + generation). No COM objects.

Ignored
  Dictionary contents, candidate strings, renderer coordinates, 50 ms,
  HRESULT subdivisions, CandidateEffect Keep/Show/End.
***************************************************************************)

CONSTANTS
    ThreeState,
    PeerFirst,
    OwnerFirst,
    ProbeCanFail,
    MaxEvents,
    MaxGen

ASSUME /\ ThreeState \in BOOLEAN
       /\ PeerFirst \in BOOLEAN
       /\ OwnerFirst \in BOOLEAN
       /\ ~(PeerFirst /\ OwnerFirst)
       /\ ProbeCanFail \in BOOLEAN
       /\ MaxEvents \in Nat \ {0}
       /\ MaxGen \in Nat \ {0}

Actors == {"A", "B"}

VARIABLES
    liveOwner,
    localComposition,
    ownerAlive,
    claimGen,
    liveClaimGen,
    uiOwner,
    hostInsertedThisKey,
    engineAppliedThisKey,
    absorbedThisKey,
    appliedBy,
    eventCount

vars == <<liveOwner, localComposition, ownerAlive, claimGen, liveClaimGen,
          uiOwner, hostInsertedThisKey, engineAppliedThisKey, absorbedThisKey,
          appliedBy, eventCount>>

TypeOK ==
    /\ liveOwner \in {"None"} \cup Actors
    /\ localComposition \in [Actors -> BOOLEAN]
    /\ ownerAlive \in [Actors -> BOOLEAN]
    /\ claimGen \in [Actors -> 1..MaxGen]
    /\ liveClaimGen \in 0..MaxGen
    /\ uiOwner \in {"None"} \cup Actors
    /\ hostInsertedThisKey \in BOOLEAN
    /\ engineAppliedThisKey \in BOOLEAN
    /\ absorbedThisKey \in BOOLEAN
    /\ appliedBy \in {"None"} \cup Actors
    /\ eventCount \in 0..MaxEvents

Init ==
    /\ liveOwner = "None"
    /\ localComposition = [a \in Actors |-> FALSE]
    /\ ownerAlive = [a \in Actors |-> TRUE]
    /\ claimGen = [a \in Actors |-> 1]
    /\ liveClaimGen = 0
    /\ uiOwner = "None"
    /\ hostInsertedThisKey = FALSE
    /\ engineAppliedThisKey = FALSE
    /\ absorbedThisKey = FALSE
    /\ appliedBy = "None"
    /\ eventCount = 0

WithinBudget == eventCount < MaxEvents

Disposition(actor) ==
    IF localComposition[actor]
    THEN "ApplyLocal"
    ELSE IF ThreeState /\ liveOwner # "None" /\ liveOwner # actor
         THEN "AbsorbPeer"
         ELSE "HostEligible"

Eaten(disposition) ==
    CASE disposition = "ApplyLocal"  -> TRUE
      [] disposition = "AbsorbPeer"  -> TRUE
      [] OTHER                      -> ~ProbeCanFail

ClearKey ==
    /\ hostInsertedThisKey' = FALSE
    /\ engineAppliedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ appliedBy' = "None"

TypeA ==
    /\ WithinBudget
    /\ ownerAlive["A"]
    /\ liveOwner = "None"
    /\ ~localComposition["A"]
    /\ liveOwner' = "A"
    /\ localComposition' = [localComposition EXCEPT !["A"] = TRUE]
    /\ liveClaimGen' = claimGen["A"]
    /\ uiOwner' = "A"
    /\ eventCount' = eventCount + 1
    /\ ClearKey
    /\ UNCHANGED <<ownerAlive, claimGen>>

CanDeliver(first, second) ==
    /\ first # second
    /\ {first, second} = Actors
    /\ IF PeerFirst THEN first = "B"
       ELSE IF OwnerFirst THEN first = "A"
       ELSE TRUE

DeliverSpace(first, second) ==
    /\ WithinBudget
    /\ CanDeliver(first, second)
    /\ ownerAlive[first] /\ ownerAlive[second]
    /\ LET d1 == Disposition(first)
           d2 == Disposition(second)
           e1 == Eaten(d1)
           e2 == Eaten(d2)
       IN  /\ hostInsertedThisKey' = (~e1 \/ ~e2)
           /\ engineAppliedThisKey' = (d1 = "ApplyLocal" \/ d2 = "ApplyLocal")
           /\ absorbedThisKey' = (d1 = "AbsorbPeer" \/ d2 = "AbsorbPeer")
           /\ appliedBy' = IF d1 = "ApplyLocal" THEN first
                           ELSE IF d2 = "ApplyLocal" THEN second
                           ELSE "None"
           /\ eventCount' = eventCount + 1
           /\ UNCHANGED <<liveOwner, localComposition, ownerAlive, claimGen,
                         liveClaimGen, uiOwner>>

SpaceAB == DeliverSpace("A", "B")
SpaceBA == DeliverSpace("B", "A")

\* After owner teardown, a remaining idle instance must be able to
\* return Space to the host. Dual delivery is not required.
IdleHostSpace ==
    /\ WithinBudget
    /\ liveOwner = "None"
    /\ \E a \in Actors : ownerAlive[a]
    /\ hostInsertedThisKey' = ~Eaten("HostEligible")
    /\ engineAppliedThisKey' = FALSE
    /\ absorbedThisKey' = FALSE
    /\ appliedBy' = "None"
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<liveOwner, localComposition, ownerAlive, claimGen,
                  liveClaimGen, uiOwner>>

Teardown(actor) ==
    /\ WithinBudget
    /\ ownerAlive[actor]
    /\ ownerAlive' = [ownerAlive EXCEPT ![actor] = FALSE]
    /\ localComposition' = [localComposition EXCEPT ![actor] = FALSE]
    /\ IF liveOwner = actor
       THEN /\ liveOwner' = "None"
            /\ liveClaimGen' = 0
       ELSE UNCHANGED <<liveOwner, liveClaimGen>>
    /\ IF uiOwner = actor THEN uiOwner' = "None" ELSE UNCHANGED uiOwner
    /\ IF claimGen[actor] < MaxGen
       THEN claimGen' = [claimGen EXCEPT ![actor] = claimGen[actor] + 1]
       ELSE UNCHANGED claimGen
    /\ eventCount' = eventCount + 1
    /\ ClearKey

ReplaceContextA ==
    /\ WithinBudget
    /\ ownerAlive["A"]
    /\ liveOwner = "A"
    /\ claimGen["A"] < MaxGen
    /\ claimGen' = [claimGen EXCEPT !["A"] = claimGen["A"] + 1]
    /\ liveClaimGen' = claimGen["A"] + 1
    /\ eventCount' = eventCount + 1
    /\ ClearKey
    /\ UNCHANGED <<liveOwner, localComposition, ownerAlive, uiOwner>>

\* An old generation token must not drop a newer live claim.
StaleReleaseA ==
    /\ WithinBudget
    /\ liveOwner = "A"
    /\ ownerAlive["A"]
    /\ liveClaimGen = claimGen["A"]
    /\ claimGen["A"] > 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<liveOwner, localComposition, ownerAlive, claimGen,
                  liveClaimGen, uiOwner, hostInsertedThisKey,
                  engineAppliedThisKey, absorbedThisKey, appliedBy>>

Done ==
    /\ (eventCount = MaxEvents \/ \A a \in Actors : ~ownerAlive[a])
    /\ UNCHANGED vars

Next ==
    TypeA \/ SpaceAB \/ SpaceBA \/ IdleHostSpace
    \/ Teardown("A") \/ Teardown("B")
    \/ ReplaceContextA \/ StaleReleaseA \/ Done

Spec == Init /\ [][Next]_vars

NoHostInsertWhileLive ==
    liveOwner # "None" => ~hostInsertedThisKey

OnlyOwnerApplies ==
    engineAppliedThisKey => (appliedBy = liveOwner /\ appliedBy # "None")

AtMostOneEffectPerPhysicalKey ==
    ~(hostInsertedThisKey /\ engineAppliedThisKey)

ForeignPeerDoesNotEndOwnerUi ==
    liveOwner # "None" => uiOwner = liveOwner

LiveOwnerIsAlive ==
    liveOwner \in Actors => ownerAlive[liveOwner]

MatchingClaim ==
    /\ (liveOwner = "A" => liveClaimGen = claimGen["A"])
    /\ (liveOwner = "B" => liveClaimGen = claimGen["B"])
    /\ (liveOwner = "None" => liveClaimGen = 0)

NoAbsorbWithoutLiveOwner ==
    \A a \in Actors :
        Disposition(a) = "AbsorbPeer" => liveOwner \in Actors \ {a}

Safety ==
    /\ NoHostInsertWhileLive
    /\ OnlyOwnerApplies
    /\ AtMostOneEffectPerPhysicalKey
    /\ ForeignPeerDoesNotEndOwnerUi
    /\ LiveOwnerIsAlive
    /\ MatchingClaim
    /\ NoAbsorbWithoutLiveOwner

\* Reachability / negative configs.
NeverHostInserts == ~hostInsertedThisKey

=============================================================================
