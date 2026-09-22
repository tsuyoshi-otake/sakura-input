use super::*;
use sakura_proto::types::CandidatePresentation;
use sakura_proto::Candidate;

#[test]
fn failed_overlay_placement_hides_the_owned_popup_and_can_recover() {
    use std::sync::mpsc;
    use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    let _com = crate::accessibility::ComApartment::new().expect("COM apartment");
    let mut candidates = CandidateWindow::new(mpsc::sync_channel(1).0, mpsc::sync_channel(1).0)
        .expect("candidate windows");
    candidates.update(&crate::tests::candidate_state(1));
    let overlay = candidates.delete_overlay;
    // Route placement to an invalid handle while PaintState retains the real
    // owned surfaces. Restore it before asserting so teardown always owns both.
    candidates.delete_overlay = HWND::default();
    candidates.update(&crate::tests::candidate_state(2));
    candidates.delete_overlay = overlay;
    assert!(candidates.popup_rect().is_none());
    // SAFETY: both handles belong to this fixture and remain live.
    unsafe {
        assert!(!IsWindowVisible(candidates.window).as_bool());
        assert!(!IsWindowVisible(overlay).as_bool());
    }
    candidates.update(&crate::tests::candidate_state(3));
    assert!(candidates.popup_rect().is_some());
    // SAFETY: a fresh snapshot restores these same live owned windows.
    unsafe {
        assert!(IsWindowVisible(candidates.window).as_bool());
        assert!(IsWindowVisible(overlay).as_bool());
    }
}

#[test]
fn invalid_dpi_transition_hides_both_native_candidate_surfaces_and_can_recover() {
    use std::sync::mpsc;
    use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SendMessageW};

    let _com = crate::accessibility::ComApartment::new().expect("COM apartment");
    let mut candidates = CandidateWindow::new(mpsc::sync_channel(1).0, mpsc::sync_channel(1).0)
        .expect("candidate windows");
    for revision in [1, 2] {
        candidates.update(&crate::tests::candidate_state(revision));
        assert!(candidates.popup_rect().is_some());
        // SAFETY: all HWNDs belong to this fixture. The missing suggested
        // rectangle exercises the explicitly supported malformed DPI branch.
        unsafe {
            assert!(IsWindowVisible(candidates.window).as_bool());
            assert!(IsWindowVisible(candidates.delete_overlay).as_bool());
            SendMessageW(candidates.window, WM_DPICHANGED, None, None);
            assert!(candidates.popup_rect().is_none());
            assert!(!IsWindowVisible(candidates.delete_overlay).as_bool());
            assert!(
                !IsWindowVisible(candidates.window).as_bool(),
                "invalid DPI left the display visible after state became hidden"
            );
        }
    }
}

#[test]
fn lost_feed_hides_native_popup_and_click_targets_without_replaying_stale_updates() {
    use std::sync::{mpsc, Arc, Mutex};
    use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SendMessageW};

    struct Fixture {
        host: HWND,
        app: Box<crate::App>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // SAFETY: this fixture owns the hidden host and clears its app
            // pointer before either the host or boxed app is destroyed.
            unsafe {
                SetWindowLongPtrW(self.host, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.host);
            }
        }
    }
    impl Fixture {
        fn report(&self, signal: crate::watch::Signal) {
            crate::report(self.host.0 as isize, &self.app.mailbox, signal);
        }

        fn dispatch(&self) {
            // SAFETY: a synchronous dispatch on the owning UI thread; no
            // reference into app is held while the window procedure runs.
            unsafe {
                SendMessageW(self.host, crate::WM_UI, None, None);
            }
        }

        fn assert_visible(&self, expected: bool) {
            assert_eq!(self.app.candidates.popup_rect().is_some(), expected);
            // SAFETY: these are this fixture's live, owned windows, never
            // windows found by a global class lookup or the installed IME.
            unsafe {
                assert_eq!(
                    IsWindowVisible(self.app.candidates.window).as_bool(),
                    expected
                );
                assert_eq!(
                    IsWindowVisible(self.app.candidates.delete_overlay).as_bool(),
                    expected
                );
            }
        }
    }

    let _com = crate::accessibility::ComApartment::new().expect("COM apartment");
    let indicator = crate::indicator::Indicator::new().expect("indicator");
    let candidates = CandidateWindow::new(mpsc::sync_channel(1).0, mpsc::sync_channel(1).0)
        .expect("candidate windows");
    let host = crate::create_host().expect("hidden renderer host");
    let mut fixture = Fixture {
        host,
        app: Box::new(crate::App {
            indicator,
            candidates,
            pad: None,
            pad_theme: AppearanceTheme::Auto,
            raw_input: crate::raw_input::RawInputOwner::new(host, 0),
            pad_shortcut: sakura_proto::PadShortcut::Disabled,
            pad_config_generation: 0,
            shown_indicator: None,
            mailbox: Arc::new(Mutex::new(None)),
            history_delete_completions: mpsc::channel().1,
            candidate_commit_completions: mpsc::channel().1,
        }),
    };
    // SAFETY: the app's Box has a stable address and Fixture clears it in Drop.
    unsafe {
        SetWindowLongPtrW(host, GWLP_USERDATA, (&raw mut *fixture.app) as isize);
    }
    let snapshot =
        |revision| crate::watch::Signal::Ui(Box::new(crate::tests::candidate_state(revision)));
    fixture.report(snapshot(17));
    fixture.dispatch();
    fixture.assert_visible(true);

    // An old update was posted but not yet drawn when the feed failed.
    fixture.report(snapshot(18));
    fixture.report(crate::watch::Signal::Unavailable);
    fixture.dispatch();
    fixture.assert_visible(false);
    assert_eq!(fixture.app.shown_indicator, None);
    assert_eq!(fixture.app.pad_theme, AppearanceTheme::Dark);
    fixture.dispatch(); // leftover WM_UI cannot replay the old snapshot
    fixture.assert_visible(false);

    // Reconnection can reset the engine revision. A pending invalidation
    // must not hide the newer snapshot when the UI pump finally catches up.
    fixture.report(crate::watch::Signal::Unavailable);
    fixture.report(snapshot(1));
    fixture.dispatch();
    fixture.assert_visible(true);
    fixture.dispatch();
    fixture.assert_visible(true);
    fixture.report(crate::watch::Signal::Unavailable);
    fixture.dispatch();
    fixture.assert_visible(false);
}

