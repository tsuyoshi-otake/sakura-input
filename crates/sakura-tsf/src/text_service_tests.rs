use super::*;
use crate::testing::fake_engine::{
    fake_engine_for_reject_then_key, fake_engine_for_undo_timeout, fake_engine_for_unknown_undo,
    fake_engine_with_no_further_requests,
};
use sakura_proto::{Modifiers, Segment, UnderlineKind};

#[test]
fn wait_status_labels_name_the_in_flight_ai_operation() {
    assert_eq!(
        ai_wait_status_label(AiTextOperation::Transform),
        "AI変換中…"
    );
    assert_eq!(
        ai_wait_status_label(AiTextOperation::Proofread),
        "AI校正中…"
    );
}

fn layout_claim(context: ContextId) -> LayoutQueryClaim {
    let mut journal: WriteCoordinator<()> = WriteCoordinator::new(1);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(context).is_empty());
    let reservation = journal.reserve(context).expect("reserve");
    let visible = journal.tail_visible();
    journal
        .attach(reservation, (), false, visible.clone(), visible)
        .expect("attach");
    let request = journal.begin_head().expect("request");
    let lease = journal
        .complete_applied(request.ticket)
        .expect("completion")
        .ui_lease
        .expect("UI lease");
    LayoutQueryClaim { context, lease }
}

/// Traces what `recover_from_engine_unavailable` (this file, reached
/// from `Reconvert` on `Answer::Unavailable`) actually does to a
/// *healthy* connection and an unrelated in-flight write -- not from
/// reading the source, but by executing its three COM-free steps
/// directly.
///
/// `Reconvert`'s own precondition (`composition_is_idle`) means a local
/// `TooLarge` there can never reach this rescue path with a live
/// composition on screen, and `enqueue_finalization`'s early return for
/// an idle/empty projection means the one step this test cannot exercise
/// (`finalize_visible_text_async`, which needs a real `ITfContext` this
/// crate's test suite has no way to construct without a live COM host)
/// is a documented no-op in that exact case. What remains observable
/// here is the blast radius of the first three steps: do they discard
/// more than the one operation that actually failed?
#[test]
fn recovering_from_a_local_encode_failure_still_drops_a_healthy_link_and_its_own_reservation() {
    let (name, server) = fake_engine_with_no_further_requests("recovery");

    let service = TextService::new();
    *service.engine.borrow_mut() = Engine::attached_to(&name);
    assert!(
        service.engine.borrow().is_connected(),
        "the handshake must have completed"
    );

    // Stands in for the reservation `Reconvert` takes out before it ever
    // asks the engine anything (`reserve_write`, called before
    // `ask_reconversion`) -- still unattached, exactly as it is at the
    // moment a local encode failure answers `Unavailable` and this
    // rescue path runs.
    let context = ContextId(1);
    {
        let mut writes = service.writes.borrow_mut();
        assert!(writes.activate().is_empty());
        assert!(writes.observe_context(context).is_empty());
        writes
            .reserve(context)
            .expect("the journal must admit one reservation");
    }
    assert_eq!(service.writes.borrow().pending_len(), 1);

    // `recover_from_engine_unavailable`'s three COM-free steps, in
    // order, verbatim from `text_service.rs`.
    service.invalidate_inflight_composition_write_as_unknown();
    service.cancel_all_writes(CancelReason::EngineUnavailable, true);
    service.disconnect(DisconnectReason::EngineUnavailableRecovery);

    assert_eq!(
        service.writes.borrow().pending_len(),
        0,
        "the rescue path drains the whole journal -- including a \
             reservation the failed local request never actually used -- \
             not just the one operation that failed"
    );
    assert!(
        !service.engine.borrow().is_connected(),
        "current behavior: recovering from a request this process's \
             own encoder rejected locally still tears down an otherwise \
             healthy engine connection"
    );

    drop(service);
    server.join().expect("the server thread");
}

/// The full contract for a local reconvert encode failure. `Engine::request`
/// answers `Answer::Rejected` without dropping the link (`engine.rs`), and
/// `Reconvert`'s `Answer::Rejected` arm (this file) cancels only its own
/// reservation -- with the accurately-named `CancelReason::RequestRejected`,
/// not the `PredecessorFailed` used for the `Busy` arm right next to it --
/// and never calls `recover_from_engine_unavailable`. Contrast with
/// `recovering_from_a_local_encode_failure_still_drops_a_healthy_link_and_its_own_reservation`
/// above, which proves what that rescue path *does* do when it runs: this
/// test's surviving unrelated write and surviving link are the observable
/// proof that path was never invoked.
///
/// `Reconvert`'s live `service.writes: WriteCoordinator<PendingWrite>`
/// needs a real `ITfContext` to `attach` a second, unrelated write --
/// unavailable in this crate's test suite without a live COM host (the
/// same constraint documented on `layout_claim`). `cancel_reservation`'s
/// scoping (`write_coordinator.rs`) has no special case per payload type,
/// so a standalone `WriteCoordinator<()>` proves the exact mechanism the
/// fix calls.
#[test]
fn local_reconvert_encode_failure_rejects_only_that_operation() {
    // The scoped-cancellation half of the contract: does cancelling the
    // failing operation's own reservation leave an unrelated,
    // already-attached write alone?
    let context = ContextId(1);
    let mut journal: WriteCoordinator<()> = WriteCoordinator::new(2);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(context).is_empty());

    let unrelated = journal
        .reserve(context)
        .expect("reserve the unrelated write");
    let visible = journal.tail_visible();
    journal
        .attach(unrelated, (), false, visible.clone(), visible.clone())
        .expect("attach the unrelated write ahead of the failing one");

    let own = journal.reserve(context).expect(
        "the coordinator must still admit the failing operation's own \
             reservation behind the already-attached unrelated one",
    );
    assert_eq!(journal.pending_len(), 2);

    let cancelled = journal.cancel_reservation(own, CancelReason::RequestRejected);
    assert_eq!(
        cancelled.len(),
        1,
        "only the failing operation's own reservation is cancelled"
    );
    assert_eq!(
        journal.pending_len(),
        1,
        "the unrelated, already-attached write must still be pending"
    );

    // The link-and-request half of the contract, against the real
    // `Engine` and a real peer: `Answer::Rejected`, the link stays
    // usable, and a normal request right after succeeds on it.
    let (name, server) = fake_engine_for_reject_then_key("reject-scope");
    let mut engine = Engine::attached_to(&name);
    assert!(engine.is_connected(), "the handshake must have completed");

    // A `text` this large fails `write_str`'s own `MAX_STRING_BYTES`
    // (4096 bytes, see `wire.rs`) check well before the request could
    // even approach `MAX_PAYLOAD` (64 KiB).
    let huge_text = "あ".repeat(30_000); // 90,000 bytes
    assert!(
        matches!(engine.reconvert(huge_text, false), Answer::Rejected),
        "a local encode failure must answer Rejected, not Unavailable"
    );
    assert!(
        engine.is_connected(),
        "a local encode failure must not drop an otherwise healthy link"
    );
    assert!(
        matches!(
            engine.send_key(KeyInput {
                code: KeyCode::Char,
                ch: Some('k'),
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            }),
            Answer::Ready(_)
        ),
        "the immediately following normal request must succeed on the \
             same link"
    );

    drop(engine);
    server.join().expect("the server thread");
}

fn preedit(parts: &[&str]) -> Preedit {
    Preedit {
        segments: parts
            .iter()
            .map(|text| Segment {
                text: (*text).to_owned(),
                underline: UnderlineKind::Raw,
            })
            .collect(),
        cursor: 0,
    }
}

/// Builds a `Preedit` out of explicit `(text, underline)` pairs, for
/// tests that need something other than every segment defaulting to
/// `UnderlineKind::Raw`.
fn preedit_with_underlines(parts: &[(&str, UnderlineKind)]) -> Preedit {
    Preedit {
        segments: parts
            .iter()
            .map(|(text, underline)| Segment {
                text: (*text).to_owned(),
                underline: *underline,
            })
            .collect(),
        cursor: 0,
    }
}

/// The concatenation of every segment's text, discarding its underline --
/// the same flattening `visible_text` does, but taking the segments
/// `Update::Show` now carries directly rather than a whole `Preedit`.
fn segments_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect()
}

