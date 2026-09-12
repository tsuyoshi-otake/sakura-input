use super::*;
use sakura_proto::ErrorCode;

#[test]
fn a_new_session_starts_idle_in_hiragana_mode_with_normal_scope() {
    let mut session = Session::new("notepad.exe");
    assert_eq!(session.process_name(), "notepad.exe");
    assert_eq!(session.host_policy(), HostPolicy::Ordinary);
    assert_eq!(session.mode(), Mode::Hiragana);
    assert_eq!(session.scope(), InputScope::Normal);
    assert_eq!(session.state(), State::Idle);
    assert!(!session.is_composing());
    assert!(
        session.idle_space_is_full(false),
        "Hiragana idle Space starts full-width"
    );
    session.record_current_commit("Claude", 0, 0, 1);
    assert!(
        !session.idle_space_is_full(false),
        "an ASCII commit must not be followed by an ideographic Space"
    );
    session.record_current_commit("今日", 0, 0, 1);
    assert!(
        session.idle_space_is_full(false),
        "a Japanese commit restores ideographic idle Space"
    );
}

#[test]
fn renderer_basename_fixes_private_policy_independent_of_normal_scope() {
    let mut session = Session::new(SAKURA_RENDERER_PROCESS_NAME);
    assert_eq!(session.host_policy(), HostPolicy::PrivateRendererUi);
    assert!(session.host_policy().is_private_renderer_ui());

    // A renderer-owned Pad is normally classified as ordinary text for
    // conversion. Even after a classification gap, re-publishing Normal
    // must not relax the host policy.
    session.apply_input_scope(InputScope::Unclassified);
    session.apply_input_scope(InputScope::Normal);
    assert_eq!(session.scope(), InputScope::Normal);
    assert_eq!(session.host_policy(), HostPolicy::PrivateRendererUi);
    assert!(!session.host_policy().allows_persistence());
    assert!(session.host_policy().allows_local_worker());
    assert!(!session.host_policy().allows_ai_text());
}

#[test]
fn only_the_exact_renderer_basename_is_private() {
    for name in ["SAKURA_RENDERER.EXE", "sakura_renderer.exe"] {
        assert_eq!(
            HostPolicy::from_process_name(name),
            HostPolicy::PrivateRendererUi,
            "Windows basename matching is case-insensitive for {name}"
        );
    }
    for name in [
        "sakura_renderer.exe.bak",
        "C:\\Program Files\\Sakura Input\\sakura_renderer.exe",
        "not_sakura_renderer.exe",
        "sakura_renderer",
    ] {
        assert_eq!(
            HostPolicy::from_process_name(name),
            HostPolicy::Ordinary,
            "non-basename {name:?} must not gain the renderer policy"
        );
    }
}

#[test]
fn runtime_preferences_update_live_policy_without_resetting_mode_or_text() {
    let mut table = SessionTable::new();
    let id = table.create("notepad.exe").expect("session");
    let session = table.get_mut(id).expect("live session");
    session.mode = Mode::Katakana;
    session.preedit.push_str("かな").expect("preedit");

    let mut normalizer = Normalizer::default();
    normalizer.width.alnum = sakura_core::Width::Full;
    let preferences = Preferences {
        normalizer,
        prediction_enabled: false,
        association_enabled: false,
        suggest_accept: SuggestAccept::ShiftEnter,
        ..Preferences::default()
    };
    table.apply_runtime_preferences(preferences, &[], false);

    let session = table.get(id).expect("live session");
    assert_eq!(session.mode, Mode::Katakana);
    assert_eq!(session.preedit.as_str(), "かな");
    assert_eq!(session.normalizer.width.alnum, sakura_core::Width::Full);
    assert!(!session.prediction_enabled);
    assert_eq!(session.suggest_accept, SuggestAccept::ShiftEnter);
    assert!(!session.association_enabled);
}

#[test]
fn an_oversized_process_name_is_truncated_on_a_char_boundary_not_rejected() {
    // Every character is 3 bytes, so the exact byte cap lands
    // mid-character; truncation must back off to the char before it
    // rather than split it or panic.
    let long = "あ".repeat(MAX_PROCESS_NAME_BYTES); // MAX_PROCESS_NAME_BYTES / 3 chars short of MAX_PROCESS_NAME_BYTES * 3 bytes
    let session = Session::new(&long);
    assert!(session.process_name().len() <= MAX_PROCESS_NAME_BYTES);
    assert!(long.starts_with(session.process_name()));
    assert!(session
        .process_name()
        .is_char_boundary(session.process_name().len()));
}