fn candidates(items: Vec<Candidate>, selected: u16, kind: CandidateKind) -> CandidateList {
    CandidateList {
        kind,
        presentation: CandidatePresentation::Expanded,
        items,
        selected,
        page_size: CANDIDATE_PAGE_SIZE as u16,
    }
}

fn item(text: &str, annotation: &str) -> Candidate {
    Candidate {
        text: text.to_owned(),
        annotation: annotation.to_owned(),
        deletable_history: false,
    }
}

fn deletable_history_item(text: &str) -> Candidate {
    Candidate {
        text: text.to_owned(),
        annotation: "presentation-only".to_owned(),
        deletable_history: true,
    }
}

fn detail() -> CandidateDetail {
    CandidateDetail {
        reading: "ようご".to_owned(),
        definition: "絵文字😀と結合文字e\u{301}を含む、全文折り返し表示の説明です。".repeat(8),
        definition_truncated: true,
        aliases: vec!["別名A".to_owned(), "別名B".to_owned()],
        related: vec!["関連語".to_owned()],
        similar: Vec::new(),
        antonyms: vec!["反対語".to_owned()],
    }
}

fn screen(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
    ScreenRect {
        left,
        top,
        right,
        bottom,
    }
}

fn inflate_y(rect: ScreenRect, px: i32) -> ScreenRect {
    ScreenRect {
        top: rect.top.saturating_sub(px),
        bottom: rect.bottom.saturating_add(px),
        ..rect
    }
}

fn composition_cover_is_unavoidable(anchor: ScreenRect, height: i32, work: RECT, gap: i32) -> bool {
    let below = anchor.bottom.saturating_add(gap);
    let above = anchor.top.saturating_sub(gap).saturating_sub(height);
    below.saturating_add(height) > work.bottom && above < work.top
}

/// A 5px-grid oracle: if any popup origin on that grid clears both
/// rectangles and stays within the detour, the production placement
/// had a nearby alternative to covering the composition.
fn clear_origin_on_five_px_grid(
    anchor: ScreenRect,
    document: Option<ScreenRect>,
    width: i32,
    height: i32,
    work: RECT,
    detour: i32,
) -> Option<RECT> {
    let max_x = (work.right.saturating_sub(width)).max(work.left);
    let max_y = (work.bottom.saturating_sub(height)).max(work.top);
    let mut xs = vec![work.left, max_x, anchor.left.clamp(work.left, max_x)];
    if let Some(document) = document {
        xs.push(document.left.saturating_sub(8).saturating_sub(width));
        xs.push(document.right.saturating_add(8));
    }
    xs.sort_unstable();
    xs.dedup();
    let mut y = work.top;
    while y <= max_y {
        for &x in &xs {
            let left = x.clamp(work.left, max_x);
            let rect = RECT {
                left,
                top: y,
                right: left.saturating_add(width),
                bottom: y.saturating_add(height),
            };
            if rect.left >= work.left
                && rect.right <= work.right
                && rect.top >= work.top
                && rect.bottom <= work.bottom
                && caret_distance(rect, anchor) <= detour
                && !covers_composition(rect, anchor)
                && !covers_document(rect, document)
            {
                return Some(rect);
            }
        }
        let next = y.saturating_add(5);
        if next <= y {
            break;
        }
        y = next;
    }
    None
}

#[test]
fn digit_labels_cover_exactly_the_protocol_page() {
    for index in 0..CANDIDATE_PAGE_SIZE {
        assert_eq!(number_label(index), format!("{}.", index + 1));
    }
    assert_eq!(number_label(CANDIDATE_PAGE_SIZE), "");
}

#[test]
fn summaries_use_global_candidate_indices() {
    assert_eq!(page_summary(9, 14, 14), "10–14 / 14");
    assert_eq!(page_summary(0, 9, 19), "1–9 / 19");
}

#[test]
fn empty_text_never_crosses_draw_text() {
    assert_eq!(drawable_utf16(""), None);
    assert_eq!(
        drawable_utf16("候補"),
        Some("候補".encode_utf16().collect())
    );
}

#[test]
fn high_contrast_delete_glyph_uses_the_system_window_text_role() {
    let palette = high_contrast_palette();
    assert_eq!(palette.action, system_color(COLOR_WINDOWTEXT));
    assert_eq!(palette.surface, system_color(COLOR_WINDOW));
    assert_eq!(palette.selected, system_color(COLOR_HIGHLIGHT));
}

#[test]
fn layout_is_bounded_and_scales_all_tokens_for_adversarial_text() {
    for length in [0, 1, 2, 16, 128, 4096] {
        let content = "候".repeat(length);
        let list = candidates(vec![item(&content, &content)], 0, CandidateKind::Conversion);
        let at_96 = layout(&list, 96);
        let at_144 = layout(&list, 144);
        let at_192 = layout(&list, 192);
        assert!((MIN_WIDTH_96..=MAX_WIDTH_96).contains(&at_96.width));
        assert_eq!(at_144.width, scaled(at_96.width, 144));
        assert_eq!(at_192.width, scaled(at_96.width, 192));
        assert_eq!(at_144.row_height, scaled(ROW_HEIGHT_96, 144));
        assert_eq!(at_192.footer_height, scaled(FOOTER_HEIGHT_96, 192));
        assert!(at_96.annotation_width <= MAX_WIDTH_96 / 2);
    }
}

#[test]
fn columns_are_stable_and_non_overlapping_for_annotations() {
    for surface_length in 0..80 {
        for annotation_length in 0..40 {
            let list = candidates(
                vec![item(
                    &"a".repeat(surface_length),
                    &"注".repeat(annotation_length),
                )],
                0,
                CandidateKind::Suggestion,
            );
            let layout = layout(&list, 96);
            let row = RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.row_height,
            };
            let (surface, annotation) = candidate_columns(row, layout);
            assert!(surface.left <= surface.right);
            assert!(annotation.left <= annotation.right);
            assert!(surface.right <= annotation.left || layout.annotation_width == 0);
            assert!(annotation.right <= row.right.saturating_sub(layout.padding));
        }
    }
}