fn output(commit: Option<&str>, shown: Option<&[&str]>) -> Output {
    Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: shown.map(preedit),
        commit: commit.map(str::to_owned),
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    }
}

fn state(text: &str, has_composition: bool) -> VisibleState {
    VisibleState {
        text: text.to_owned(),
        has_composition,
    }
}

#[test]
fn adjacency_observation_gate_requires_exact_collapsed_caret_and_text() {
    assert!(adjacency_observations_match(
        true,
        true,
        Some("考慮漏れ"),
        "考慮漏れ"
    ));
    assert!(!adjacency_observations_match(
        false,
        true,
        Some("考慮漏れ"),
        "考慮漏れ"
    ));
    assert!(!adjacency_observations_match(
        true,
        false,
        Some("考慮漏れ"),
        "考慮漏れ"
    ));
    assert!(!adjacency_observations_match(true, true, None, "考慮漏れ"));
    assert!(!adjacency_observations_match(
        true,
        true,
        Some("検討漏れ"),
        "考慮漏れ"
    ));
}

#[test]
fn only_a_complete_nonempty_composition_commit_arms_adjacency() {
    let complete = WritePlan {
        updates: vec![Update::Commit("考慮漏れ".to_owned())],
        before: state("考慮漏れ", true),
        after: state("", false),
    };
    assert_eq!(adjacency_commit_text(&complete), Some("考慮漏れ"));

    for plan in [
        WritePlan {
            updates: vec![Update::Commit("a".to_owned())],
            before: state("", false),
            after: state("", false),
        },
        WritePlan {
            updates: vec![Update::Commit("一部".to_owned())],
            before: state("一部残り", true),
            after: state("残り", true),
        },
        WritePlan {
            updates: vec![Update::Commit(String::new())],
            before: state("考慮漏れ", true),
            after: state("", false),
        },
        WritePlan {
            updates: vec![Update::Discard],
            before: state("考慮漏れ", true),
            after: state("", false),
        },
    ] {
        assert_eq!(adjacency_commit_text(&plan), None);
    }
}

#[test]
fn authoritative_adjacency_invalidation_cannot_be_overwritten_by_late_publication() {
    let service = TextService::new();
    service.focus_foreground.set(true);
    service
        .cached_input_scope
        .set(Some(sakura_proto::InputScope::Normal));

    service.invalidate_document_adjacency();
    service.publish_document_adjacency(DocumentAdjacency::Unverified);

    assert!(service.document_adjacency.borrow().is_none());
    assert!(
        service.document_adjacency_must_reset.get(),
        "the next real key must still retire engine-side context"
    );
}

#[test]
fn commit_undo_post_document_access_projection_failure_reports_unknown_and_terminalizes_link() {
    let context_id = ContextId(77);
    let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(2);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(context_id).is_empty());
    let reservation = journal.reserve(context_id).expect("reservation");
    let before = journal.tail_visible();
    journal
        .attach(reservation, true, true, before.clone(), state("復元", true))
        .expect("attach pending undo");
    let ticket = journal.begin_head().expect("requested head").ticket;
    let (name, peer) = fake_engine_for_unknown_undo("projection-failure");
    let service = TextService::new();
    *service.engine.borrow_mut() = Engine::attached_to(&name);
    assert!(service.engine.borrow().is_connected());
    let settlement_count = Cell::new(0);
    let terminal = terminalize_unknown_undo_after_document_access(
        &mut journal,
        ticket,
        |payload| *payload,
        |outcome| {
            settlement_count.set(settlement_count.get() + 1);
            assert_eq!(outcome, UndoCommitOutcome::Unknown);
            assert!(service.settle_undo_commit(outcome));
            false
        },
    );

    assert_eq!(terminal.completions.len(), 1);
    assert_eq!(settlement_count.get(), 1);
    assert!(terminal.has_undo);
    assert!(terminal.journal_drained);
    assert!(!terminal.retry_allowed);
    assert!(!terminal.settlement_confirmed);
    assert!(terminal.disconnect_required);
    assert_eq!(journal.pending_len(), 0);

    // The production caller retires the link when this shared primitive
    // cannot confirm the settlement. The callback/context integration is
    // intentionally not fabricated here; the real PendingWrite invariant
    // remains `context: ITfContext` in production.
    if terminal.disconnect_required {
        service.disconnect(DisconnectReason::DocumentAccessUndoTerminalized);
    }
    assert!(!service.engine.borrow().is_connected());
    peer.join().expect("fake engine terminal outcome");
}

#[test]
fn commit_undo_reentrant_journal_borrow_has_a_bounded_terminal_owner() {
    // A COM-free test cannot fabricate the real `PendingWrite.context`.
    // Exercise the shared production Probe decision and journal primitive
    // with its equivalent payload first, then exercise the TextService
    // marker that owns a refused `RefCell` borrow. Together these are the
    // production units used by the real callback path.
    let context_id = ContextId(78);
    let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(2);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(context_id).is_empty());
    let reservation = journal.reserve(context_id).expect("reservation");
    let before = journal.tail_visible();
    journal
        .attach(reservation, true, true, before.clone(), state("x", true))
        .expect("attach pending undo");
    let ticket = journal.begin_head().expect("requested head").ticket;
    let service = TextService::new();
    service
        .undo_terminalization
        .set(Some(UndoCommitOutcome::Unknown));
    let marker_before = service.undo_terminalization.get();
    let pending_before = journal.pending_len();
    let terminals_before = journal.terminal_records();
    let settlement_count = Cell::new(0);
    assert_eq!(
        service.probe_fence(context_id).expect("Probe fence"),
        ProbeFence::Busy
    );
    assert_eq!(
        decide_probe_fence(
            service.undo_terminalization.get(),
            &journal,
            context_id,
            |payload| *payload,
            false,
            false,
        ),
        ProbeFence::Busy
    );
    // Probe has only read the shared production decision inputs: the
    // deferred marker, journal payload, terminal records, and settlement
    // callback state are byte-for-byte/logically unchanged.
    assert_eq!(service.undo_terminalization.get(), marker_before);
    assert_eq!(journal.pending_len(), pending_before);
    assert_eq!(journal.terminal_records(), terminals_before);
    assert_eq!(settlement_count.get(), 0);

    // The corresponding real-key owner settles once before returning the
    // same consumed result. An empty journal has no payload to settle, but
    // its marker still must be cleared rather than allowing the key to be
    // applied after the handoff.
    assert!(service.deferred_undo_consumes_real_key());
    assert_eq!(service.undo_terminalization.get(), None);
    assert_eq!(settlement_count.get(), 0);
    service
        .undo_terminalization
        .set(Some(UndoCommitOutcome::Unknown));

    let terminal = terminalize_unknown_undo_after_document_access(
        &mut journal,
        ticket,
        |payload| *payload,
        |outcome| {
            settlement_count.set(settlement_count.get() + 1);
            assert_eq!(outcome, UndoCommitOutcome::Unknown);
            false
        },
    );
    assert_eq!(terminal.completions.len(), 1);
    assert!(terminal.has_undo);
    assert!(terminal.journal_drained);
    assert!(!terminal.retry_allowed);
    assert!(!terminal.settlement_confirmed);
    assert!(terminal.disconnect_required);
    assert_eq!(settlement_count.get(), 1);
    assert_eq!(journal.pending_len(), 0);

    let writes = service.writes.borrow_mut();

    // This is the failure point shared by cancel-all and the callback
    // cancellation helpers. Returning here without an owner would leave
    // a real pending undo transaction fenced forever; the non-empty
    // generic journal above proves the payload side before this COM-free
    // marker test.
    service.cancel_all_writes_with_undo_outcome(
        CancelReason::StaleCallback,
        true,
        Some(UndoCommitOutcome::Unknown),
    );
    assert_eq!(
        service.undo_terminalization.get(),
        Some(UndoCommitOutcome::Unknown)
    );
    drop(writes);

    // The next owner drains any journal left by the re-entrant owner and
    // clears the marker exactly once; no later key can observe a stale
    // Busy/pending state.
    assert!(service.try_settle_deferred_undo_terminalization());
    assert_eq!(service.undo_terminalization.get(), None);
    assert_eq!(service.writes.borrow().pending_len(), 0);
    assert!(service.try_settle_deferred_undo_terminalization());
    assert_eq!(service.undo_terminalization.get(), None);
}

