--------------------------- MODULE WriteCoordinator ---------------------------
(***************************************************************************)
(* Abstract protocol model of crates/sakura-tsf/src/write_coordinator.rs  *)
(* at revision 0e766fd (sakura-input 1.0.3).                               *)
(*                                                                         *)
(* Modeled: activation/focus generations, context identity,                *)
(* committed/tail revisions, the bounded operation journal                 *)
(* (Reserved -> Ready -> Requested), ticket issuance/validation,           *)
(* UI lease issuance/adoption, and the epoch events that must invalidate   *)
(* outstanding tickets and leases.                                         *)
(*                                                                         *)
(* Abstracted away (checked instead by the Rust bounded checker against    *)
(* the verbatim implementation copy): text projections and the            *)
(* ProjectionMismatch check, cancel reasons, capacity>2 shapes,           *)
(* u64 wrapping arithmetic (naturals here; the impl only compares for      *)
(* equality), and lease supersession at equal revision (finding F4).      *)
(*                                                                         *)
(* Deadness is a trace-layer oracle: any epoch event (activate,            *)
(* deactivate, focus change, context replacement, revision bump) marks    *)
(* every outstanding ticket/lease dead, as does terminalization of the     *)
(* ticket's own operation.  Safety then demands that a dead ticket never   *)
(* validates and a dead lease never adopts.                                *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS Contexts, Cap, MaxRev, MaxGen, MaxId

NoCtx == 0

VARIABLES
  active,   \* journal accepts work
  act,      \* activation generation
  foc,      \* focus generation
  ctx,      \* observed context identity (NoCtx = none)
  crev,     \* committed document revision
  trev,     \* tail (speculative) revision
  ops,      \* journal: sequence of operation records
  nextId,   \* operation id allocator
  adopted,  \* currently adopted UI lease id (0 = none)
  tickets,  \* every ticket ever issued, with trace-layer dead flag
  leases,   \* every UI lease ever issued, with trace-layer dead flag
  alarm     \* TRUE once a dead lease has been adopted (safety violation)

vars == <<active, act, foc, ctx, crev, trev, ops, nextId, adopted, tickets, leases, alarm>>

HasRes == \E i \in 1..Len(ops) : ops[i].ph = "res"
ResIdx == CHOOSE i \in 1..Len(ops) : ops[i].ph = "res"

KillTickets == {[t EXCEPT !.dead = TRUE] : t \in tickets}
KillLeases  == {[l EXCEPT !.dead = TRUE] : l \in leases}

TypeOK ==
  /\ active \in BOOLEAN
  /\ ctx \in {NoCtx} \cup Contexts
  /\ adopted \in 0..MaxId
  /\ \A i \in 1..Len(ops) : ops[i].ph \in {"res", "rdy", "req"}

Init ==
  /\ active = FALSE /\ act = 0 /\ foc = 0 /\ ctx = NoCtx
  /\ crev = 0 /\ trev = 0 /\ ops = <<>> /\ nextId = 1
  /\ adopted = 0 /\ tickets = {} /\ leases = {} /\ alarm = FALSE

(* --- lifecycle / epoch events ------------------------------------------ *)

Activate ==
  /\ act < MaxGen /\ foc < MaxGen
  /\ active' = TRUE /\ act' = act + 1 /\ foc' = foc + 1 /\ ctx' = NoCtx
  /\ crev' = 0 /\ trev' = 0 /\ ops' = <<>> /\ adopted' = 0
  /\ tickets' = KillTickets /\ leases' = KillLeases
  /\ UNCHANGED <<nextId, alarm>>

Deactivate ==
  /\ active /\ act < MaxGen /\ foc < MaxGen
  /\ active' = FALSE /\ act' = act + 1 /\ foc' = foc + 1 /\ ctx' = NoCtx
  /\ trev' = crev /\ ops' = <<>> /\ adopted' = 0
  /\ tickets' = KillTickets /\ leases' = KillLeases
  /\ UNCHANGED <<crev, nextId, alarm>>

FocusChanged ==
  /\ foc < MaxGen
  /\ foc' = foc + 1 /\ trev' = crev /\ ops' = <<>> /\ adopted' = 0
  /\ tickets' = KillTickets /\ leases' = KillLeases
  /\ UNCHANGED <<active, act, ctx, crev, nextId, alarm>>

(* composition_terminated / document_changed / abandon_projection:
   commit an out-of-band revision bump and drain the journal. *)
RevisionBump ==
  /\ crev < MaxRev
  /\ crev' = crev + 1 /\ trev' = crev + 1 /\ ops' = <<>> /\ adopted' = 0
  /\ tickets' = KillTickets /\ leases' = KillLeases
  /\ UNCHANGED <<active, act, foc, ctx, nextId, alarm>>

ObserveNew ==
  /\ ctx = NoCtx
  /\ \E c \in Contexts :
       ctx' = c
  /\ UNCHANGED <<active, act, foc, crev, trev, ops, nextId, adopted, tickets, leases, alarm>>

(* Context replacement: note that the implementation resets the committed  *)
(* revision to 0 but does NOT advance the activation or focus generation.  *)
ObserveReplace ==
  /\ ctx # NoCtx
  /\ \E c \in Contexts :
       /\ c # ctx
       /\ ctx' = c
  /\ crev' = 0 /\ trev' = 0 /\ ops' = <<>> /\ adopted' = 0
  /\ tickets' = KillTickets /\ leases' = KillLeases
  /\ UNCHANGED <<active, act, foc, nextId, alarm>>

(* --- journal operations ------------------------------------------------- *)

Reserve ==
  /\ active /\ ctx # NoCtx /\ Len(ops) < Cap /\ ~HasRes /\ nextId <= MaxId
  /\ ops' = Append(ops, [id |-> nextId, ph |-> "res", a |-> act, f |-> foc,
                         c |-> ctx, b |-> trev, r |-> trev, w |-> FALSE])
  /\ nextId' = nextId + 1
  /\ UNCHANGED <<active, act, foc, ctx, crev, trev, adopted, tickets, leases, alarm>>