#[test]
fn history_delete_is_typed_right_aligned_and_has_a_larger_dpi_scaled_hit_target() {
    let list = candidates(
        vec![
            item("annotation-is-not-a-capability", "履歴"),
            deletable_history_item("history"),
        ],
        0,
        CandidateKind::Suggestion,
    );
    for dpi in [96, 120, 144, 168, 192] {
        let layout = layout(&list, dpi);
        assert_eq!(
            layout.history_delete_glyph_size,
            scaled(HISTORY_DELETE_GLYPH_SIZE_96, dpi)
        );
        assert_eq!(
            layout.history_delete_hit_size,
            scaled(HISTORY_DELETE_HIT_SIZE_96, dpi)
        );
        assert_eq!(
            layout.history_delete_stroke,
            scaled(HISTORY_DELETE_STROKE_96, dpi)
        );
        assert_eq!(
            layout.history_delete_gutter,
            scaled(HISTORY_DELETE_HIT_SIZE_96 + HISTORY_DELETE_GAP_96, dpi)
        );
        let popup = PopupLayout {
            candidates: RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.height,
            },
            detail: None,
        };
        let second_row = RECT {
            left: 0,
            top: layout.row_height,
            right: layout.width,
            bottom: layout.row_height * 2,
        };
        let glyph = history_delete_rect(second_row, layout);
        let hit_rect = history_delete_hit_rect(second_row, layout);
        assert_eq!(glyph.right, second_row.right - layout.padding);
        assert_eq!(glyph.right - glyph.left, layout.history_delete_glyph_size);
        assert_eq!(
            hit_rect.right - hit_rect.left,
            layout.history_delete_hit_size
        );
        assert_eq!(
            hit_rect.bottom - hit_rect.top,
            layout.history_delete_hit_size
        );
        assert!(hit_rect.left <= glyph.left);
        assert!(glyph.right <= hit_rect.right);
        assert!(
            (hit_rect.left + hit_rect.right - glyph.left - glyph.right).abs() <= 1,
            "the larger target stays centered on the visual glyph"
        );
        assert!(second_row.left <= hit_rect.left);
        assert!(hit_rect.right <= second_row.right);
        assert!(second_row.top <= hit_rect.top);
        assert!(hit_rect.bottom <= second_row.bottom);
        let hit = POINT {
            x: hit_rect.left,
            y: (hit_rect.top + hit_rect.bottom) / 2,
        };
        assert!(hit.x < glyph.left, "pointer target exceeds the glyph");
        let targets = history_delete_targets(&list, popup, layout);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].candidate_index, 1);
        assert_eq!(targets[0].row, second_row);
        assert_eq!(targets[0].hit, hit_rect);
        assert_eq!(
            history_delete_request_at_client_point(&list, popup, layout, 41, hit),
            Some(HistoryDeleteRequest {
                revision: 41,
                candidate_index: 1,
            })
        );
        let first_row_hit = history_delete_hit_rect(
            RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.row_height,
            },
            layout,
        );
        assert_eq!(
            history_delete_request_at_client_point(
                &list,
                popup,
                layout,
                41,
                POINT {
                    x: (first_row_hit.left + first_row_hit.right) / 2,
                    y: (first_row_hit.top + first_row_hit.bottom) / 2,
                },
            ),
            None,
            "annotation alone must never create a deletion capability"
        );
        let row_text = POINT {
            x: hit_rect.left.saturating_sub(1),
            y: hit.y,
        };
        assert_eq!(
            history_delete_request_at_client_point(&list, popup, layout, 41, row_text),
            None,
            "row text is not a deletion control"
        );
        assert_eq!(
            candidate_commit_request_at_client_point(&list, popup, layout, 41, row_text),
            Some(CandidateCommitRequest {
                revision: 41,
                candidate_index: 1,
            }),
            "a deliberate row click queues that exact visible candidate"
        );

        let (surface, annotation) = candidate_columns(second_row, layout);
        assert!(surface.right <= annotation.left || layout.annotation_width == 0);
        assert!(annotation.right <= hit_rect.left);
    }
}