#[test]
fn commit_undo_test_only_context_fences_preserve_replacement_cleanup_ownership() {
    let first = ContextId(81);
    let second = ContextId(82);

    let mut same_context_full: WriteCoordinator<bool> = WriteCoordinator::new(1);
    assert!(same_context_full.activate().is_empty());
    assert!(same_context_full.observe_context(first).is_empty());
    let reservation = same_context_full
        .reserve(first)
        .expect("same-context reservation");
    assert!(!same_context_full.can_admit_for_context(first));
    assert_eq!(
        decide_probe_fence(None, &same_context_full, first, |_| false, false, false),
        ProbeFence::Declined,
        "a same-context full/reserved journal remains an ordinary host decline"
    );
    assert_eq!(same_context_full.pending_len(), 1);
    assert!(
        same_context_full
            .cancel_reservation(reservation, CancelReason::PredecessorFailed)
            .len()
            == 1
    );

    let mut different_context_full: WriteCoordinator<bool> = WriteCoordinator::new(1);
    assert!(different_context_full.activate().is_empty());
    assert!(different_context_full.observe_context(first).is_empty());
    let _reservation = different_context_full
        .reserve(first)
        .expect("different-context reservation");
    assert!(different_context_full.can_admit_for_context(second));
    assert_eq!(
        decide_probe_fence(
            None,
            &different_context_full,
            second,
            |_| false,
            false,
            false,
        ),
        ProbeFence::ContextReplacement,
        "Probe must fence a replacement instead of using the old session"
    );
    assert_eq!(different_context_full.pending_len(), 1);
    let cancelled = different_context_full.observe_context(second);
    assert_eq!(cancelled.len(), 1, "real replacement owns old cleanup once");
    assert_eq!(different_context_full.pending_len(), 0);
    assert!(!different_context_full.is_context_replacement(second));
    assert!(different_context_full.can_admit_for_context(second));
    // The reservation was terminalized by the one real replacement owner;
    // a second cleanup attempt has no journal entry to consume.
    assert!(different_context_full.observe_context(second).is_empty());

    let mut different_context_non_full: WriteCoordinator<bool> = WriteCoordinator::new(2);
    assert!(different_context_non_full.activate().is_empty());
    assert!(different_context_non_full.observe_context(first).is_empty());
    assert!(different_context_non_full.can_admit_for_context(second));
    assert_eq!(
        decide_probe_fence(
            None,
            &different_context_non_full,
            second,
            |_| false,
            false,
            false,
        ),
        ProbeFence::ContextReplacement,
        "a non-full replacement is still a context transition, not an old-session Probe"
    );
    assert_eq!(different_context_non_full.pending_len(), 0);
    assert!(different_context_non_full
        .observe_context(second)
        .is_empty());
    assert!(!different_context_non_full.is_context_replacement(second));
}