#[test]
fn reset_clears_the_composition_but_not_mode_or_scope() {
    let mut session = Session::new("cmd.exe");
    session.mode = Mode::Katakana;
    session.scope = InputScope::Url;
    session.preedit.push_str("か").expect("fits");
    assert!(session.is_composing());

    session.reset();

    assert!(!session.is_composing());
    assert_eq!(session.state(), State::Idle);
    assert_eq!(session.mode(), Mode::Katakana);
    assert_eq!(session.scope(), InputScope::Url);
}

#[test]
fn pending_romaji_alone_counts_as_composing() {
    // "k" alone never resolves on its own (it waits for a vowel), so
    // feeding it leaves `preedit` empty and only `romaji` pending --
    // exactly the case `is_composing`/`state` must not miss.
    let table = romaji::Table::builtin().expect("the shipped romaji table compiles");
    let mut session = Session::new("cmd.exe");
    table
        .feed(&mut session.romaji, 'k', &mut session.preedit)
        .expect("fits");
    assert!(session.preedit.is_empty());
    assert!(!session.romaji.is_empty());
    assert!(session.is_composing());
    assert_eq!(session.state(), State::Composing);
}

#[test]
fn segment_resize_changes_only_the_focused_boundary_and_is_reversible() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("あいうえおかきく").expect("fits");
    let segments = [6u16, 12, 18, 24].map(|reading_end| ConversionSegment {
        reading_end,
        ..ConversionSegment::default()
    });
    assert!(session.set_segments(&segments));
    session.converting = true;
    session.focus_next_segment();
    assert_eq!(session.focused_segment(), 1);

    assert!(session.resize_focused_segment(true));
    assert_eq!(session.segment_range(0), Some(0..6));
    assert_eq!(session.segment_range(1), Some(6..15));
    assert_eq!(session.segment_range(2), Some(15..18));
    assert_eq!(session.segment_range(3), Some(18..24));

    assert!(session.resize_focused_segment(false));
    assert_eq!(session.segment_range(0), Some(0..6));
    assert_eq!(session.segment_range(1), Some(6..12));
    assert_eq!(session.segment_range(2), Some(12..18));
    assert_eq!(session.segment_range(3), Some(18..24));
}

#[test]
fn resize_focused_segment_splits_a_single_bunsetsu_conversion_on_shrink() {
    // The common case this fixes: the engine guessed the whole reading
    // as one segment, and Shift+Left must still be able to re-cut it.
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("あいう").expect("fits");
    let segments = [ConversionSegment {
        reading_end: 9,
        ..ConversionSegment::default()
    }];
    assert!(session.set_segments(&segments));
    session.converting = true;
    assert_eq!(session.segment_count(), 1);
    assert_eq!(session.focused_segment(), 0);

    // Growing the only segment has nothing past the conversion's end to
    // absorb, so it must keep refusing exactly as it did before this
    // segment could ever split.
    assert!(!session.resize_focused_segment(true));
    assert_eq!(session.segment_count(), 1);

    assert!(session.resize_focused_segment(false));
    assert_eq!(session.segment_count(), 2);
    assert_eq!(session.segment_range(0), Some(0..6));
    assert_eq!(session.segment_range(1), Some(6..9));
    // Both boundaries land on character boundaries -- "う" (3 bytes)
    // stays intact in the new trailing segment, never split in half.
    assert!(session.preedit.as_str().is_char_boundary(6));
    assert!(session.preedit.as_str().is_char_boundary(9));
    assert_eq!(&session.preedit.as_str()[0..6], "あい");
    assert_eq!(&session.preedit.as_str()[6..9], "う");
    // Focus stays on the segment that was resized, matching the
    // boundary-slide branch, which never moves focus either.
    assert_eq!(session.focused_segment(), 0);
    assert_eq!(session.segment_selection(0), 0);
    assert_eq!(session.segment_selection(1), 0);
    assert_eq!(session.segment_transform(0), (SegmentTransform::None, 0));
    assert_eq!(session.segment_transform(1), (SegmentTransform::None, 0));
}