#[test]
fn input_disabled_display_and_enabled_overlay_styles_are_separate() {
    let display_ex = display_popup_ex_style();
    let overlay_ex = delete_overlay_ex_style();
    for style in [display_ex, overlay_ex] {
        assert_ne!(style.0 & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(style.0 & WS_EX_TOOLWINDOW.0, 0);
        assert_ne!(style.0 & WS_EX_TOPMOST.0, 0);
        assert_eq!(style.0 & WS_EX_TRANSPARENT.0, 0);
    }
    assert_ne!(display_popup_style().0 & WS_DISABLED.0, 0);
    assert_eq!(delete_overlay_style().0 & WS_DISABLED.0, 0);
    assert_eq!(
        delete_overlay_non_client_hit_test_result().0,
        HTCLIENT as isize,
        "the region-clipped overlay receives only delete-target pointer input"
    );
}

#[test]
fn overlay_visibility_requires_a_visible_display_a_valid_region_and_current_page_targets() {
    let target = RECT {
        left: 0,
        top: 0,
        right: 28,
        bottom: 28,
    };

    assert!(delete_overlay_should_be_visible(true, true, &[target]));
    assert!(!delete_overlay_should_be_visible(false, true, &[target]));
    assert!(!delete_overlay_should_be_visible(true, false, &[target]));
    assert!(!delete_overlay_should_be_visible(true, true, &[]));
}

#[test]
fn pending_deletes_keep_each_typed_target_suppressed_until_a_new_ui_revision() {
    let first = HistoryDeleteRequest {
        revision: 41,
        candidate_index: 2,
    };
    let second = HistoryDeleteRequest {
        revision: 41,
        candidate_index: 7,
    };
    let mut pending = Vec::new();

    assert!(!is_history_delete_pending(&pending, first));
    pending.push(first);
    assert!(is_history_delete_pending(&pending, first));
    assert!(!is_history_delete_pending(&pending, second));
    pending.push(second);
    assert!(is_history_delete_pending(&pending, first));
    assert!(is_history_delete_pending(&pending, second));

    clear_pending_history_deletes_for_new_revision(&mut pending, 41, 41);
    assert_eq!(pending, vec![first, second]);
    clear_pending_history_deletes_for_new_revision(&mut pending, 41, 42);
    assert!(pending.is_empty());
    assert!(!is_history_delete_pending(&pending, first));
}

#[test]
fn failed_delete_releases_only_its_target_while_success_waits_for_new_revision() {
    let failed = HistoryDeleteRequest {
        revision: 41,
        candidate_index: 2,
    };
    let removed = HistoryDeleteRequest {
        revision: 41,
        candidate_index: 7,
    };
    let mut pending = vec![failed, removed];

    finish_pending_history_delete(&mut pending, failed, false);
    assert_eq!(pending, vec![removed]);
    finish_pending_history_delete(&mut pending, removed, true);
    assert_eq!(
        pending,
        vec![removed],
        "an authoritative removal stays suppressed until a newer UiState"
    );
}

#[test]
fn accessibility_uses_the_requesting_hwnd_for_base_and_overlay() {
    let base = HWND::default();
    let overlay = HWND(std::ptr::dangling_mut());

    assert_eq!(candidate_accessibility_request_window(base), base);
    assert_eq!(candidate_accessibility_request_window(overlay), overlay);
    assert_ne!(
        candidate_accessibility_request_window(base),
        candidate_accessibility_request_window(overlay),
        "each WM_GETOBJECT response must be returned through its source HWND"
    );
}

#[test]
fn delete_overlay_targets_only_include_the_current_page_and_global_indices() {
    let mut items = (0..=CANDIDATE_PAGE_SIZE)
        .map(|index| item(&format!("candidate-{index}"), ""))
        .collect::<Vec<_>>();
    items[0] = deletable_history_item("first-page-history");
    items[CANDIDATE_PAGE_SIZE] = deletable_history_item("second-page-history");
    let list = candidates(items, CANDIDATE_PAGE_SIZE as u16, CandidateKind::Suggestion);
    let layout = layout(&list, 144);
    let popup = PopupLayout {
        candidates: RECT {
            left: 11,
            top: 13,
            right: 11 + layout.width,
            bottom: 13 + layout.height,
        },
        detail: None,
    };
    let targets = history_delete_targets(&list, popup, layout);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].candidate_index, CANDIDATE_PAGE_SIZE);
    assert_eq!(targets[0].row.top, popup.candidates.top);
    assert_eq!(
        history_delete_request_at_client_point(
            &list,
            popup,
            layout,
            99,
            POINT {
                x: (targets[0].hit.left + targets[0].hit.right) / 2,
                y: (targets[0].hit.top + targets[0].hit.bottom) / 2,
            },
        ),
        Some(HistoryDeleteRequest {
            revision: 99,
            candidate_index: CANDIDATE_PAGE_SIZE as u16,
        })
    );
}

#[test]
fn compact_selection_never_changes_page_sized_width_or_columns() {
    let items = (0..CANDIDATE_PAGE_SIZE)
        .map(|index| {
            item(
                &"候補".repeat(index.saturating_add(1)),
                if index % 3 == 0 { "履歴" } else { "" },
            )
        })
        .collect::<Vec<_>>();
    let mut list = candidates(items, 0, CandidateKind::Conversion);
    list.presentation = CandidatePresentation::Compact;
    let baseline = layout(&list, 96);
    for selected in 0..CANDIDATE_PAGE_SIZE {
        list.selected = selected as u16;
        let current = layout(&list, 96);
        assert_eq!(current.width, baseline.width);
        assert_eq!(current.annotation_width, baseline.annotation_width);
        assert_eq!(current.height, baseline.height);
    }
}

#[test]
fn rows_and_footer_fill_the_layout_height_at_every_dpi() {
    for dpi in [96, 120, 125, 144, 168, 192] {
        for count in 1..=CANDIDATE_PAGE_SIZE {
            let list = candidates(
                (0..count).map(|_| item("候補", "注釈")).collect(),
                0,
                CandidateKind::Conversion,
            );
            let layout = layout(&list, dpi);
            assert_eq!(
                layout.height,
                layout
                    .row_height
                    .saturating_mul(count as i32)
                    .saturating_add(layout.footer_height)
            );
        }
    }
}

#[test]
fn selection_rail_is_not_overpainted_by_the_window_border() {
    let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
    for dpi in [96, 144, 192] {
        let layout = layout(&list, dpi);
        let row = RECT {
            left: 0,
            top: 0,
            right: layout.width,
            bottom: layout.row_height,
        };
        let rail = selection_rail(row, layout);
        assert_eq!(rail.left, 1);
        assert_eq!(rail.right - rail.left, layout.rail_width);
        assert!(rail.right <= row.right);
    }
}

#[test]
fn page_rail_thumb_is_bounded_for_every_valid_page() {
    let footer = RECT {
        left: 0,
        top: 0,
        right: 300,
        bottom: FOOTER_HEIGHT_96,
    };
    for count in 1..=64 {
        for selected in 0..count {
            let list = candidates(
                (0..count).map(|_| item("候補", "注釈")).collect(),
                selected as u16,
                CandidateKind::Conversion,
            );
            let rail = page_rail(footer, &list, layout(&list, 96));
            assert!(rail.track.top <= rail.thumb.top);
            assert!(rail.thumb.top < rail.thumb.bottom);
            assert!(rail.thumb.bottom <= rail.track.bottom);
            assert_eq!(rail.track.left, rail.thumb.left);
            assert_eq!(rail.track.right, rail.thumb.right);
        }
    }
}

#[test]
fn placement_supports_negative_virtual_desktop_coordinates() {
    let work = RECT {
        left: -1920,
        top: 0,
        right: 0,
        bottom: 1080,
    };
    let anchor = ScreenRect {
        left: -1800,
        top: 100,
        right: -1700,
        bottom: 124,
    };
    let popup = place(anchor, 440, 200, work, 4);
    assert_eq!(popup.left, -1800);
    assert_eq!(popup.top, 128);
    assert!(popup.right <= work.right);
}

#[test]
fn placement_flips_above_at_the_bottom_edge_and_clamps_oversize() {
    let work = RECT {
        left: 200,
        top: 100,
        right: 500,
        bottom: 400,
    };
    let anchor = ScreenRect {
        left: 450,
        top: 350,
        right: 470,
        bottom: 370,
    };
    let popup = place(anchor, 600, 600, work, 4);
    assert_eq!((popup.left, popup.top), (work.left, work.top));
}