#[test]
fn commit_undo_real_context_replacement_continues_first_and_toggle_keys_to_apply() {
    let first = ContextId(85);
    let second = ContextId(86);
    let mut journal: WriteCoordinator<()> = WriteCoordinator::new(1);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(first).is_empty());
    let reservation = journal.reserve(first).expect("old-context reservation");
    assert!(journal.is_context_replacement(second));

    // This is the exact action selected by the production
    // `handle_key_input` branch before it calls `observe_write_context`.
    // The action is independent of the physical key, so both the first
    // character and the preserved HankakuZenkaku key must continue into
    // the same Apply path after cleanup.
    for (name, key) in [
        (
            "first character",
            KeyInput {
                code: KeyCode::Char,
                ch: Some('a'),
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
        ),
        (
            "HankakuZenkaku",
            KeyInput {
                code: KeyCode::HankakuZenkaku,
                ch: None,
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
        ),
    ] {
        assert!(!key.test_only, "{name} must use the real-key action");
        assert_eq!(
            decide_real_fence(false, false, false, false, true),
            RealFenceAction::ReplaceAndApply,
            "successful context replacement must continue {name} to Apply"
        );
    }

    let cancelled = journal.observe_context(second);
    assert_eq!(cancelled.len(), 1, "replacement cleanup has one owner");
    assert_eq!(journal.pending_len(), 0);
    assert!(!journal.is_context_replacement(second));
    assert_eq!(
        decide_real_fence(false, false, false, false, false),
        RealFenceAction::Apply,
        "after successful cleanup the same callback reaches Apply"
    );

    // Every non-Apply branch remains an explicit terminal result; none
    // may silently fall through to a raw host key or a second Apply.
    assert_eq!(
        decide_real_fence(true, true, true, true, true),
        RealFenceAction::DeferredTerminalization
    );
    assert_eq!(
        decide_real_fence(false, true, true, true, true),
        RealFenceAction::Consume
    );
    assert_eq!(
        decide_real_fence(false, false, false, true, true),
        RealFenceAction::Decline
    );

    // The reservation was terminalized by replacement ownership; no
    // second cleanup or callback remains to interfere with the Apply path.
    assert!(journal.observe_context(second).is_empty());
    assert!(journal
        .cancel_reservation(reservation, CancelReason::PredecessorFailed)
        .is_empty());
}

#[test]
fn queued_conversion_show_is_not_dropped_by_a_later_candidate_end() {
    let mut work = super::DeferredWork {
        candidates: Some(7),
        ..super::DeferredWork::<u8>::default()
    };
    work.retain_candidate_end();
    assert_eq!(
        work.candidates,
        Some(7),
        "履歴ポップアップ中の変換 Show を End が消してはいけない"
    );
    assert!(
        !work.end_candidates,
        "Show が残っているなら End は起こさない"
    );
}

#[test]
fn converting_space_keeps_a_live_composition_across_context_replacement() {
    let space = KeyInput {
        code: KeyCode::Space,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    let henkan = KeyInput {
        code: KeyCode::Henkan,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    let letter = KeyInput {
        code: KeyCode::Char,
        ch: Some('a'),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    assert!(keep_live_composition_for_convert(space, true));
    assert!(keep_live_composition_for_convert(henkan, true));
    assert!(!keep_live_composition_for_convert(letter, true));
    assert!(!keep_live_composition_for_convert(space, false));
    assert!(!journal_replacement_applies(space, true, true));
    assert!(journal_replacement_applies(letter, true, true));
    assert_eq!(
        decide_real_fence(false, false, false, false, false),
        RealFenceAction::Apply,
        "Space during a live reading must convert, not replace-and-insert"
    );
    assert_eq!(
        decide_real_fence(false, false, false, false, true),
        RealFenceAction::ReplaceAndApply,
        "non-convert keys still follow replacement"
    );

    assert_eq!(
        PhysicalKeyOwner::of(space, true),
        PhysicalKeyOwner::Ime,
        "OnTestKeyDown of Space against a live reading is IME-owned"
    );
    assert_eq!(
        PhysicalKeyOwner::of(henkan, true),
        PhysicalKeyOwner::Ime,
        "Henkan is IME-owned on TestKeyDown even when it is also the AI trigger"
    );
    assert_eq!(
        PhysicalKeyOwner::of(letter, true),
        PhysicalKeyOwner::HostEligible
    );
    assert_eq!(
        PhysicalKeyOwner::of(space, false),
        PhysicalKeyOwner::HostEligible,
        "idle Space stays host-eligible so Japanese fullwidth insert works"
    );
    let ctrl_space = KeyInput {
        code: KeyCode::Space,
        ch: None,
        modifiers: Modifiers::CTRL,
        repeat: false,
        test_only: true,
    };
    let alt_space = KeyInput {
        modifiers: Modifiers::ALT,
        ..space
    };
    assert_eq!(
        PhysicalKeyOwner::of(ctrl_space, true),
        PhysicalKeyOwner::HostEligible,
        "Ctrl+Space remains IntelliSense"
    );
    assert_eq!(
        PhysicalKeyOwner::of(alt_space, true),
        PhysicalKeyOwner::HostEligible
    );
    assert!(
        PhysicalKeyOwner::Ime.terminal_eaten(false).as_bool(),
        "admission/Probe failure must not give a live convert key to the host"
    );
    assert!(!PhysicalKeyOwner::HostEligible
        .terminal_eaten(false)
        .as_bool());
    assert!(PhysicalKeyOwner::HostEligible
        .terminal_eaten(true)
        .as_bool());
    assert!(
        ConversionKeyDisposition::HostEligible.eats_blocked(false, letter, Some(Mode::Hiragana)),
        "blocked first letter in Japanese mode must not become WM_CHAR"
    );
    assert!(
        !ConversionKeyDisposition::HostEligible.eats_blocked(false, letter, Some(Mode::Direct)),
        "Direct mode still belongs to the host"
    );

    // Keep the real-key fence regression document-free: a blocked live
    // conversion path declines its engine work, but must still eat the
    // physical Space before Electron can insert it into the reading.
    let action = decide_real_fence(false, false, false, true, false);
    assert_eq!(action, RealFenceAction::Decline);
    let eaten = match action {
        RealFenceAction::Decline => PhysicalKeyOwner::of(space, true).terminal_eaten(false),
        _ => false.into(),
    };
    assert!(
        eaten.as_bool(),
        "a real-path Decline must not return a live conversion Space to the host"
    );

    assert_eq!(
        resolve_convert_write_source(false, true, false, true),
        ConvertWriteSource::LiveStored,
        "a suggestion layout still names the live document after composition.context is missing"
    );
    assert_eq!(
        resolve_convert_write_source(false, false, true, true),
        ConvertWriteSource::LiveStored,
        "a queued suggestion payload is enough to retarget Space onto the reading"
    );
    assert_eq!(
        resolve_convert_write_source(false, false, false, false),
        ConvertWriteSource::Callback
    );
    assert_eq!(
        resolve_convert_write_source(false, false, false, true),
        ConvertWriteSource::Absorb,
        "an idle peer without a stored live document must not receive the conversion"
    );
}

#[test]
fn queued_engine_recovery_consumes_keys_until_the_finalizer_terminates() {
    assert_eq!(
            decide_real_fence(false, false, true, false, false),
            RealFenceAction::Consume,
            "returning the key to the host while the old composition finalizer is queued allows stale text to be replayed after host edits"
        );

    let context = ContextId(57);
    let mut journal = WriteCoordinator::<()>::new(1);
    assert!(journal.activate().is_empty());
    assert!(journal.observe_context(context).is_empty());
    assert_eq!(
        decide_probe_fence(None, &journal, context, |_| false, true, false),
        ProbeFence::Busy,
        "OnTestKeyDown must report the same recovery fence without mutating it"
    );
}

#[test]
fn commit_undo_probe_fence_priority_matches_real_terminal_order() {
    assert_eq!(
        probe_action(ProbeFence::ContextReplacement),
        ProbeAction::Ask {
            fresh_context: true
        }
    );
    assert_eq!(
        probe_action(ProbeFence::Open),
        ProbeAction::Ask {
            fresh_context: false
        }
    );

    struct Case {
        name: &'static str,
        marker: Option<UndoCommitOutcome>,
        payload_is_undo: Option<bool>,
        input_blocked: bool,
        requested_context: ContextId,
        expected: ProbeFence,
    }

    let first = ContextId(91);
    let second = ContextId(92);
    let cases = [
        Case {
            name: "marker dominates every other fence",
            marker: Some(UndoCommitOutcome::Unknown),
            payload_is_undo: Some(true),
            input_blocked: true,
            requested_context: second,
            expected: ProbeFence::Busy,
        },
        Case {
            name: "undo payload dominates input block and replacement",
            marker: None,
            payload_is_undo: Some(true),
            input_blocked: true,
            requested_context: second,
            expected: ProbeFence::Busy,
        },
        Case {
            name: "input block dominates replacement",
            marker: None,
            payload_is_undo: Some(false),
            input_blocked: true,
            requested_context: second,
            expected: ProbeFence::Declined,
        },
        Case {
            name: "replacement dominates full admission",
            marker: None,
            payload_is_undo: Some(false),
            input_blocked: false,
            requested_context: second,
            expected: ProbeFence::ContextReplacement,
        },
        Case {
            name: "same-context full journal declines",
            marker: None,
            payload_is_undo: Some(false),
            input_blocked: false,
            requested_context: first,
            expected: ProbeFence::Declined,
        },
        Case {
            name: "open admission probes",
            marker: None,
            payload_is_undo: None,
            input_blocked: false,
            requested_context: first,
            expected: ProbeFence::Open,
        },
    ];

    for case in cases {
        let capacity = usize::from(case.payload_is_undo.is_some()).max(1);
        let mut journal: WriteCoordinator<bool> = WriteCoordinator::new(capacity);
        assert!(journal.activate().is_empty(), "{}", case.name);
        assert!(journal.observe_context(first).is_empty(), "{}", case.name);
        if let Some(payload_is_undo) = case.payload_is_undo {
            let reservation = journal.reserve(first).expect(case.name);
            let before = journal.tail_visible();
            journal
                .attach(
                    reservation,
                    payload_is_undo,
                    true,
                    before,
                    state("fence", true),
                )
                .expect(case.name);
        }

        let pending_before = journal.pending_len();
        let terminals_before = journal.terminal_records();
        let actual = decide_probe_fence(
            case.marker,
            &journal,
            case.requested_context,
            |payload| *payload,
            false,
            case.input_blocked,
        );
        assert_eq!(actual, case.expected, "{}", case.name);
        assert_eq!(journal.pending_len(), pending_before, "{}", case.name);
        assert_eq!(
            journal.terminal_records(),
            terminals_before,
            "{}",
            case.name
        );
    }
}

#[test]
fn commit_undo_early_rejected_timeout_drops_the_live_link() {
    let (name, peer) = fake_engine_for_undo_timeout("early-rejected");
    let service = TextService::new();
    *service.engine.borrow_mut() = Engine::attached_to(&name);
    assert!(service.engine.borrow().is_connected());

    assert!(!service.settle_undo_commit_or_disconnect(UndoCommitOutcome::Rejected));
    assert!(!service.engine.borrow().is_connected());
    peer.join().expect("timeout fake engine");
}

#[test]
fn preserved_key_registration_is_only_the_unmodified_vk_kanji_toggle() {
    let registrations = preserved_key_registrations()
        .iter()
        .map(|registration| {
            (
                registration.guid,
                registration.key.uVKey,
                registration.key.uModifiers,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        registrations,
        vec![(GUID_PRESERVEDKEY_IME_TOGGLE, VK_KANJI.0 as u32, 0)]
    );
}

#[test]
fn preserved_key_input_maps_only_the_toggle_guid() {
    assert_eq!(
        preserved_key_input(&GUID_PRESERVEDKEY_IME_TOGGLE),
        Some(KeyInput {
            code: KeyCode::HankakuZenkaku,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        })
    );

    for guid in [
        sakura_reg::GUID_PRESERVEDKEY_IME_ON,
        sakura_reg::GUID_PRESERVEDKEY_IME_OFF,
        GUID::from_u128(0),
    ] {
        assert_eq!(preserved_key_input(&guid), None);
    }
}

#[test]
fn unknown_key_without_character_is_declined_before_tsf_work() {
    let modifier_only = KeyInput {
        code: KeyCode::Unknown,
        ch: None,
        modifiers: Modifiers::SHIFT,
        repeat: false,
        test_only: false,
    };
    assert!(is_unactionable_key_input(modifier_only));

    let enter = KeyInput {
        code: KeyCode::Enter,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    assert!(!is_unactionable_key_input(enter));
}

#[test]
fn shifted_ascii_letter_is_not_declined_by_modifier_guard() {
    let shifted_letter = KeyInput {
        code: KeyCode::Char,
        ch: Some('A'),
        modifiers: Modifiers::SHIFT,
        repeat: false,
        test_only: false,
    };

    assert!(!is_unactionable_key_input(shifted_letter));
}

#[test]
fn input_scope_mapping_is_sensitive_and_unknown_values_fail_closed() {
    assert_eq!(
        map_tf_input_scope(TfInputScope(0)),
        Some(InputScope::Normal)
    );
    assert_eq!(map_tf_input_scope(TfInputScope(1)), Some(InputScope::Url));
    assert_eq!(map_tf_input_scope(TfInputScope(4)), Some(InputScope::Email));
    assert_eq!(
        map_tf_input_scope(TfInputScope(6)),
        Some(InputScope::Password)
    );
    assert_eq!(
        map_tf_input_scope(TfInputScope(20)),
        Some(InputScope::Digits)
    );

    assert_eq!(
        classify_tf_input_scopes(&[TfInputScope(0), TfInputScope(6)]),
        InputScope::Password
    );
    assert_eq!(
        classify_tf_input_scopes(&[TfInputScope(0), TfInputScope(999)]),
        InputScope::Unclassified
    );
}

#[test]
fn a_host_that_declares_no_scope_is_ordinary_text_not_a_classification_failure() {
    // Notepad, VS Code, and every plain text field answer this way. When
    // it was treated as a failure the developer history discarded 100% of
    // real input while reporting zero drops and zero persistence errors.
    assert_eq!(classify_declared_scopes(None), InputScope::Normal);

    // A declared scope still decides the class, and an unrecognised value
    // still fails closed.
    assert_eq!(
        classify_declared_scopes(Some(&[TfInputScope(0)])),
        InputScope::Normal
    );
    assert_eq!(
        classify_declared_scopes(Some(&[TfInputScope(6)])),
        InputScope::Password
    );
    assert_eq!(
        classify_declared_scopes(Some(&[TfInputScope(0), TfInputScope(999)])),
        InputScope::Unclassified
    );
}

#[test]
fn an_empty_variant_classifies_as_normal_and_other_shapes_stay_fail_closed() {
    assert_eq!(
        classify_input_scope_variant(VARIANT::default()).expect("an empty VARIANT classifies"),
        InputScope::Normal
    );

    // A VARIANT carrying something other than an ITfInputScope is a
    // property this build does not understand, so it must not be guessed.
    assert!(classify_input_scope_variant(VARIANT::from(42i32)).is_err());
}

#[test]
fn lifecycle_revocation_prevents_an_inflight_callback_from_republishing_state() {
    let mut owner = CompositionWriteOwner::default();
    let flight = owner.begin().expect("first composition flight");
    assert!(owner.owns(flight));
    assert!(owner.lifecycle_is_current(flight));

    // This models OnCompositionTerminated, detach, or context replacement
    // running while the callback is inside a host COM call. The callback's
    // later publish/fail attempt must be a no-op rather than restoring its
    // cloned handle into the lifecycle-retired CompositionState.
    owner.invalidate();
    assert!(!owner.owns(flight));
    assert!(!owner.finish(flight));
    assert!(!owner.lifecycle_is_current(flight));

    let next = owner.begin().expect("next composition flight");
    assert_ne!(flight, next);
    assert!(owner.owns(next));
}

#[test]
fn expected_self_termination_is_not_an_external_lifecycle_event() {
    let mut owner = CompositionWriteOwner::default();
    let flight = owner.begin().expect("composition flight");
    let canonical = CompositionIdentity(7);

    // Before the precise EndComposition callback arms the marker, a host
    // termination for this same composition is still external and must
    // take the lifecycle cancellation path.
    assert!(!expected_self_termination_matches(
        &owner,
        None,
        Some(canonical),
        canonical
    ));

    let expected = Some(ExpectedSelfTermination {
        flight,
        composition: canonical,
    });
    assert!(expected_self_termination_matches(
        &owner,
        expected,
        Some(canonical),
        canonical
    ));
    assert!(!expected_self_termination_matches(
        &owner,
        expected,
        Some(canonical),
        CompositionIdentity(8)
    ));

    owner.invalidate();
    assert!(!expected_self_termination_matches(
        &owner,
        expected,
        Some(canonical),
        canonical
    ));
}

#[test]
fn failed_write_keeps_a_canonical_handle_when_the_callback_clone_is_consumed() {
    assert_eq!(
        merge_canonical_handle(Some("canonical"), None),
        Some("canonical")
    );
    assert_eq!(
        merge_canonical_handle(Some("canonical"), Some("replacement")),
        Some("replacement")
    );
    assert_eq!(merge_canonical_handle::<&str>(None, None), None);
}

#[test]
fn stale_layout_proposal_keeps_the_newer_subscription_and_retires_only_itself() {
    let newer = "newer subscription";
    let proposed = "older proposal";
    let (retained, retired) = merge_layout_subscription(Some(newer), proposed, false);
    assert_eq!(retained, Some(newer));
    assert_eq!(retired, Some(proposed));
}

#[test]
fn stale_same_context_lease_keeps_the_newer_subscription_and_geometry() {
    let newer_lease = "lease B";
    let stale_lease = "lease A";
    let mut layout = LayoutState {
        phase: GeometryPhase::QueryQueued,
        ..Default::default()
    };

    let (retained_lease, retire_geometry) =
        resolve_same_context_layout_lease(newer_lease, stale_lease, false);
    if retire_geometry {
        layout.retire_geometry_for_lease_rollover();
    }
    assert_eq!(retained_lease, newer_lease);
    assert_eq!(layout.phase, GeometryPhase::QueryQueued);

    let (retained_lease, retire_geometry) =
        resolve_same_context_layout_lease(retained_lease, "lease C", true);
    if retire_geometry {
        layout.retire_geometry_for_lease_rollover();
    }
    assert_eq!(retained_lease, "lease C");
    assert_eq!(layout.phase, GeometryPhase::Idle);
}

#[test]
fn focus_change_during_an_inflight_mutation_revokes_publish_and_marks_unknown() {
    let service = TextService::new();
    let flight = {
        let mut state = service.composition.borrow_mut();
        state.text = "visible before focus loss".to_owned();
        state.write_owner.begin().expect("composition flight")
    };

    service.invalidate_for_focus_change();

    let mut state = service.composition.borrow_mut();
    assert!(!state.known);
    assert!(!state.write_owner.owns(flight));
    // A callback returning after the focus notification cannot finish the
    // revoked flight and therefore cannot republish its local projection.
    assert!(!state.write_owner.finish(flight));
    drop(state);
    assert!(service.input_blocked());
}

#[test]
fn focus_regain_before_deferred_dispatch_keeps_a_known_projection_without_cancelled_output() {
    let service = TextService::new();
    {
        let mut state = service.composition.borrow_mut();
        state.text = "known visible preedit".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
        deferred.dispatch.work.focus_loss = true;
    }

    service.resume_after_focus_gain();

    let state = service.composition.borrow();
    assert!(state.known);
    assert_eq!(state.text, "known visible preedit");
    drop(state);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.dispatch.work.focus_loss);
    assert!(!deferred.focus_reconciliation_required);
    drop(deferred);
    assert!(!service.input_blocked());
}

#[test]
fn focus_regain_after_engine_commit_started_abandons_an_empty_journal() {
    let service = TextService::new();
    {
        let mut writes = service.writes.borrow_mut();
        assert!(writes.activate().is_empty());
        assert!(writes.observe_context(ContextId(31337)).is_empty());
    }
    {
        let mut state = service.composition.borrow_mut();
        state.text = "engine may have committed before this callback".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.dispatch.work.focus_loss = true;
        deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
    }

    service.resume_after_focus_gain();

    let state = service.composition.borrow();
    assert!(state.known);
    assert!(state.text.is_empty());
    assert!(state.handle.is_none());
    assert!(state.context.is_none());
    drop(state);
    let writes = service.writes.borrow();
    assert_eq!(writes.pending_len(), 0);
    assert_eq!(writes.committed_visible(), VisibleState::empty());
    assert_eq!(writes.tail_visible(), VisibleState::empty());
    drop(writes);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.dispatch.work.focus_loss);
    assert!(!service.input_blocked());
}

#[test]
fn rejected_finalizer_after_engine_commit_retires_the_visible_projection() {
    let service = TextService::new();
    {
        let mut state = service.composition.borrow_mut();
        state.text = "visible while RequestEditSession is refused".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
        deferred.dispatch.work.focus_loss = true;
    }

    // This models an outer/session HRESULT refusal after the focus owner
    // already asked the engine to commit. The write payload has already
    // been terminalized by the journal, so terminal cleanup must not rely
    // on it being present.
    service.settle_cancelled_writes(
        vec![Completion::<PendingWrite> {
            outcome: TerminalOutcome::Rejected,
            payload: None,
            ui_lease: None,
        }],
        true,
        None,
    );

    let state = service.composition.borrow();
    assert!(state.known);
    assert!(state.text.is_empty());
    assert!(state.handle.is_none());
    assert!(state.context.is_none());
    drop(state);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.dispatch.work.focus_loss);
    drop(deferred);
    assert!(!service.input_blocked());
}

#[test]
fn reentrant_focus_retirement_gets_one_deferred_retry_then_retires_every_owner() {
    let service = TextService::new();
    {
        let mut writes = service.writes.borrow_mut();
        assert!(writes.activate().is_empty());
        assert!(writes.observe_context(ContextId(31_338)).is_empty());
    }
    {
        let mut composition = service.composition.borrow_mut();
        composition.text = "visible while a re-entrant owner holds the projection".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
        deferred.dispatch.work.focus_loss = true;
        // Model the one hidden-window message already in the queue. The
        // retry must coalesce into this ownership rather than post a
        // second message (and this keeps the test COM-free).
        deferred.dispatch.posted = true;
    }

    // The first retirement attempt is synthetically refused by the active
    // composition borrow. `settle_cancelled_writes` still terminalizes the
    // journal/engine/UI side immediately, then leaves exactly one deferred
    // document-free retry for after this borrow is released.
    let held_composition = service.composition.borrow_mut();
    service.settle_cancelled_writes(
        vec![Completion::<PendingWrite> {
            outcome: TerminalOutcome::Rejected,
            payload: None,
            ui_lease: None,
        }],
        true,
        None,
    );
    service.queue_focus_reconciliation();
    {
        let deferred = service.deferred.borrow();
        assert_eq!(
            deferred.focus_finalization,
            FocusFinalizationPhase::ReconciliationQueued
        );
        assert!(deferred.dispatch.work.focus_reconcile);
        assert!(deferred.dispatch.posted);
        // `terminalize_cancelled_state` explicitly queued candidate UI
        // teardown before the delayed composition retirement.
        assert!(deferred.dispatch.work.end_candidates);
    }
    assert!(service.input_blocked());
    assert_eq!(service.writes.borrow().pending_len(), 0);
    drop(held_composition);

    // Consume the one message's work. A duplicate scheduler invocation
    // above did not create a second focus-reconcile bit/message.
    let dispatched = {
        let mut deferred = service.deferred.borrow_mut();
        let work = deferred
            .dispatch
            .take_for_dispatch(false)
            .expect("the one deferred retry message");
        assert!(work.focus_reconcile);
        assert!(!deferred.dispatch.work.focus_reconcile);
        assert!(!deferred.dispatch.work.has_work());
        work
    };
    assert!(dispatched.end_candidates);
    service.end_candidates();
    service.dispatch_focus_reconciliation();

    let composition = service.composition.borrow();
    assert!(composition.known);
    assert!(composition.text.is_empty());
    assert!(composition.handle.is_none());
    assert!(composition.context.is_none());
    drop(composition);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.focus_reconciliation_required);
    assert!(!deferred.dispatch.work.focus_loss);
    assert!(!deferred.dispatch.work.focus_reconcile);
    drop(deferred);
    let writes = service.writes.borrow();
    assert_eq!(writes.pending_len(), 0);
    assert_eq!(writes.committed_visible(), VisibleState::empty());
    assert_eq!(writes.tail_visible(), VisibleState::empty());
    drop(writes);
    // The engine/UI terminal path is idempotently invoked both before and
    // after the retry, so an unavailable engine cannot be reused here.
    assert!(!service.engine.borrow().is_connected());
    assert!(!service.input_blocked());
}

#[test]
fn borrowed_focus_gain_projection_replaces_pre_engine_finalizer_with_one_retry() {
    let service = TextService::new();
    {
        let mut writes = service.writes.borrow_mut();
        assert!(writes.activate().is_empty());
        assert!(writes.observe_context(ContextId(31_339)).is_empty());
    }
    {
        let mut composition = service.composition.borrow_mut();
        composition.text = "visible while focus gain re-enters a composition callback".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
        deferred.dispatch.work.focus_loss = true;
        // Model the one live hidden-window message. The focus-gain path
        // must reuse this owner rather than append a second retry.
        deferred.dispatch.posted = true;
    }

    // A focus-gain callback re-entering while another composition callback
    // holds this RefCell cannot prove which projection is visible. It must
    // therefore revoke the *pre-engine* focus-loss request before returning
    // instead of leaving it free to call Engine::commit later.
    let held_composition = service.composition.borrow_mut();
    service.resume_after_focus_gain();
    // A second focus notification still has only the existing one-bit
    // reconciliation owner; it cannot post or terminalize a duplicate.
    service.resume_after_focus_gain();
    {
        let deferred = service.deferred.borrow();
        assert_eq!(
            deferred.focus_finalization,
            FocusFinalizationPhase::ReconciliationQueued
        );
        assert!(deferred.focus_reconciliation_required);
        assert!(!deferred.dispatch.work.focus_loss);
        assert!(deferred.dispatch.work.focus_reconcile);
        assert!(deferred.dispatch.work.end_candidates);
        assert!(deferred.dispatch.posted);
    }
    assert!(!service.focus_gain_reconciliation_pending.get());
    assert!(service.input_blocked());
    drop(held_composition);

    let dispatched = {
        let mut deferred = service.deferred.borrow_mut();
        let work = deferred
            .dispatch
            .take_for_dispatch(false)
            .expect("the existing hidden-window message owns the retry");
        assert!(!work.focus_loss);
        assert!(work.focus_reconcile);
        assert!(!deferred.dispatch.work.focus_reconcile);
        assert!(!deferred.dispatch.work.has_work());
        work
    };
    // The original focus-loss finalizer no longer has a state transition it
    // can begin, so it cannot commit the engine after focus has returned.
    assert!(!service.begin_focus_finalization());
    assert!(dispatched.end_candidates);
    service.end_candidates();
    service.dispatch_focus_reconciliation();

    let composition = service.composition.borrow();
    assert!(composition.known);
    assert!(composition.text.is_empty());
    assert!(composition.handle.is_none());
    assert!(composition.context.is_none());
    drop(composition);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.focus_reconciliation_required);
    assert!(!deferred.dispatch.work.focus_loss);
    assert!(!deferred.dispatch.work.focus_reconcile);
    drop(deferred);
    let writes = service.writes.borrow();
    assert_eq!(writes.pending_len(), 0);
    assert_eq!(writes.committed_visible(), VisibleState::empty());
    assert_eq!(writes.tail_visible(), VisibleState::empty());
    drop(writes);
    assert!(!service.input_blocked());
}

#[test]
fn unpostable_focus_reconciliation_becomes_an_explicit_lifecycle_owner() {
    let service = TextService::new();
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::EngineCommitStarted;
    }

    // `TextService::new` has no hidden window. A failed PostMessage path
    // must never leave `ReconciliationQueued` without a message owner.
    service.queue_focus_reconciliation();

    let deferred = service.deferred.borrow();
    assert_eq!(
        deferred.focus_finalization,
        FocusFinalizationPhase::ReconciliationAwaitingLifecycle
    );
    assert!(deferred.focus_reconciliation_required);
    assert!(!deferred.dispatch.work.focus_reconcile);
    assert!(!deferred.dispatch.posted);
    drop(deferred);
    assert!(service.input_blocked());
}

#[test]
fn detach_is_the_terminal_owner_after_destroying_a_reconciliation_message() {
    let service = TextService::new();
    {
        let mut composition = service.composition.borrow_mut();
        composition.text = "projection awaiting lifecycle cleanup".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::ReconciliationQueued;
        deferred.focus_reconciliation_required = true;
        deferred.dispatch.work.focus_reconcile = true;
    }

    // `destroy_deferred_window` first transfers the lost message to the
    // lifecycle phase; `detach` then owns the document-free retirement and
    // reports a failure if that final attempt cannot acquire the state.
    service
        .detach()
        .expect("lifecycle cleanup retires the projection");

    let composition = service.composition.borrow();
    assert!(composition.known);
    assert!(composition.text.is_empty());
    assert!(composition.handle.is_none());
    drop(composition);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.focus_reconciliation_required);
    assert!(!deferred.dispatch.work.focus_reconcile);
    drop(deferred);
    assert!(!service.input_blocked());
}