#[test]
fn resize_focused_segment_refuses_to_shrink_a_one_character_segment() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("あ").expect("fits");
    let segments = [ConversionSegment {
        reading_end: 3,
        ..ConversionSegment::default()
    }];
    assert!(session.set_segments(&segments));
    session.converting = true;

    assert!(!session.resize_focused_segment(false));
    assert_eq!(session.segment_count(), 1);
    assert_eq!(session.segment_range(0), Some(0..3));
}

#[test]
fn resize_focused_segment_splits_the_trailing_segment_of_a_multi_segment_conversion() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("あいうえおかきく").expect("fits");
    let segments = [6u16, 12, 18, 24].map(|reading_end| ConversionSegment {
        reading_end,
        ..ConversionSegment::default()
    });
    assert!(session.set_segments(&segments));
    session.converting = true;
    session.focus_next_segment();
    session.focus_next_segment();
    session.focus_next_segment();
    assert_eq!(session.focused_segment(), 3);

    // Growing the trailing segment still refuses: there is nothing past
    // the conversion's end for it to absorb.
    assert!(!session.resize_focused_segment(true));
    assert_eq!(session.segment_count(), 4);

    assert!(session.resize_focused_segment(false));
    assert_eq!(session.segment_count(), 5);
    assert_eq!(session.segment_range(0), Some(0..6));
    assert_eq!(session.segment_range(1), Some(6..12));
    assert_eq!(session.segment_range(2), Some(12..18));
    assert_eq!(session.segment_range(3), Some(18..21));
    assert_eq!(session.segment_range(4), Some(21..24));
    assert!(session.preedit.as_str().is_char_boundary(21));
    assert_eq!(&session.preedit.as_str()[18..21], "き");
    assert_eq!(&session.preedit.as_str()[21..24], "く");
    // Earlier boundaries (0..18) are untouched, matching the
    // boundary-slide branch's "every other boundary stays pinned"
    // guarantee.
    assert_eq!(session.focused_segment(), 3);
    assert_eq!(session.segment_selection(4), 0);
    assert_eq!(session.segment_transform(4), (SegmentTransform::None, 0));
}

#[test]
fn resize_focused_segment_refuses_to_split_once_segment_count_is_at_capacity() {
    // MAX_SEGMENTS - 1 one-character segments, plus a trailing
    // two-character segment: exactly MAX_SEGMENTS segments already, with
    // the trailing one otherwise splittable on every ground except room.
    let mut session = Session::new("editor.exe");
    let mut reading = String::new();
    let mut ends = Vec::new();
    for _ in 0..MAX_SEGMENTS - 1 {
        reading.push('x');
        ends.push(u16::try_from(reading.len()).expect("fits"));
    }
    reading.push_str("yz");
    ends.push(u16::try_from(reading.len()).expect("fits"));
    session.preedit.push_str(&reading).expect("fits");
    let segments: Vec<ConversionSegment> = ends
        .into_iter()
        .map(|reading_end| ConversionSegment {
            reading_end,
            ..ConversionSegment::default()
        })
        .collect();
    assert!(session.set_segments(&segments));
    session.converting = true;
    assert_eq!(session.segment_count(), MAX_SEGMENTS);
    for _ in 0..MAX_SEGMENTS - 1 {
        session.focus_next_segment();
    }
    assert_eq!(session.focused_segment(), MAX_SEGMENTS - 1);

    assert!(!session.resize_focused_segment(false));
    assert_eq!(session.segment_count(), MAX_SEGMENTS);
}

#[test]
fn commit_undo_cache_restores_reading_and_rolls_back_context() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("かな").expect("fits");
    session.raw_input.push_str("kana").expect("fits");
    session.record_current_commit("加奈", 42, 0, 1);
    let cached = session.cached_surface_fingerprint("かな");
    assert_eq!(cached, Some((text_hash("加奈"), 6)));
    assert_eq!(session.carry_right_id(), 42);
    session.reset();

    assert!(session.undo_commit().is_some());
    assert_eq!(session.preedit.as_str(), "かな");
    assert_eq!(session.raw_input.as_str(), "kana");
    assert_eq!(session.carry_right_id(), 0);
    assert_eq!(
        session.cached_surface_fingerprint("かな"),
        cached,
        "the recency entry remains live until the host acknowledges deletion"
    );
    assert!(session.reject_undo_commit());
    assert_eq!(session.carry_right_id(), 42);
    assert!(session.undo_commit().is_some());
    assert!(session.acknowledge_undo_commit());
    assert_eq!(session.cached_surface_fingerprint("かな"), None);
    assert_eq!(session.undo_commit(), None, "undo depth is exactly one");
}