/// When the popup is taller than the free space both below and above the
/// composition, covering it is unavoidable — but the popup must take the
/// roomier side, not blindly slide up over the caret line from below.
#[test]
fn placement_covers_from_the_roomier_side_only_when_nothing_fits() {
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_000,
        bottom: 600,
    };
    let low_anchor = ScreenRect {
        left: 100,
        top: 500,
        right: 300,
        bottom: 524,
    };
    let pinned_top = place(low_anchor, 300, 550, work, 8);
    assert_eq!(pinned_top.top, work.top);

    let high_anchor = ScreenRect {
        left: 100,
        top: 30,
        right: 300,
        bottom: 54,
    };
    let pinned_bottom = place(high_anchor, 300, 550, work, 8);
    assert_eq!(pinned_bottom.bottom, work.bottom);
}

/// The reported bug, at its measured geometry: a multi-line box whose
/// caret sits near the top. "Below the composition" clears the caret
/// line and still lands inside the box the user is typing into.
/// The measured 884x581 host edit control. Every placement that clears a
/// box that deep is a long way from the caret, so the popup stays where
/// the user is looking: directly under the caret, on the box's empty
/// part. Moving it beside the box instead put it 858 px away.
#[test]
fn a_deep_editable_box_keeps_the_popup_next_to_the_caret() {
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_920,
        bottom: 1_032,
    };
    let document = ScreenRect {
        left: 208,
        top: 191,
        right: 1_092,
        bottom: 772,
    };
    let anchor = ScreenRect {
        left: 216,
        top: 195,
        right: 280,
        bottom: 225,
    };

    let placed = place_candidates(anchor, Some(document), 260, 274, work, 4, 160);
    assert_eq!(placed, place(anchor, 260, 274, work, 4));
    assert_eq!(caret_distance(placed, anchor), 4);
    assert!(placed.left >= work.left && placed.right <= work.right);
    assert!(placed.top >= work.top && placed.bottom <= work.bottom);
}

/// Each rung of the ladder in turn — and the budget that stops the
/// ladder — so neither a later rung nor the fallback can silently become
/// the answer for a case the other should have taken.
#[test]
fn placement_only_detours_while_it_stays_next_to_the_caret() {
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_000,
        bottom: 600,
    };
    let gap = 4;
    let detour = 160;

    // A box no taller than the caret line: below is already clear, and
    // the popup must not move off it.
    let short = ScreenRect {
        left: 100,
        top: 100,
        right: 400,
        bottom: 130,
    };
    let anchor = ScreenRect {
        left: 100,
        top: 100,
        right: 160,
        bottom: 130,
    };
    assert_eq!(
        place_candidates(anchor, Some(short), 200, 200, work, gap, detour),
        place(anchor, 200, 200, work, gap),
    );

    // A chat-height box at the bottom of the screen. Below does not fit,
    // so the composition-only placement flips above the caret and lands
    // on the lines already typed; stepping past the box's top edge costs
    // a few dozen pixels and keeps them readable.
    let chat_box = ScreenRect {
        left: 100,
        top: 480,
        right: 700,
        bottom: 570,
    };
    let caret_in_chat_box = ScreenRect {
        left: 100,
        top: 510,
        right: 160,
        bottom: 540,
    };
    assert!(covers_composition(
        place(caret_in_chat_box, 200, 100, work, gap),
        chat_box
    ));
    let above_box = place_candidates(
        caret_in_chat_box,
        Some(chat_box),
        200,
        100,
        work,
        gap,
        detour,
    );
    assert_eq!(above_box.bottom, chat_box.top - gap);
    assert!(caret_distance(above_box, caret_in_chat_box) <= detour);

    // The same idea with room under the box instead of above it.
    let high_box = ScreenRect {
        left: 100,
        top: 100,
        right: 400,
        bottom: 230,
    };
    let caret_in_high_box = ScreenRect {
        left: 100,
        top: 105,
        right: 160,
        bottom: 135,
    };
    let below_box = place_candidates(
        caret_in_high_box,
        Some(high_box),
        200,
        100,
        work,
        gap,
        detour,
    );
    assert_eq!(below_box.top, high_box.bottom + gap);
    assert!(caret_distance(below_box, caret_in_high_box) <= detour);

    // A narrow box with no room above or below it. Beside it is still
    // within reach of the caret, so the popup goes there, level with it.
    let narrow_box = ScreenRect {
        left: 100,
        top: 50,
        right: 220,
        bottom: 550,
    };
    let caret_in_narrow_box = ScreenRect {
        left: 100,
        top: 60,
        right: 160,
        bottom: 90,
    };
    let beside = place_candidates(
        caret_in_narrow_box,
        Some(narrow_box),
        200,
        100,
        work,
        gap,
        detour,
    );
    assert_eq!(beside.left, narrow_box.right + gap);
    assert_eq!(beside.top, caret_in_narrow_box.top);
    assert!(caret_distance(beside, caret_in_narrow_box) <= detour);

    // The same box made wide: now every clear placement is far from the
    // caret, so the popup stays under it and accepts the overlap.
    let wide_box = ScreenRect {
        left: 100,
        top: 50,
        right: 800,
        bottom: 550,
    };
    let stays = place_candidates(
        caret_in_narrow_box,
        Some(wide_box),
        200,
        100,
        work,
        gap,
        detour,
    );
    assert_eq!(stays, place(caret_in_narrow_box, 200, 100, work, gap));
    assert_eq!(caret_distance(stays, caret_in_narrow_box), gap);
}

/// Fixed-seed sweep over caret, box, and work-area geometries. Whatever
/// the shape, the popup is either the composition-only placement or a
/// detour that is both within budget and clear of what it stepped off.
#[test]
fn placement_never_strays_further_from_the_caret_than_the_budget() {
    let work = RECT {
        left: -400,
        top: -200,
        right: 1_600,
        bottom: 1_000,
    };
    let gap = 4;
    let detour = 160;
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as i32
    };
    let mut detours = 0usize;
    for _ in 0..8_192 {
        let left = work.left + next() % 1_800;
        let top = work.top + next() % 1_100;
        let document = ScreenRect {
            left,
            top,
            right: left + 40 + next() % 1_200,
            bottom: top + 24 + next() % 900,
        };
        let caret_left = left + next() % (document.right - left).max(1);
        let caret_top = top + next() % (document.bottom - top).max(1);
        let anchor = ScreenRect {
            left: caret_left,
            top: caret_top,
            right: caret_left + 8 + next() % 200,
            bottom: caret_top + 16 + next() % 40,
        };
        let width = 200 + next() % 280;
        let height = 60 + next() % 400;

        let placed = place_candidates(anchor, Some(document), width, height, work, gap, detour);
        if placed == place(anchor, width, height, work, gap) {
            continue;
        }
        detours += 1;
        assert!(
            caret_distance(placed, anchor) <= detour,
            "detour {placed:?} strayed from {anchor:?}"
        );
        assert!(!covers_composition(placed, document));
        assert!(!covers_composition(placed, anchor));
        assert!(placed.left >= work.left && placed.right <= work.right);
        assert!(placed.top >= work.top && placed.bottom <= work.bottom);
    }
    assert!(
        detours > 0,
        "the sweep never took a detour, so it proved nothing about them"
    );
}