#[test]
fn cancelled_accepted_output_forces_pre_dispatch_focus_reconciliation() {
    let reservation_only = Completion {
        outcome: TerminalOutcome::Cancelled(CancelReason::FocusChanged),
        payload: None::<()>,
        ui_lease: None,
    };
    let accepted_output = Completion {
        outcome: TerminalOutcome::Cancelled(CancelReason::FocusChanged),
        payload: Some(()),
        ui_lease: None,
    };
    assert!(!cancelled_outputs_require_focus_reconciliation(&[
        reservation_only
    ]));
    assert!(cancelled_outputs_require_focus_reconciliation(&[
        accepted_output
    ]));

    let service = TextService::new();
    {
        let mut state = service.composition.borrow_mut();
        // The projection is locally known, but the accepted output that
        // was cancelled at the focus boundary may already have advanced
        // the engine without reaching the document.
        state.text = "known only on this side".to_owned();
    }
    {
        let mut deferred = service.deferred.borrow_mut();
        deferred.focus_finalization = FocusFinalizationPhase::DeferredQueued;
        deferred.dispatch.work.focus_loss = true;
    }
    service.require_focus_reconciliation();

    service.resume_after_focus_gain();

    let state = service.composition.borrow();
    assert!(state.known);
    assert!(state.text.is_empty());
    assert!(state.handle.is_none());
    assert!(state.context.is_none());
    drop(state);
    let deferred = service.deferred.borrow();
    assert_eq!(deferred.focus_finalization, FocusFinalizationPhase::Idle);
    assert!(!deferred.focus_reconciliation_required);
    assert!(!deferred.dispatch.work.focus_loss);
    drop(deferred);
    assert!(!service.input_blocked());
}