fn test_cross_commit_bridge() -> SessionCrossCommitBridge {
    SessionCrossCommitBridge::new("もれ", "漏れ", 1841, 4_000).expect("bounded bridge")
}

#[test]
fn cross_commit_bridge_is_volatile_normal_scope_state() {
    let mut session = Session::new("editor.exe");
    session.apply_input_scope(InputScope::Normal);
    session.preedit.push_str("こうりょもれ").expect("fits");
    session.record_current_commit_with_bridge(
        "考慮漏れ",
        1949,
        0,
        2,
        Some(test_cross_commit_bridge()),
    );
    let bridge = session.cross_commit_bridge().expect("stored bridge");
    assert_eq!(bridge.tail_reading, "もれ");
    assert_eq!(bridge.tail_surface, "漏れ");
    assert_eq!(bridge.prefix_right_id.raw(), 1841);
    assert_eq!(bridge.prefix_cost, 4_000);

    // Ending the composition preserves immediate adjacency in the same
    // positively classified context.
    session.reset();
    assert!(session.cross_commit_bridge().is_some());

    // Classification uncertainty can later become Normal again; clearing
    // now prevents the old text from reviving across that gap.
    session.apply_input_scope(InputScope::Unclassified);
    assert!(session.cross_commit_bridge().is_none());
    session.apply_input_scope(InputScope::Normal);
    assert!(session.cross_commit_bridge().is_none());
}

#[test]
fn bridge_rejects_nonadjacent_or_unsupported_commit_boundaries() {
    let mut session = Session::new("editor.exe");
    session.apply_input_scope(InputScope::Normal);

    session.record_current_commit_with_bridge(
        "考慮漏れ、",
        1949,
        0,
        2,
        Some(test_cross_commit_bridge()),
    );
    assert!(session.cross_commit_bridge().is_none(), "clause boundary");

    session.record_current_commit_with_bridge(
        "different",
        1949,
        0,
        1,
        Some(test_cross_commit_bridge()),
    );
    assert!(session.cross_commit_bridge().is_none(), "surface mismatch");

    session.association_enabled = false;
    session.record_current_commit_with_bridge(
        "考慮漏れ",
        1949,
        0,
        2,
        Some(test_cross_commit_bridge()),
    );
    assert!(session.cross_commit_bridge().is_none(), "association off");

    for scope in [
        InputScope::Password,
        InputScope::Url,
        InputScope::Email,
        InputScope::Digits,
    ] {
        let mut sensitive = Session::new("editor.exe");
        sensitive.apply_input_scope(InputScope::Normal);
        sensitive.record_current_commit_with_bridge(
            "考慮漏れ",
            1949,
            0,
            2,
            Some(test_cross_commit_bridge()),
        );
        assert!(sensitive.cross_commit_bridge().is_some());
        sensitive.apply_input_scope(scope);
        sensitive.record_current_commit_with_bridge(
            "考慮漏れ",
            1949,
            0,
            2,
            Some(test_cross_commit_bridge()),
        );
        assert!(
            sensitive.cross_commit_bridge().is_none(),
            "{scope:?} must neither retain nor expose bridge text"
        );
    }

    let mut unknown = Session::new("editor.exe");
    unknown.apply_input_scope(InputScope::Normal);
    unknown.record_current_commit_with_bridge(
        "考慮漏れ",
        1949,
        0,
        2,
        Some(test_cross_commit_bridge()),
    );
    unknown.apply_input_scope(InputScope::Unclassified);
    assert!(unknown.cross_commit_bridge().is_none(), "unknown scope");

    assert!(SessionCrossCommitBridge::new("の", "の", 1841, 4_000).is_none());
    assert!(is_cross_commit_boundary("考慮漏れ "));
    assert!(is_cross_commit_boundary("考慮漏れ\n"));
    assert!(is_cross_commit_boundary("考慮漏れ!"));
    assert!(is_cross_commit_boundary("考慮漏れ?"));
    assert!(!is_cross_commit_boundary("考慮漏れ"));
}