/// A document filling the work area — a full-window editor — has no
/// placement that clears it. That must cost nothing: the popup keeps
/// exactly the geometry it had before the editable area was known.
#[test]
fn a_document_with_no_clear_placement_keeps_the_previous_behaviour() {
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_000,
        bottom: 600,
    };
    let anchor = ScreenRect {
        left: 100,
        top: 100,
        right: 160,
        bottom: 130,
    };
    let whole_screen = ScreenRect {
        left: 0,
        top: 0,
        right: 1_000,
        bottom: 600,
    };
    let expected = place(anchor, 200, 200, work, 4);
    assert_eq!(
        place_candidates(anchor, Some(whole_screen), 200, 200, work, 4, 160),
        expected,
    );
    // And a host that reports no editable area at all is the same case.
    assert_eq!(
        place_candidates(anchor, None, 200, 200, work, 4, 160),
        expected
    );
}

/// Every caret line on a 5px vertical grid, plus a ±5px GetTextExt wobble.
/// Covering the composition is allowed only when the popup is taller than
/// the free space on both sides. A nearby clear origin on the same grid
/// means the cover was avoidable.
#[test]
fn five_px_vertical_sweep_keeps_the_composition_readable() {
    let list = candidates(
        (0..CANDIDATE_PAGE_SIZE)
            .map(|_| item("変換", "へんかん"))
            .collect(),
        3,
        CandidateKind::Conversion,
    );
    let mut avoidable_cover = Vec::new();
    let mut avoidable_cover_count = 0usize;
    let mut fragile_five_px = Vec::new();
    let mut fragile_five_px_count = 0usize;
    let mut covered_document = 0usize;
    let mut placements = 0usize;
    let mut unavoidable_cover = 0usize;

    for dpi in [96u32, 120, 144, 192] {
        let candidate_layout = layout(&list, dpi);
        let width = candidate_layout.width;
        let height = candidate_layout.height;
        let gap = candidate_layout.gap;
        let detour = candidate_layout.detour;
        let caret_h = scaled(24, dpi).max(16);
        let caret_w = scaled(64, dpi).max(24);
        let works = [
            RECT {
                left: 0,
                top: 0,
                right: 1_920,
                bottom: 1_040,
            },
            RECT {
                left: 0,
                top: 0,
                right: 1_280,
                bottom: 720,
            },
            RECT {
                left: 0,
                top: 0,
                right: 1_366,
                bottom: 728,
            },
        ];
        for work in works {
            let with_detail =
                detail_layout("変換", &detail(), dpi, work.bottom.saturating_sub(work.top));
            let documents: [Option<ScreenRect>; 6] = [
                None,
                Some(screen(16, 24, 688, 60)),
                Some(screen(208, 191, 1_092, 772)),
                Some(screen(
                    308,
                    901.min(work.bottom - 40),
                    1_192,
                    work.bottom - 18,
                )),
                Some(screen(
                    400,
                    640.min(work.bottom - 80),
                    1_400,
                    work.bottom - 30,
                )),
                Some(screen(work.left, work.top, work.right, work.bottom)),
            ];
            for document in documents {
                let (x_left, x_right, y_top, y_bottom) = match document {
                    Some(document) => {
                        (document.left, document.right, document.top, document.bottom)
                    }
                    None => (work.left + 40, work.right - 40, work.top, work.bottom),
                };
                let xs = [
                    x_left.saturating_add(8),
                    (x_left.saturating_add(x_right)) / 2,
                    x_right.saturating_sub(caret_w).saturating_sub(8),
                ];
                let mut caret_top = y_top;
                let last_top = y_bottom.saturating_sub(caret_h).max(y_top);
                while caret_top <= last_top {
                    for &caret_left in &xs {
                        let anchor = screen(
                            caret_left,
                            caret_top,
                            caret_left.saturating_add(caret_w),
                            caret_top.saturating_add(caret_h),
                        );
                        if !anchor.is_valid() {
                            continue;
                        }
                        placements += 1;
                        let placed = popup_placement(
                            anchor,
                            document,
                            candidate_layout,
                            Some(with_detail),
                            work,
                        );
                        let covers_text = covers_composition(placed.window, anchor);
                        if covers_text {
                            if composition_cover_is_unavoidable(anchor, height, work, gap) {
                                unavoidable_cover += 1;
                            } else {
                                avoidable_cover_count += 1;
                                if avoidable_cover.len() < 24 {
                                    let oracle = clear_origin_on_five_px_grid(
                                        anchor, document, width, height, work, detour,
                                    );
                                    avoidable_cover.push(format!(
                                            "dpi={dpi} caret=({caret_left},{caret_top}) placed=[{},{} {}x{}] oracle={oracle:?} doc={document:?}",
                                            placed.window.left,
                                            placed.window.top,
                                            placed.window.right - placed.window.left,
                                            placed.window.bottom - placed.window.top,
                                        ));
                                }
                            }
                        } else {
                            let wobble = inflate_y(anchor, 5);
                            if covers_composition(placed.window, wobble)
                                && !composition_cover_is_unavoidable(wobble, height, work, gap)
                            {
                                fragile_five_px_count += 1;
                                if fragile_five_px.len() < 24 {
                                    fragile_five_px.push(format!(
                                            "dpi={dpi} caret=({caret_left},{caret_top}) placed=[{},{} {}x{}] wobble={wobble:?}",
                                            placed.window.left,
                                            placed.window.top,
                                            placed.window.right - placed.window.left,
                                            placed.window.bottom - placed.window.top,
                                        ));
                                }
                            }
                        }
                        if covers_document(placed.window, document) {
                            covered_document += 1;
                        }
                    }
                    let next = caret_top.saturating_add(5);
                    if next <= caret_top {
                        break;
                    }
                    caret_top = next;
                }
            }
        }
    }

    let summary = format!(
            "5px sweep: placements={placements} unavoidable_composition_cover={unavoidable_cover} document_cover={covered_document} avoidable={avoidable_cover_count} fragile_5px={fragile_five_px_count}"
        );
    println!("{summary}");
    if let Ok(path) = std::env::var("SAKURA_SWEEP_OUT") {
        std::fs::write(&path, summary.as_bytes()).expect("write sweep summary");
    }
    assert!(
            avoidable_cover.is_empty(),
            "popup covered the composition while below or above still fit ({avoidable_cover_count} hits):\n{}",
            avoidable_cover.join("\n")
        );
    assert!(
        fragile_five_px.is_empty(),
        "a ±5px GetTextExt wobble covered the composition ({fragile_five_px_count} hits):\n{}",
        fragile_five_px.join("\n")
    );
}