#[test]
fn focus_change_without_an_inflight_mutation_keeps_known_text_for_finalization() {
    let service = TextService::new();
    {
        let mut state = service.composition.borrow_mut();
        state.text = "known visible preedit".to_owned();
    }

    service.invalidate_for_focus_change();

    let state = service.composition.borrow();
    assert!(state.known);
    assert_eq!(state.text, "known visible preedit");
}

#[test]
fn candidate_operation_holds_reentrant_deferred_work_until_controller_restore() {
    let mut dispatch = DeferredDispatchState {
        posted: true,
        work: DeferredWork {
            write: true,
            layout: true,
            layout_abandon: true,
            focus_loss: false,
            focus_reconcile: false,
            end_candidates: true,
            candidates: Some("newer candidates"),
        },
    };

    // The nested hidden-window message consumes its post but leaves every
    // kind of work in place while the outer controller is out of its slot.
    assert!(dispatch.take_for_dispatch(true).is_none());
    assert!(!dispatch.posted);
    assert!(dispatch.work.write);
    assert!(dispatch.work.layout);
    assert!(dispatch.work.layout_abandon);
    assert!(dispatch.work.end_candidates);
    assert_eq!(dispatch.work.candidates, Some("newer candidates"));

    // Once the controller is restored, exactly one replacement post is
    // needed. The next dispatch drains the retained work normally.
    assert!(dispatch.needs_repost_after_candidate_operation());
    dispatch.posted = true;
    let drained = dispatch.take_for_dispatch(false).expect("deferred work");
    assert!(drained.write);
    assert!(drained.layout);
    assert!(drained.layout_abandon);
    assert!(drained.end_candidates);
    assert_eq!(drained.candidates, Some("newer candidates"));
    assert!(!dispatch.work.has_work());
}