#[test]
fn cross_commit_bridge_is_isolated_per_session() {
    let mut table = SessionTable::new();
    let first = table.create("editor.exe").expect("first session");
    let second = table.create("editor.exe").expect("second session");

    let first_session = table.get_mut(first).expect("first live session");
    first_session.apply_input_scope(InputScope::Normal);
    first_session.record_current_commit_with_bridge(
        "考慮漏れ",
        1949,
        0,
        2,
        Some(test_cross_commit_bridge()),
    );

    assert!(table
        .get(first)
        .expect("first live session")
        .cross_commit_bridge()
        .is_some());
    assert!(table
        .get(second)
        .expect("second live session")
        .cross_commit_bridge()
        .is_none());

    table
        .get_mut(second)
        .expect("second live session")
        .reset_carryover();
    assert!(table
        .get(first)
        .expect("first live session")
        .cross_commit_bridge()
        .is_some());
}

#[test]
fn undo_and_carry_reset_clear_cross_commit_bridge() {
    let mut session = Session::new("editor.exe");
    session.apply_input_scope(InputScope::Normal);
    session.preedit.push_str("もれ").expect("fits");
    session.record_current_commit_with_bridge("漏れ", 1949, 0, 1, Some(test_cross_commit_bridge()));
    session.reset();
    assert!(session.cross_commit_bridge().is_some());
    assert!(session.undo_commit().is_some());
    assert!(session.cross_commit_bridge().is_none());
    assert!(session.reject_undo_commit());
    assert!(
        session.cross_commit_bridge().is_none(),
        "a rejected host deletion must not revive implicit text context"
    );

    session.reset();
    session.preedit.push_str("もれ").expect("fits");
    session.record_current_commit_with_bridge("漏れ", 1949, 0, 1, Some(test_cross_commit_bridge()));
    assert!(session.cross_commit_bridge().is_some());
    session.reset_carryover();
    assert!(session.cross_commit_bridge().is_none());
}

#[test]
fn commit_undo_rejection_and_ack_keep_engine_and_document_terminal_states_aligned() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("reading").expect("fits");
    session.raw_input.push_str("raw").expect("fits");
    session.record_current_commit("committed", 7, 0, 1);
    session.reset();

    // A moved caret/text mismatch is a host rejection: the simulated
    // document is untouched, while the engine returns to its exact
    // post-commit idle state and keeps the bounded undo record.
    let mut document = String::from("prefixother");
    let expected = session.undo_commit().expect("undo is pending");
    assert_eq!(expected.as_str(), "committed");
    assert!(session.undo_pending());
    if !document.ends_with(expected.as_str()) {
        assert!(session.reject_undo_commit());
    }
    assert_eq!(document, "prefixother");
    assert!(!session.undo_pending());
    assert!(!session.is_composing());
    assert_eq!(session.carry_right_id(), 7);
    assert!(session.undo_commit().is_some(), "rejection preserves undo");

    // Exact verification succeeds: only then does the simulated host
    // delete and the engine consume its record/cache entry.
    let expected = session.undo_record.surface.as_str().to_owned();
    document.push_str("committed");
    assert!(document.ends_with(&expected));
    document.truncate(document.len() - expected.len());
    assert!(session.acknowledge_undo_commit());
    assert_eq!(document, "prefixother");
    assert!(!session.undo_pending());
    assert!(session.is_composing());
    assert_eq!(session.cached_surface_fingerprint("reading"), None);
    assert!(session.undo_commit().is_none(), "ack consumes undo");
}

#[test]
fn commit_undo_wrapped_commit_history_uses_only_the_live_ring_window() {
    let mut session = Session::new("editor.exe");
    for _ in 0..COMMIT_CACHE_CAPACITY {
        session.preedit.push_str("いった").expect("fits");
        session.record_current_commit("言った", 1, 0, 1);
        session.reset();
    }
    for _ in 0..4 {
        session.preedit.push_str("かんすう").expect("fits");
        session.record_current_commit("関数", 2, 1, 1);
        session.reset();
    }

    assert_eq!(
        session.domain_it_ratio_per_mille(),
        u16::try_from(4_000 / COMMIT_CACHE_CAPACITY).expect("ratio")
    );
    assert!(session.undo_commit().is_some());
    assert_eq!(
        session.domain_it_ratio_per_mille(),
        u16::try_from(4_000 / COMMIT_CACHE_CAPACITY).expect("ratio"),
        "the recency entry remains until the host reports Applied"
    );
    assert!(session.acknowledge_undo_commit());
    assert_eq!(
        session.domain_it_ratio_per_mille(),
        u16::try_from(3_000 / (COMMIT_CACHE_CAPACITY - 1)).expect("ratio")
    );
}