/// The HWND is one opaque rectangle around the list and the detail. A
/// taller side pane that itself misses the caret can still stretch that
/// rectangle down over the line being typed.
#[test]
fn taller_side_detail_must_not_stretch_the_window_over_the_composition() {
    let list = candidates(
        (0..CANDIDATE_PAGE_SIZE)
            .map(|_| item("変換", "へんかん"))
            .collect(),
        3,
        CandidateKind::Conversion,
    );
    let candidate_layout = layout(&list, 96);
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_920,
        bottom: 1_040,
    };
    let detail_layout = detail_layout("変換", &detail(), 96, work.bottom - work.top);
    assert!(
        detail_layout.height > candidate_layout.height,
        "this case needs a definition taller than the list"
    );
    let anchor = screen(410, 980, 474, 1_004);
    // VS Code / Electron often reports the whole frame as GetScreenExt, so
    // every off-document detour is farther than the caret budget. Fallback
    // sits just above the caret; a taller detail must not then stretch the
    // opaque HWND back down over the line.
    let document = screen(work.left, work.top, work.right, work.bottom);
    let placed = popup_placement(
        anchor,
        Some(document),
        candidate_layout,
        Some(detail_layout),
        work,
    );
    assert!(
        !covers_composition(placed.window, anchor),
        "window L{} T{} R{} B{} covered composition {anchor:?}; detail={:?}",
        placed.window.left,
        placed.window.top,
        placed.window.right,
        placed.window.bottom,
        placed.layout.detail
    );
}

/// A candidate list that flipped above the composition leaves the
/// bottom detail slot sitting exactly on the text being typed. The
/// detail must go absent rather than cover it. A taller side pane is
/// also omitted when the opaque HWND around both panes would stretch
/// back down over the composition; a one-line caret with room beside
/// the list still keeps a side pane that stays a full gap away.
#[test]
fn detail_below_is_omitted_rather_than_covering_the_composition() {
    let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
    let candidate_layout = layout(&list, 96);
    let detail_layout = detail_layout("候補", &detail(), 96, 148);
    assert_eq!(detail_layout.height, 148);
    // A composition wrapped over several lines, ending near the bottom
    // of a short work area, so the candidate list flips above it.
    let anchor = ScreenRect {
        left: 20,
        top: 200,
        right: 260,
        bottom: 340,
    };
    let narrow = RECT {
        left: 0,
        top: 0,
        right: 480,
        bottom: 390,
    };
    let placement = popup_placement(anchor, None, candidate_layout, Some(detail_layout), narrow);
    assert_eq!(placement.window.bottom, anchor.top - candidate_layout.gap);
    assert!(placement.layout.detail.is_none());

    // The same tall composition with room on the right still omits: the
    // side pane itself misses the text, but the HWND would not.
    let wide = RECT {
        left: 0,
        top: 0,
        right: 1_000,
        bottom: 390,
    };
    let control = popup_placement(anchor, None, candidate_layout, Some(detail_layout), wide);
    assert!(control.layout.detail.is_none());
    assert_eq!(control.window.bottom, anchor.top - candidate_layout.gap);

    let one_line = screen(20, 80, 80, 104);
    let with_side = popup_placement(one_line, None, candidate_layout, Some(detail_layout), wide);
    let side = with_side
        .layout
        .detail
        .expect("side detail stays a full gap from a one-line caret");
    assert_eq!(side.left, with_side.layout.candidates.right);
    assert!(!covers_composition(
        with_side.window,
        vertically_padded(one_line, candidate_layout.gap)
    ));
}

/// A tall detail pane beside the list slides up to fit the work area.
/// When the composition is wide enough that the slide would put the pane
/// on top of it, the pane goes absent instead.
#[test]
fn side_detail_that_would_slide_over_the_composition_is_omitted() {
    let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
    let candidate_layout = layout(&list, 96);
    let mut long = detail();
    long.definition = "全文を折り返して表示する長い日本語説明。".repeat(1_000);
    let detail_layout = detail_layout("候補", &long, 96, 340);
    let anchor = ScreenRect {
        left: 20,
        top: 300,
        right: 700,
        bottom: 324,
    };
    let work = RECT {
        left: 0,
        top: 0,
        right: 1_200,
        bottom: 400,
    };
    let placement = popup_placement(anchor, None, candidate_layout, Some(detail_layout), work);
    assert!(placement.layout.detail.is_none());
}

#[test]
fn detail_wraps_all_available_lines_and_only_three_relation_words() {
    let value = "😀e\u{301}".repeat(128);
    let full = definition_lines(&value, 40, 96, usize::MAX, false);
    assert!(full.len() > 2);
    assert!(!full.last().expect("last full line").ends_with('…'));
    let bounded = definition_lines(&value, 40, 96, 3, false);
    assert_eq!(bounded.len(), 3);
    assert!(bounded[2].ends_with('…'));
    let source_bounded = definition_lines("complete preview", 400, 96, 8, true);
    assert!(source_bounded[0].ends_with('…'));
    assert_eq!(
        relation_text(&["a".into(), "b".into(), "c".into(), "d".into()]),
        "a・b・c"
    );
}