#[test]
fn candidate_operation_does_not_post_again_when_work_already_has_a_message() {
    let dispatch: DeferredDispatchState<()> = DeferredDispatchState {
        posted: true,
        work: DeferredWork {
            write: true,
            ..Default::default()
        },
    };

    assert!(!dispatch.needs_repost_after_candidate_operation());
    assert!(dispatch.posted);
    assert!(dispatch.work.write);
}

#[derive(Debug)]
struct CandidateTeardownProbe {
    active: Option<&'static str>,
    old_end_count: usize,
    newer_end_count: usize,
}

impl CandidateTeardownProbe {
    fn end_old(&mut self) {
        assert_eq!(self.active.take(), Some("old"));
        self.old_end_count += 1;
    }

    fn install_and_end_newer(&mut self, candidate: &'static str) {
        assert!(self.active.replace(candidate).is_none());
        assert_eq!(self.active.take(), Some("newer"));
        self.newer_end_count += 1;
    }
}

fn assert_newer_candidate_survives_teardown_reentry(
    reenter_during_unadvise: bool,
) -> CandidateTeardownProbe {
    let dispatch = RefCell::new(DeferredDispatchState {
        posted: true,
        work: DeferredWork {
            candidates: Some("newer"),
            ..Default::default()
        },
    });
    let mut controller = CandidateTeardownProbe {
        active: Some("old"),
        old_end_count: 0,
        newer_end_count: 0,
    };
    let nested_dispatch = || {
        assert!(dispatch.borrow_mut().take_for_dispatch(true).is_none());
        assert_eq!(dispatch.borrow().work.candidates, Some("newer"));
    };

    run_candidate_teardown_host_calls(
        Some(()),
        &mut controller,
        |_| {
            if reenter_during_unadvise {
                nested_dispatch();
            }
        },
        |controller| {
            controller.end_old();
            if !reenter_during_unadvise {
                nested_dispatch();
            }
        },
    );

    let mut dispatch = dispatch.borrow_mut();
    assert!(dispatch.needs_repost_after_candidate_operation());
    dispatch.posted = true;
    let newer = dispatch
        .take_for_dispatch(false)
        .expect("retained newer candidate")
        .candidates
        .expect("newer candidate payload");
    drop(dispatch);
    controller.install_and_end_newer(newer);
    controller
}

#[test]
fn newer_candidate_survives_reentry_during_subscription_unadvise() {
    let controller = assert_newer_candidate_survives_teardown_reentry(true);
    assert_eq!(controller.old_end_count, 1);
    assert_eq!(controller.newer_end_count, 1);
    assert!(controller.active.is_none());
}

#[test]
fn newer_candidate_survives_reentry_during_end_ui_element() {
    let controller = assert_newer_candidate_survives_teardown_reentry(false);
    assert_eq!(controller.old_end_count, 1);
    assert_eq!(controller.newer_end_count, 1);
    assert!(controller.active.is_none());
}

#[test]
fn stale_geometry_is_abandoned_and_a_new_lease_starts_from_idle() {
    let mut layout = LayoutState {
        phase: GeometryPhase::QueryQueued,
        ..Default::default()
    };
    layout.retire_geometry_for_lease_rollover();
    assert_eq!(layout.phase, GeometryPhase::Idle);
}

#[test]
fn matching_layout_abandonment_has_an_explicit_terminal_phase() {
    let claim = layout_claim(ContextId(41_001));
    let mut phase = GeometryPhase::QueryQueued;

    abandon_matching_geometry(&mut phase, Some(claim), claim);

    assert_eq!(phase, GeometryPhase::Unavailable);
}

#[test]
fn refused_layout_abandonment_remains_owned_until_one_later_attempt() {
    let service = TextService::new();
    let claim = layout_claim(ContextId(41_002));
    service.layout_abandon_pending.set(Some(claim));

    let borrowed = service.layout.borrow_mut();
    assert!(service.settle_pending_layout_abandon().is_err());
    assert_eq!(service.layout_abandon_pending.get(), Some(claim));
    drop(borrowed);

    assert_eq!(
        service
            .settle_pending_layout_abandon()
            .expect("later lifecycle attempt"),
        Some(false)
    );
    assert_eq!(service.layout_abandon_pending.get(), None);
    assert_eq!(
        service
            .settle_pending_layout_abandon()
            .expect("no duplicate attempt"),
        None
    );
}

/// The engine emits the normalized kana and the romaji still being
/// typed as separate segments; the user sees one run of text.
#[test]
fn segments_are_shown_as_one_run() {
    assert_eq!(visible_text(&preedit(&["か", "t"])), "かt");
    assert_eq!(visible_text(&preedit(&[])), "");
}

#[test]
fn a_preedit_is_planned_without_mutating_composition_state() {
    let service = TextService::new();
    let plan = service.plan(&output(None, Some(&["か"]))).expect("plan");
    assert!(
        matches!(plan.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "か")
    );
    assert_eq!(plan.before, VisibleState::empty());
    assert_eq!(plan.after, state("か", true));
    assert_eq!(
        service.composition_projection().expect("projection"),
        VisibleState::empty()
    );
}