#[test]
fn commit_undo_unknown_clears_untrusted_carry_and_personal_context() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("かな").expect("fits");
    session.record_current_commit("加奈", 77, 1, 1);
    session.reset();
    assert_eq!(session.carry_right_id(), 77);
    assert!(session.has_carry);
    assert!(session.cached_surface_fingerprint("かな").is_some());

    assert!(session.undo_commit().is_some());
    assert!(session.undo_pending());
    assert!(session.abort_undo_commit());

    assert!(!session.undo_pending());
    assert_eq!(session.carry_right_id(), 0);
    assert!(!session.has_carry);
    assert_eq!(session.domain_it_ratio_per_mille(), 0);
    assert_eq!(session.cached_surface_fingerprint("かな"), None);
    assert!(session.undo_commit().is_none());
}

#[test]
fn sentence_boundary_and_sensitive_scope_gate_recent_context() {
    let mut session = Session::new("editor.exe");
    session.preedit.push_str("いしゃに").expect("fits");
    session.record_current_commit("医者に", 42, 0, 2);
    assert_eq!(session.carry_right_id(), 42);
    session.reset();

    session.preedit.push_str("おわり").expect("fits");
    session.record_current_commit("終わり。", 9, 0, 1);
    assert_eq!(session.carry_right_id(), 0);
    session.reset();

    session.scope = InputScope::Password;
    session.preedit.push_str("ひみつ").expect("fits");
    session.record_current_commit("秘密", 7, 1, 1);
    assert_eq!(session.carry_right_id(), 0);
    assert_eq!(session.cached_surface_fingerprint("ひみつ"), None);
    session.reset();
    assert_eq!(session.undo_commit(), None);
}

#[test]
fn a_fresh_session_table_hands_out_ids_starting_at_one() {
    let mut table = SessionTable::new();
    assert!(table.is_empty());
    let id = table.create("a.exe").expect("room for one session");
    assert_eq!(id, 1);
    assert_eq!(table.len(), 1);
    assert!(!table.is_empty());
}

#[test]
fn created_sessions_are_reachable_by_get_and_get_mut() {
    let mut table = SessionTable::new();
    let id = table.create("a.exe").expect("room");
    assert_eq!(table.get(id).map(Session::process_name), Some("a.exe"));
    table.get_mut(id).expect("live").mode = Mode::FullAlnum;
    assert_eq!(table.get(id).map(Session::mode), Some(Mode::FullAlnum));
}

#[test]
fn an_unknown_id_resolves_to_nothing() {
    let mut table = SessionTable::new();
    assert!(table.get(999).is_none());
    assert!(table.get_mut(999).is_none());
    assert!(!table.delete(999));
}

#[test]
fn deleting_a_session_frees_its_slot_but_never_its_id() {
    let mut table = SessionTable::new();
    let first = table.create("a.exe").expect("room");
    assert!(table.delete(first));
    assert_eq!(table.len(), 0);
    assert!(table.get(first).is_none());

    let second = table.create("b.exe").expect("room");
    assert_ne!(first, second, "a deleted id must never be handed out again");
    assert!(second > first);
}

#[test]
fn the_table_reports_busy_once_full_and_recovers_after_a_delete() {
    let mut table = SessionTable::new();
    let mut ids = Vec::new();
    for _ in 0..MAX_SESSIONS {
        ids.push(table.create("app.exe").expect("under the cap"));
    }
    assert_eq!(table.len(), MAX_SESSIONS);
    assert_eq!(table.create("one.exe"), Err(ErrorCode::Busy));

    // Freeing one slot makes room for exactly one more.
    assert!(table.delete(ids[0]));
    assert!(table.create("another.exe").is_ok());
    assert_eq!(table.create("yet-another.exe"), Err(ErrorCode::Busy));
}

#[test]
fn clear_empties_the_table_without_resetting_the_id_counter() {
    let mut table = SessionTable::new();
    table.create("a.exe").expect("room");
    table.create("b.exe").expect("room");
    table.clear();
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);

    // The next id keeps counting up from where it left off, not from 1
    // again — a stale request naming an id from before the clear must
    // not be able to resolve against whatever gets created after it.
    let id = table.create("c.exe").expect("room");
    assert_eq!(id, 3);
}

#[test]
fn default_matches_new() {
    assert_eq!(SessionTable::default().len(), SessionTable::new().len());
}