#[test]
fn detail_width_is_constant_and_height_grows_then_caps_at_every_dpi() {
    for dpi in [96, 120, 144, 168, 192, 240] {
        let mut short = detail();
        short.definition = "短い説明。".to_owned();
        let mut long = detail();
        long.definition = "全文を折り返して表示する長い日本語説明。".repeat(1_000);
        let max_height = scaled(720, dpi);
        let short_layout = detail_layout("用語", &short, dpi, max_height);
        let long_layout = detail_layout("長さの異なる用語", &long, dpi, max_height);
        assert_eq!(short_layout.width, scaled(DETAIL_WIDTH_96, dpi));
        assert_eq!(long_layout.width, short_layout.width);
        assert!(long_layout.height > short_layout.height);
        assert!(long_layout.height <= max_height);
        assert!(max_height - long_layout.height < long_layout.line_height);
    }
}

#[test]
fn desktop_detail_width_fixture_keeps_the_pane_within_the_candidate_height() {
    let list = candidates(
        (0..18)
            .map(|index| item(&format!("fixture-candidate-{index}"), ""))
            .collect(),
        0,
        CandidateKind::Suggestion,
    );
    for dpi in [96, 120, 144, 192] {
        let candidate_layout = layout(&list, dpi);
        let work = RECT {
            left: 0,
            top: 0,
            right: scaled(1280, dpi),
            bottom: scaled(600, dpi),
        };
        let anchor = screen(
            scaled(120, dpi),
            scaled(120, dpi),
            scaled(140, dpi),
            scaled(144, dpi),
        );
        let make_detail = |definition: String| CandidateDetail {
            reading: "fixture-reading".into(),
            definition,
            definition_truncated: false,
            aliases: vec![],
            related: vec![],
            similar: vec![],
            antonyms: vec![],
        };
        let short = detail_layout(
            "fixture-candidate-0",
            &make_detail("complete-definition".into()),
            dpi,
            work.bottom,
        );
        let long = detail_layout(
            "fixture-candidate-0",
            &make_detail(format!("long-complete-definition-{}", "x".repeat(160))),
            dpi,
            work.bottom,
        );
        assert!(long.height > short.height);
        assert!(long.height <= candidate_layout.height);
        let short = popup_placement(anchor, None, candidate_layout, Some(short), work);
        let long = popup_placement(anchor, None, candidate_layout, Some(long), work);
        assert!(short.layout.detail.is_some());
        assert!(long.layout.detail.is_some());
        assert_eq!(
            long.window.right - long.window.left,
            short.window.right - short.window.left
        );
        let oversized = detail_layout(
            "fixture-candidate-0",
            &make_detail(format!("long-complete-definition-{}", "x".repeat(880))),
            dpi,
            work.bottom,
        );
        let oversized = popup_placement(anchor, None, candidate_layout, Some(oversized), work);
        assert!(
            oversized.layout.detail.is_none(),
            "oversized pane must not cover composition at DPI {dpi}"
        );
    }
}

#[test]
fn wrapping_preserves_every_scalar_when_height_is_available() {
    for dpi in [96, 120, 144, 168, 192, 240] {
        for width in 1..=512 {
            let value = "日本語😀e\u{301}ABC・説明".repeat(5);
            let lines = definition_lines(&value, width, dpi, usize::MAX, false);
            assert_eq!(lines.concat(), value);
        }
    }
}

#[test]
fn detail_placement_preserves_candidate_geometry_at_dpi_and_work_edges() {
    let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
    let detail = detail();
    let work = RECT {
        left: 0,
        top: 0,
        right: 2_500,
        bottom: 1_800,
    };
    for dpi in [96, 120, 144, 168, 192, 240] {
        let candidate_layout = layout(&list, dpi);
        let placement = popup_placement(
            ScreenRect {
                left: 100,
                top: 100,
                right: 120,
                bottom: 124,
            },
            None,
            candidate_layout,
            Some(detail_layout("候補", &detail, dpi, work.bottom - work.top)),
            work,
        );
        assert_eq!(
            placement.layout.candidates.right - placement.layout.candidates.left,
            candidate_layout.width
        );
        assert_eq!(
            placement.layout.candidates.bottom - placement.layout.candidates.top,
            candidate_layout.height
        );
        assert!(placement.layout.detail.is_some());
        assert!(placement.window.left >= work.left);
        assert!(placement.window.top >= work.top);
        assert!(placement.window.right <= work.right);
        assert!(placement.window.bottom <= work.bottom);
    }
}

#[test]
fn detail_falls_back_right_left_bottom_then_absent() {
    let list = candidates(vec![item("候補", "")], 0, CandidateKind::Conversion);
    let candidate_layout = layout(&list, 96);
    let detail_layout = detail_layout("候補", &detail(), 96, 700);

    let right = popup_placement(
        ScreenRect {
            left: 100,
            top: 80,
            right: 120,
            bottom: 104,
        },
        None,
        candidate_layout,
        Some(detail_layout),
        RECT {
            left: 0,
            top: 0,
            right: 1_000,
            bottom: 700,
        },
    );
    assert_eq!(
        right.layout.detail.expect("right detail").left,
        right.layout.candidates.right
    );

    let left = popup_placement(
        ScreenRect {
            left: 400,
            top: 80,
            right: 420,
            bottom: 104,
        },
        None,
        candidate_layout,
        Some(detail_layout),
        RECT {
            left: 0,
            top: 0,
            right: 800,
            bottom: 700,
        },
    );
    assert_eq!(
        left.layout.detail.expect("left detail").right,
        left.layout.candidates.left
    );

    let below = popup_placement(
        ScreenRect {
            left: 20,
            top: 80,
            right: 40,
            bottom: 104,
        },
        None,
        candidate_layout,
        Some(detail_layout),
        RECT {
            left: 0,
            top: 0,
            right: 480,
            bottom: 700,
        },
    );
    assert!(below.layout.detail.expect("bottom detail").top > below.layout.candidates.bottom);

    let absent = popup_placement(
        ScreenRect {
            left: 20,
            top: 600,
            right: 40,
            bottom: 624,
        },
        None,
        candidate_layout,
        Some(detail_layout),
        RECT {
            left: 0,
            top: 0,
            right: 480,
            bottom: 700,
        },
    );
    assert!(absent.layout.detail.is_none());
}