/// The engine's per-segment `UnderlineKind` -- raw input, a converted
/// clause, or the one clause currently focused -- has to survive the
/// trip from `Output` into the `Update::Show` the document write
/// actually applies. Losing it here (e.g. by flattening to a string
/// before planning, as this function used to) would draw every clause
/// with the same underline no matter how the engine tagged it.
#[test]
fn plan_carries_each_segments_underline_kind_into_the_show_update() {
    let mut answer = output(None, None);
    answer.preedit = Some(preedit_with_underlines(&[
        ("わたし", UnderlineKind::Converted),
        ("は", UnderlineKind::Focused),
        ("にほん", UnderlineKind::Raw),
    ]));

    let plan = plan_from_visible(VisibleState::empty(), &answer).expect("plan");

    match plan.updates.as_slice() {
        [Update::Show(segments)] => {
            let [converted, focused, raw] = segments.as_slice() else {
                panic!("expected three preedit segments, got {segments:?}");
            };
            assert_eq!(converted.text, "わたし");
            assert_eq!(converted.underline, UnderlineKind::Converted);
            assert_eq!(focused.text, "は");
            assert_eq!(focused.underline, UnderlineKind::Focused);
            assert_eq!(raw.text, "にほん");
            assert_eq!(raw.underline, UnderlineKind::Raw);
        }
        other => panic!("expected one multi-segment show, got {other:?}"),
    }
    assert_eq!(plan.after, state("わたしはにほん", true));
}

/// Enter mid-word: the converted text is committed and the tail the
/// engine is still working on stays underlined. The commit has to come
/// first, because the new composition starts where its text ended.
#[test]
fn a_commit_with_a_tail_ends_one_composition_and_opens_another() {
    let plan =
        plan_from_visible(state("かな", true), &output(Some("漢字"), Some(&["か"]))).expect("plan");
    match plan.updates.as_slice() {
        [Update::Commit(committed), Update::Show(shown)] => {
            assert_eq!(committed, "漢字");
            assert_eq!(segments_text(shown), "か");
        }
        other => panic!("expected a commit then a show, got {other:?}"),
    }
    assert_eq!(plan.before, state("かな", true));
    assert_eq!(plan.after, state("か", true));
}

/// Escape: nothing committed, nothing left to show, and something on
/// screen that has to come off it.
#[test]
fn an_empty_answer_discards_what_is_on_screen() {
    let plan = plan_from_visible(state("か", true), &output(None, None)).expect("plan");
    assert!(matches!(plan.updates.as_slice(), [Update::Discard]));
    assert_eq!(plan.after, VisibleState::empty());
}

/// The idle case, and by far the most common one: a key that touched no
/// composition must not cost the document an edit session.
#[test]
fn an_empty_answer_with_nothing_on_screen_does_nothing() {
    let plan = plan_from_visible(VisibleState::empty(), &output(None, None)).expect("plan");
    assert!(plan.updates.is_empty());
    assert_eq!(plan.after, VisibleState::empty());
}

/// A commit that empties the preedit must not also emit a discard: the
/// commit already closed the composition, and discarding afterwards
/// would reopen and clear one.
#[test]
fn a_plain_commit_is_a_single_operation() {
    let plan = plan_from_visible(state("か", true), &output(Some("か"), None)).expect("plan");
    assert!(matches!(plan.updates.as_slice(), [Update::Commit(text)] if text == "か"));
    assert_eq!(plan.after, VisibleState::empty());
}

#[test]
fn idle_space_commit_does_not_replace_a_live_reading() {
    let before = state("にほんごにゅうりょくのてすと", true);
    let plan = plan_from_visible(before.clone(), &output(Some("\u{3000}"), None)).expect("plan");
    assert!(plan.updates.is_empty());
    assert_eq!(plan.after, before);
    let ascii = plan_from_visible(state("にほんご", true), &output(Some(" "), None)).expect("plan");
    assert!(ascii.updates.is_empty());
    assert_eq!(ascii.after.text, "にほんご");
}

#[test]
fn commit_undo_deletes_the_committed_run_before_restoring_preedit() {
    let mut restored = output(None, Some(&["かな"]));
    restored.delete_before = "加奈".to_owned();

    let plan = plan_from_visible(VisibleState::empty(), &restored).expect("commit undo plan");

    assert!(matches!(
        plan.updates.as_slice(),
        [Update::DeleteBefore(text), Update::Show(shown)]
            if text == "加奈" && segments_text(shown) == "かな"
    ));
    assert_eq!(plan.after, state("かな", true));
}

#[test]
fn commit_undo_malformed_is_rejected_without_mutating_visible_state() {
    let composing = state("編集中", true);
    let mut while_composing = output(None, Some(&["復元"]));
    while_composing.delete_before = "行った".to_owned();
    assert!(plan_from_visible(composing.clone(), &while_composing).is_err());
    assert_eq!(composing, state("編集中", true));

    let mut with_commit = output(Some("競合"), None);
    with_commit.delete_before = "加奈".to_owned();
    assert!(plan_from_visible(VisibleState::empty(), &with_commit).is_err());
}

#[test]
fn shift_latin_backspace_retype_plans_chain_to_aiueo_not_aiuoeo() {
    let typed = plan_from_visible(VisibleState::empty(), &output(None, Some(&["AIUEO"])))
        .expect("type AIUEO");
    assert!(
        matches!(typed.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUEO")
    );
    let erased = plan_from_visible(typed.after.clone(), &output(None, Some(&["AIUE"])))
        .expect("Shift+Backspace");
    assert!(
        matches!(erased.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUE")
    );
    let retyped =
        plan_from_visible(erased.after.clone(), &output(None, Some(&["AIUEO"]))).expect("retype O");
    assert!(
        matches!(retyped.updates.as_slice(), [Update::Show(segments)] if segments_text(segments) == "AIUEO")
    );
    assert_ne!(retyped.after.text, "AIUOEO");
    assert_eq!(retyped.after, state("AIUEO", true));
}

#[test]
fn plans_chain_through_explicit_projections() {
    let first = plan_from_visible(VisibleState::empty(), &output(None, Some(&["かん"])))
        .expect("first plan");
    let second =
        plan_from_visible(first.after.clone(), &output(Some("かん"), None)).expect("second plan");
    assert_eq!(first.before, VisibleState::empty());
    assert_eq!(first.after, state("かん", true));
    assert!(matches!(second.updates.as_slice(), [Update::Commit(text)] if text == "かん"));
    assert_eq!(second.after, VisibleState::empty());
}

#[test]
fn function_provider_exposes_the_reconversion_interface() {
    let service: IUnknown = TextService::new().into();
    let provider: ITfFunctionProvider = service.cast().expect("function provider");
    // SAFETY: both GUID pointers are live for the call and the returned
    // object is retained by the interface wrapper.
    let function = unsafe {
        provider
            .GetFunction(&GUID::from_u128(0), &ITfFnReconversion::IID)
            .expect("reconversion function")
    };
    let _: ITfFnReconversion = function.cast().expect("ITfFnReconversion");
    assert_eq!(
        // SAFETY: `provider` is a live COM interface and the call has no
        // borrowed output pointer beyond its returned value.
        unsafe { provider.GetType().expect("type") },
        CLSID_SAKURA_TSF
    );
    assert_eq!(
        // SAFETY: `provider` is live and the returned BSTR is owned by its
        // interface wrapper.
        unsafe { provider.GetDescription().expect("description") }.to_string(),
        TEXT_SERVICE_DESCRIPTION
    );
}

#[test]
fn input_mode_item_exposes_a_split_menu_button_to_tsf() {
    let service: IUnknown = TextService::new().into();
    let item: ITfLangBarItem = service.cast().expect("language-bar item");
    let mut info = TF_LANGBARITEMINFO::default();

    // SAFETY: `item` is a live in-process COM object and `info` is a
    // writable output structure for the duration of this call.
    unsafe { item.GetInfo(&mut info).expect("language-bar info") };

    assert_eq!(info.guidItem, GUID_LBI_INPUTMODE);
    assert_ne!(info.dwStyle & TF_LBI_STYLE_BTN_BUTTON, 0);
    assert_ne!(info.dwStyle & TF_LBI_STYLE_BTN_MENU, 0);
}
