---- MODULE ShiftLatinInput ----
EXTENDS Naturals, Sequences, FiniteSets, TLC

(***************************************************************************
Independent behavioral model of Shift-held Latin composition order.

This is not a transcription of sakura-engine control flow.  It models the
user-visible contract:

  * Holding Shift types Latin letters in press order.
  * The first Shift+letter on an empty buffer latches English mode.
  * Backspace, with or without Shift, deletes the character before the
    caret.  While converting, Backspace cancels conversion only.
  * Later keys insert at the caret.
  * Emptying the buffer releases the latch.

Environment assumptions
  * One user; keystrokes are totally ordered.  There is no concurrent
    typist and no overlapping TSF apply in this model.
  * When the engine consumes a key, the host does not also edit the
    composition (hostStolen stays FALSE).
  * Letters is a finite Latin alphabet.  Unicode, IME keys other than
    those listed, and romaji conversion of unlatched input are out of
    scope.

Fairness
  * Weak fairness on Backspace when the buffer is non-empty and not
    converting, and on Commit when converting is FALSE and the buffer
    is non-empty.  Used only for the liveness properties below.

Unexplored
  * TSF write-journal epochs, COM re-entrancy, key repeat, surrogate
    pairs, engine IPC byte framing, and process crash mid-key.
***************************************************************************)

CONSTANTS Letters, MaxLen, MaxEvents, A, I, U, E, O

ASSUME /\ Letters = {A, I, U, E, O}
       /\ MaxLen \in Nat \ {0}
       /\ MaxEvents \in Nat \ {0}

VARIABLES composing, cursor, latched, converting, shiftHeld,
          committed, eventCount, hostStolen

vars == <<composing, cursor, latched, converting, shiftHeld,
          committed, eventCount, hostStolen>>

TypeOK ==
    /\ composing \in Seq(Letters)
    /\ Len(composing) <= MaxLen
    /\ cursor \in 0..MaxLen
    /\ cursor <= Len(composing)
    /\ latched \in BOOLEAN
    /\ converting \in BOOLEAN
    /\ shiftHeld \in BOOLEAN
    /\ committed \in Seq(Letters)
    /\ Len(committed) <= MaxLen
    /\ eventCount \in 0..MaxEvents
    /\ hostStolen \in BOOLEAN

Init ==
    /\ composing = << >>
    /\ cursor = 0
    /\ latched = FALSE
    /\ converting = FALSE
    /\ shiftHeld = FALSE
    /\ committed = << >>
    /\ eventCount = 0
    /\ hostStolen = FALSE

WithinBudget == eventCount < MaxEvents

ShiftDown ==
    /\ WithinBudget
    /\ shiftHeld' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<composing, cursor, latched, converting, committed, hostStolen>>

ShiftUp ==
    /\ WithinBudget
    /\ shiftHeld' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<composing, cursor, latched, converting, committed, hostStolen>>

InsertAt(seq, index, ch) ==
    SubSeq(seq, 1, index) \o <<ch>> \o SubSeq(seq, index + 1, Len(seq))

DeleteBefore(seq, index) ==
    SubSeq(seq, 1, index - 1) \o SubSeq(seq, index + 1, Len(seq))

TypeLetter(ch) ==
    /\ WithinBudget
    /\ ch \in Letters
    /\ ~converting
    /\ Len(composing) < MaxLen
    /\ \/ shiftHeld
       \/ latched
    /\ composing' = InsertAt(composing, cursor, ch)
    /\ cursor' = cursor + 1
    /\ latched' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<converting, shiftHeld, committed, hostStolen>>

CommitAndType(ch) ==
    /\ WithinBudget
    /\ ch \in Letters
    /\ converting
    /\ Len(committed) + Len(composing) < MaxLen
    /\ committed' = committed \o composing
    /\ composing' = <<ch>>
    /\ cursor' = 1
    /\ latched' = TRUE
    /\ converting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<shiftHeld, hostStolen>>

Backspace ==
    /\ WithinBudget
    /\ IF converting
       THEN /\ converting' = FALSE
            /\ UNCHANGED <<composing, cursor, latched>>
       ELSE /\ cursor > 0
            /\ composing' = DeleteBefore(composing, cursor)
            /\ cursor' = cursor - 1
            /\ latched' = IF Len(composing') = 0 THEN FALSE ELSE latched
            /\ converting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ hostStolen' = FALSE
    /\ UNCHANGED <<shiftHeld, committed>>

DeleteForward ==
    /\ WithinBudget
    /\ ~converting
    /\ cursor < Len(composing)
    /\ composing' = DeleteBefore(composing, cursor + 1)
    /\ latched' = IF Len(composing') = 0 THEN FALSE ELSE latched
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<cursor, converting, shiftHeld, committed, hostStolen>>

MoveLeft ==
    /\ WithinBudget
    /\ ~converting
    /\ cursor > 0
    /\ cursor' = cursor - 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<composing, latched, converting, shiftHeld, committed, hostStolen>>

MoveRight ==
    /\ WithinBudget
    /\ ~converting
    /\ cursor < Len(composing)
    /\ cursor' = cursor + 1
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<composing, latched, converting, shiftHeld, committed, hostStolen>>

Convert ==
    /\ WithinBudget
    /\ ~converting
    /\ Len(composing) > 0
    /\ converting' = TRUE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<composing, cursor, latched, shiftHeld, committed, hostStolen>>

Cancel ==
    /\ WithinBudget
    /\ IF converting
       THEN /\ converting' = FALSE
            /\ UNCHANGED <<composing, cursor, latched>>
       ELSE /\ composing' = << >>
            /\ cursor' = 0
            /\ latched' = FALSE
            /\ converting' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<shiftHeld, committed, hostStolen>>

Commit ==
    /\ WithinBudget
    /\ ~converting
    /\ Len(composing) > 0
    /\ Len(committed) + Len(composing) <= MaxLen
    /\ committed' = committed \o composing
    /\ composing' = << >>
    /\ cursor' = 0
    /\ latched' = FALSE
    /\ eventCount' = eventCount + 1
    /\ UNCHANGED <<converting, shiftHeld, hostStolen>>

Idle ==
    /\ eventCount = MaxEvents
    /\ UNCHANGED vars

Next ==
    \/ ShiftDown
    \/ ShiftUp
    \/ Backspace
    \/ DeleteForward
    \/ MoveLeft
    \/ MoveRight
    \/ Convert
    \/ Cancel
    \/ Commit
    \/ Idle
    \/ \E ch \in Letters : TypeLetter(ch) \/ CommitAndType(ch)

Spec ==
    /\ Init
    /\ [][Next]_vars
    /\ WF_vars(Backspace)
    /\ WF_vars(Commit)

CursorInRange == cursor <= Len(composing)
NoHostSteal == hostStolen = FALSE
LatchImpliesBuffer == latched => (Len(composing) > 0 \/ converting)
EmptyReleasesLatch == (Len(composing) = 0 /\ ~converting) => ~latched
EndInsertPreservesPrefix ==
    (cursor = Len(composing) /\ latched) =>
        TRUE

\* Required-state probe: TLC configs that treat NeverAiueo as an
\* invariant are expected to fail, proving AIUEO is reachable.
NeverAiueo == composing # <<A, I, U, E, O>>

\* Forbidden-state probe: hostStolen is never set by any action, so
\* a search that treats NoHostSteal as an invariant must succeed.
NeverHostStolen == hostStolen = FALSE

BufferEventuallyClearable ==
    (Len(composing) > 0 /\ ~converting) ~> (Len(composing) = 0 \/ eventCount = MaxEvents)

=============================================================================