Attach(w) ==
  /\ HasRes
  /\ (w = TRUE) => (trev < MaxRev)
  /\ LET i == ResIdx
         r == IF w THEN trev + 1 ELSE trev
     IN /\ ops' = [ops EXCEPT ![i].ph = "rdy", ![i].b = trev, ![i].r = r, ![i].w = w]
        /\ trev' = r
  /\ UNCHANGED <<active, act, foc, ctx, crev, nextId, adopted, tickets, leases, alarm>>

CancelReservation ==
  /\ HasRes
  /\ LET i == ResIdx
     IN ops' = SubSeq(ops, 1, i - 1) \o SubSeq(ops, i + 1, Len(ops))
  /\ UNCHANGED <<active, act, foc, ctx, crev, trev, nextId, adopted, tickets, leases, alarm>>

BeginHead ==
  /\ ops # <<>> /\ ops[1].ph = "rdy"
  /\ ops' = [ops EXCEPT ![1].ph = "req"]
  /\ tickets' = tickets \cup
       {[id |-> ops[1].id, a |-> ops[1].a, f |-> ops[1].f,
         c |-> ops[1].c, b |-> ops[1].b, dead |-> FALSE]}
  /\ UNCHANGED <<active, act, foc, ctx, crev, trev, nextId, adopted, leases, alarm>>

(* validate_callback at write_coordinator.rs:469-491 *)
ValidateTicket(t) ==
  /\ active
  /\ act = t.a /\ foc = t.f /\ ctx = t.c
  /\ crev = t.b
  /\ ops # <<>> /\ ops[1].id = t.id /\ ops[1].ph = "req"

(* complete_applied for the requested head: commit its projection,         *)
(* terminalize it, and issue a UI lease at the result revision.            *)
CompleteApplied ==
  /\ ops # <<>> /\ ops[1].ph = "req"
  /\ \E t \in tickets : t.id = ops[1].id /\ ValidateTicket(t)
  /\ crev' = IF ops[1].w THEN ops[1].r ELSE crev
  /\ trev' = IF Len(ops) = 1 THEN (IF ops[1].w THEN ops[1].r ELSE crev) ELSE trev
  /\ leases' = leases \cup
       {[id |-> ops[1].id, a |-> act, f |-> foc, c |-> ctx,
         r |-> ops[1].r, dead |-> FALSE]}
  /\ tickets' = {IF t.id = ops[1].id THEN [t EXCEPT !.dead = TRUE] ELSE t : t \in tickets}
  /\ ops' = Tail(ops)
  /\ UNCHANGED <<active, act, foc, ctx, nextId, adopted, alarm>>

(* reject(head): the head fails, every dependent is cancelled, the tail    *)
(* snaps back to the committed revision.  dmc = document may have changed. *)
RejectHead(dmc) ==
  /\ ops # <<>> /\ ops[1].ph = "req"
  /\ \E t \in tickets : t.id = ops[1].id
  /\ (dmc = TRUE) => (ops[1].w = TRUE)
  /\ crev' = IF dmc THEN ops[1].r ELSE crev
  /\ trev' = crev'
  /\ ops' = <<>>
  /\ adopted' = 0
  /\ tickets' = KillTickets
  /\ leases' = KillLeases
  /\ UNCHANGED <<active, act, foc, ctx, nextId, alarm>>

(* --- UI lease adoption --------------------------------------------------- *)

(* adopt_ui_lease at write_coordinator.rs:552-560 *)
AdoptOK(l) ==
  /\ active
  /\ act = l.a /\ foc = l.f /\ ctx = l.c
  /\ crev = l.r

Adopt ==
  \E l \in leases :
    /\ AdoptOK(l)
    /\ adopted' = l.id
    /\ alarm' = (alarm \/ l.dead)
    /\ UNCHANGED <<active, act, foc, ctx, crev, trev, ops, nextId, tickets, leases>>

(* --- specification ------------------------------------------------------- *)

Next ==
  \/ Activate \/ Deactivate \/ FocusChanged \/ RevisionBump
  \/ ObserveNew \/ ObserveReplace
  \/ Reserve \/ Attach(TRUE) \/ Attach(FALSE) \/ CancelReservation
  \/ BeginHead \/ CompleteApplied \/ RejectHead(TRUE) \/ RejectHead(FALSE)
  \/ Adopt

Spec == Init /\ [][Next]_vars

Constraint ==
  /\ act <= MaxGen /\ foc <= MaxGen
  /\ crev <= MaxRev /\ trev <= MaxRev
  /\ nextId <= MaxId + 1

(* --- properties ---------------------------------------------------------- *)

(* P: a dead ticket must never validate its callback. *)
TicketSafety == \A t \in tickets : t.dead => ~ValidateTicket(t)

(* P: a dead UI lease must never be adopted. *)
NoStaleAdopt == ~alarm

(* P: with an empty journal the speculative tail equals the committed rev. *)
EmptyQueueConsistent == (ops = <<>>) => (trev = crev)

(* P: at most one reservation outstanding, journal within capacity. *)
JournalBounded ==
  /\ Len(ops) <= Cap
  /\ Cardinality({i \in 1..Len(ops) : ops[i].ph = "res"}) <= 1

(* P: only the head may be in the Requested phase. *)
OnlyHeadRequested == \A i \in 2..Len(ops) : ops[i].ph # "req"

===============================================================================
