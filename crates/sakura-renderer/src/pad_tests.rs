use super::*;

const DPIS: [u32; 3] = [96, 144, 192];

/// The widths a status reading can ask its slot for: nothing to say, the
/// resting one, a sentence, and more than the window has.
fn wants(dpi: u32) -> [i32; 4] {
    [0, 1, scaled(220, dpi), 4_000]
}

fn width_of(rect: RECT) -> i32 {
    rect.right.saturating_sub(rect.left)
}

fn client(width_96: i32, height_96: i32, dpi: u32) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: scaled(width_96, dpi),
        bottom: scaled(height_96, dpi),
    }
}

/// Every rectangle a control is actually placed at. Containers are checked
/// separately: they are meant to hold the leaves, not to avoid them.
fn leaves(plan: &PadLayout) -> Vec<(&'static str, RECT)> {
    let mut leaves = vec![
        ("copy", plan.copy),
        ("delete", plan.delete),
        ("new", plan.new),
        ("sort", plan.sort),
        ("sync", plan.sync),
    ];
    for (name, rect) in [
        ("menu", plan.menu),
        ("header_title", plan.header_title),
        ("status", plan.status),
        ("count", plan.count),
        ("search", plan.search),
        ("list", plan.list),
        ("divider", plan.divider),
        ("title", plan.title),
        ("body", plan.body),
    ] {
        if let Some(rect) = rect {
            leaves.push((name, rect));
        }
    }
    leaves
}

fn overlaps(a: RECT, b: RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn contains(outer: RECT, inner: RECT) -> bool {
    inner.left >= outer.left
        && inner.right <= outer.right
        && inner.top >= outer.top
        && inner.bottom <= outer.bottom
}

/// A notice clipped partway is not a notice — `Markdown をコピーしました`
/// arrived as `Markdown をコピ`. Notices are phrases now rather than
/// sentences, and the slot still grows into the gap the title is not
/// using — stopping there, because a heading squeezed into a stub is the
/// same failure the other way round.
#[test]
fn a_long_reading_widens_the_status_slot_but_never_starves_the_title() {
    for dpi in DPIS {
        for width in [520, 640, 1280] {
            for pane in [PadPane::List, PadPane::Editor] {
                let area = client(width, 520, dpi);
                // One device pixel of reading: something to say, and a
                // slot no narrower than the one a time and a save state
                // were sized for.
                let resting = layout(area, dpi, pane, 1);
                let Some(resting_status) = resting.status else {
                    continue;
                };
                assert_eq!(
                    width_of(resting_status),
                    scaled(STATUS_WIDTH_96, dpi),
                    "a time and a save state get the resting width at {width} @ {dpi}"
                );

                let sentence = scaled(220, dpi);
                let widened = layout(area, dpi, pane, sentence);
                let status = widened.status.expect("the slot does not disappear");
                assert!(
                        width_of(status) >= width_of(resting_status),
                        "a measured reading never gets less than the resting width                          at {width} @ {dpi}"
                    );
                assert!(
                    width_of(status) <= sentence,
                    "and never more than it asked for at {width} @ {dpi}"
                );
                assert_eq!(
                    status.right, resting_status.right,
                    "the slot grows leftward: its right edge is fixed"
                );
                if width == 1280 {
                    assert_eq!(
                        width_of(status),
                        sentence,
                        "a window with room grants the whole request at {dpi}"
                    );
                }

                // Asking for more than the window holds is answered with
                // what the title can spare, not with the title's own room.
                let greedy = layout(area, dpi, pane, 4_000);
                let title = greedy.title.expect("the two-pane row always has a title");
                assert!(
                    width_of(title) >= scaled(TITLE_MIN_96, dpi),
                    "the heading keeps its minimum at {width} @ {dpi} ({pane:?})"
                );
                if let Some(count) = resting.count {
                    assert_eq!(
                        greedy.count.map(width_of),
                        Some(width_of(count)),
                        "the length the writer is watching keeps its place"
                    );
                }
            }
        }
    }
}

/// A row with nothing to report says nothing, and a slot held open for a
/// reading that is not there is a gap the name could have used.
#[test]
fn a_silent_row_gives_its_slot_back_to_the_name() {
    for dpi in DPIS {
        for width in [360, 519, 520, 640, 1280] {
            for pane in [PadPane::List, PadPane::Editor] {
                let area = client(width, 520, dpi);
                let silent = layout(area, dpi, pane, 0);
                assert!(
                    silent.status.is_none(),
                    "an empty reading keeps no slot at {width} @ {dpi} ({pane:?})"
                );
                let speaking = layout(area, dpi, pane, 1);
                if let (Some(silent_title), Some(speaking_title)) = (silent.title, speaking.title) {
                    assert!(
                        width_of(silent_title) >= width_of(speaking_title),
                        "the name takes the room the reading is not using at \
                             {width} @ {dpi} ({pane:?})"
                    );
                }
            }
        }
    }
}

#[test]
fn pad_window_constants_keep_requested_logical_geometry() {
    assert_eq!(PAD_WIDTH_LOGICAL, 640);
    assert_eq!(PAD_HEIGHT_LOGICAL, 520);
    assert_eq!(PAD_MIN_WIDTH_LOGICAL, 480);
    assert_eq!(PAD_MIN_HEIGHT_LOGICAL, 360);
}

#[test]
fn utf16_control_limits_are_bounded() {
    assert_eq!(MAX_TITLE_UTF16_UNITS, 256);
    assert_eq!(MAX_BODY_UTF16_UNITS, 65_536);
}

/// The one behavior the whole responsive design rests on: the shape
/// changes at 520 logical pixels of client width, and at every DPI.
#[test]
fn the_two_pane_shape_begins_exactly_at_the_breakpoint() {
    for dpi in DPIS {
        for pane in [PadPane::List, PadPane::Editor] {
            assert!(
                !layout(client(519, 400, dpi), dpi, pane, 0).wide,
                "519 logical px at {dpi} DPI must stay one pane"
            );
            assert!(
                layout(client(520, 400, dpi), dpi, pane, 0).wide,
                "520 logical px at {dpi} DPI must be two panes"
            );
            assert!(
                layout(client(521, 400, dpi), dpi, pane, 0).wide,
                "521 logical px at {dpi} DPI must be two panes"
            );
        }
    }
}

/// A control that overlaps another is a control the user cannot read or
/// click. Exhaustive over the shapes, the boundary and every shipped DPI.
#[test]
fn no_two_controls_overlap_at_any_dpi_or_width() {
    for dpi in DPIS {
        for width in [480, 519, 520, 521, 640, 1280] {
            for height in [360, 520, 900] {
                for pane in [PadPane::List, PadPane::Editor] {
                    for want in wants(dpi) {
                        let plan = layout(client(width, height, dpi), dpi, pane, want);
                        let placed = leaves(&plan);
                        for (index, (name, rect)) in placed.iter().enumerate() {
                            assert!(
                                !is_empty(*rect),
                                "{name} is empty at {width}x{height} @ {dpi} ({pane:?}/{want})"
                            );
                            for (other, other_rect) in &placed[index + 1..] {
                                assert!(
                                        !overlaps(*rect, *other_rect),
                                        "{name} overlaps {other} at {width}x{height} @ {dpi} ({pane:?}/{want})"
                                    );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Nothing may be drawn or placed outside the client area: a clipped
/// control is as unusable as an overlapped one.
#[test]
fn every_control_stays_inside_the_client_area() {
    for dpi in DPIS {
        for width in [480, 520, 640, 1280] {
            for pane in [PadPane::List, PadPane::Editor] {
                for want in wants(dpi) {
                    let area = client(width, 520, dpi);
                    let plan = layout(area, dpi, pane, want);
                    for (name, rect) in leaves(&plan) {
                        assert!(
                            contains(area, rect),
                            "{name} leaves the client at {width} @ {dpi} ({pane:?})"
                        );
                        assert!(
                            contains(area, field_child(rect, dpi)),
                            "{name}'s field child leaves the client at {width} @ {dpi}"
                        );
                    }
                }
            }
        }
    }
}

/// The one-pane shape shows one pane and offers the way back to the other;
/// the two-pane shape shows both and has no use for a toggle.
#[test]
fn each_shape_shows_exactly_the_panes_it_promises() {
    for dpi in DPIS {
        let narrow_list = layout(client(500, 520, dpi), dpi, PadPane::List, 0);
        assert!(narrow_list.header.is_some() && narrow_list.menu.is_some());
        assert!(narrow_list.search.is_some() && narrow_list.list.is_some());
        assert!(narrow_list.title.is_none() && narrow_list.body.is_none());
        assert!(narrow_list.divider.is_none());
        assert!(
            narrow_list.header_title.is_some(),
            "with no memo open the header says which list this is"
        );

        let narrow_editor = layout(client(500, 520, dpi), dpi, PadPane::Editor, 0);
        assert!(narrow_editor.header.is_some() && narrow_editor.menu.is_some());
        assert!(narrow_editor.search.is_none() && narrow_editor.list.is_none());
        assert!(narrow_editor.title.is_some() && narrow_editor.body.is_some());
        assert!(
                narrow_editor.header_title.is_none(),
                "the open memo's own title takes the header, so the list name                  does not also claim it"
            );

        assert!(
            layout(client(500, 520, dpi), dpi, PadPane::Editor, 1)
                .status
                .is_some(),
            "the folded editor has somewhere to report from"
        );
        let roomy = layout(client(1280, 520, dpi), dpi, PadPane::Editor, 1);
        assert!(
            roomy.status.is_some() && roomy.count.is_some(),
            "given room, the editor's row carries both readings"
        );

        for pane in [PadPane::List, PadPane::Editor] {
            let wide = layout(client(720, 520, dpi), dpi, pane, 0);
            assert!(wide.menu.is_none(), "a resident list needs no toggle");
            assert!(
                    wide.header.is_none() && wide.header_title.is_none(),
                    "reaching every memo from the list beside it leaves the                      window's own caption as the only chrome above the panes"
                );
            assert!(wide.search.is_some() && wide.list.is_some());
            assert!(wide.title.is_some() && wide.body.is_some());
            assert!(wide.meta.is_some(), "the editor carries its own first row");
            assert!(wide.divider.is_some());
        }
    }
}

/// Anything the pad draws a face on needs words for the pointer too: an
/// icon alone is exactly what the owner could not read.
#[test]
fn every_drawn_face_has_hover_text() {
    for id in [MENU_ID, NEW_ID, SORT_ID, SYNC_ID, COPY_ID, DELETE_ID] {
        assert!(button_face(id, false).is_some(), "{id} has no face");
        let text = hint(id).unwrap_or_else(|| panic!("{id} has no hint"));
        // A tip repeating the button's own one-word name teaches nothing
        // that looking at the control did not already show.
        assert!(text.chars().count() > 4, "{text}");
    }
    for id in [SEARCH_ID, LIST_ID, TITLE_ID, BODY_ID, STATUS_ID] {
        assert_eq!(hint(id), None, "{id} is not a drawn face");
    }
}

/// The one control the owner misread has to name the format it produces,
/// in both the words it carries and the words it shows.
#[test]
fn the_copy_control_says_markdown() {
    let text = hint(COPY_ID).expect("the copy control has hover text");
    assert!(text.contains("Markdown"), "{text}");
    assert!(text.contains("コピー"), "{text}");
    assert_eq!(
        button_face(COPY_ID, false).map(|face| face.icon),
        Some(PadIcon::Copy)
    );
}

/// Where the memo's own controls live is the difference between the two
/// shapes: beside the memo when there is room, in the bar when there is not.
#[test]
fn copying_and_deleting_follow_the_memo_between_the_shapes() {
    for dpi in DPIS {
        let wide = layout(client(720, 520, dpi), dpi, PadPane::List, 0);
        let meta = wide.meta.expect("the two-pane editor carries a first row");
        assert!(contains(meta, wide.copy));
        assert!(contains(meta, wide.delete));
        assert!(
            wide.bottom.right <= wide.list_rail.expect("a list has a rail").right,
            "the bar belongs to the list column in the two-pane shape"
        );

        let narrow = layout(client(500, 520, dpi), dpi, PadPane::List, 0);
        assert!(contains(narrow.bottom, narrow.copy));
        assert!(contains(narrow.bottom, narrow.delete));
        assert!(
            narrow.delete.left > narrow.copy.right,
            "delete is the far end of the bar, away from the rest"
        );
    }
}

/// The bar, the header and the meta row always hold their own controls.
#[test]
fn every_band_contains_the_controls_it_owns() {
    for dpi in DPIS {
        for width in [480, 520, 1280] {
            for pane in [PadPane::List, PadPane::Editor] {
                let plan = layout(client(width, 520, dpi), dpi, pane, 0);
                // The header exists only in the folded shape, and whatever
                // it holds belongs to it.
                for (name, rect) in [("menu", plan.menu), ("header_title", plan.header_title)] {
                    let Some(rect) = rect else { continue };
                    let header = plan.header.expect("a header control needs a header");
                    assert!(contains(header, rect), "{name} escapes the header");
                }
                assert_eq!(
                    plan.header.is_some(),
                    !plan.wide,
                    "only the folded shape needs chrome above the panes"
                );
                assert!(
                    plan.status.is_none() || plan.wide || pane == PadPane::Editor,
                    "the status line describes the open memo, so it never \
                         shows without one"
                );
                assert!(
                    plan.count.is_none() || plan.wide,
                    "the folded shape has no room for a length beside the \
                         title, and the bar is not where a measurement belongs"
                );
                // Whatever the supports do, the heading keeps its minimum.
                if plan.status.is_some() || plan.count.is_some() {
                    let title = plan.title.expect("a support implies a heading");
                    assert!(
                        title.right - title.left >= scaled(TITLE_MIN_96, dpi),
                        "the heading was squeezed below its minimum at \
                             {width} @ {dpi} ({pane:?})"
                    );
                }
                // Where the memo's own row lives moves with the shape: the
                // editor's first row when there is one, the header when not.
                let row = plan.meta.or(plan.header);
                for (name, rect) in [
                    ("status", plan.status),
                    ("count", plan.count),
                    ("title", plan.meta.and(plan.title)),
                ] {
                    let Some(rect) = rect else { continue };
                    let row = row.expect("a memo row needs a band");
                    assert!(contains(row, rect), "{name} escapes the memo row");
                }
                for (name, rect) in [("new", plan.new), ("sort", plan.sort), ("sync", plan.sync)] {
                    assert!(contains(plan.bottom, rect), "{name} escapes the bar");
                }
            }
        }
    }
}

/// The pad paints a rule at the bottom of every band it owns; the children
/// must not cover it. A child that overlaps a painted edge erases exactly
/// its own span, which is how a full-width line ends up looking like two
/// short ones.
#[test]
fn no_control_covers_the_rules_the_pad_paints() {
    for dpi in [96, 120, 144, 192] {
        let border = scaled(BORDER_96, dpi).max(1);
        for width in [480, 519, 520, 640, 1280] {
            for pane in [PadPane::List, PadPane::Editor] {
                let client = RECT {
                    left: 0,
                    top: 0,
                    right: scaled(width, dpi),
                    bottom: scaled(600, dpi),
                };
                let plan = layout(client, dpi, pane, 0);
                for (band, occupants) in [
                    (
                        plan.header,
                        vec![("menu", plan.menu), ("header title", plan.header_title)],
                    ),
                    (
                        plan.meta,
                        vec![
                            ("title", plan.meta.and(plan.title)),
                            ("status", plan.status),
                            ("count", plan.count),
                            ("copy", plan.meta.map(|_| plan.copy)),
                            ("delete", plan.meta.map(|_| plan.delete)),
                        ],
                    ),
                ] {
                    let Some(band) = band else { continue };
                    let rule_top = band.bottom - border;
                    for (name, rect) in occupants {
                        let Some(rect) = rect else { continue };
                        assert!(
                            rect.bottom <= rule_top,
                            "{name} reaches {} into the rule at {rule_top} \
                                 (dpi {dpi}, width {width}, {pane:?})",
                            rect.bottom
                        );
                    }
                }
                // The bar's rule is painted on its top edge instead.
                let rule_bottom = plan.bottom.top + border;
                for (name, rect) in [("new", plan.new), ("sort", plan.sort), ("sync", plan.sync)] {
                    assert!(
                        rect.top >= rule_bottom,
                        "{name} reaches {} into the bar rule at {rule_bottom} \
                             (dpi {dpi}, width {width}, {pane:?})",
                        rect.top
                    );
                }
            }
        }
    }
}

/// The hint is the field's resting state. It must not survive a caret
/// arriving or a character being typed, or it would sit under both.
/// Paper is where the user's own words go. It is exactly as large as the
/// editor and never reaches under the chrome around it, because the ruled
/// squares showing through a list row or a toolbar would read as damage.
#[test]
fn paper_covers_the_editor_and_stops_at_the_chrome() {
    for dpi in DPIS {
        for width in [360, 519, 520, 640, 900] {
            for pane in [PadPane::List, PadPane::Editor] {
                let plan = layout(client(width, 420, dpi), dpi, pane, 0);
                let Some(paper) = plan.paper else {
                    assert!(
                        plan.body.is_none(),
                        "the editor is showing on no paper at {width} @ {dpi}"
                    );
                    continue;
                };
                let body = plan.body.expect("paper without an editor to hold");
                assert!(
                    contains(paper, body),
                    "the writing area overruns its paper at {width} @ {dpi}"
                );
                if let Some(meta) = plan.meta {
                    assert!(
                        !overlaps(paper, meta),
                        "the head row describes the memo rather than holding it, so \
                             it stands on the chrome at {width} @ {dpi}"
                    );
                }
                for band in [plan.header, plan.list, plan.search, plan.divider]
                    .into_iter()
                    .flatten()
                    .chain([plan.bottom])
                {
                    assert!(
                        !overlaps(paper, band),
                        "paper reaches under the chrome at {width} @ {dpi}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_search_hint_shows_only_while_the_field_is_empty_and_unfocused() {
    assert_eq!(placeholder_text(SEARCH_ID as i32), Some(SEARCH_PLACEHOLDER));
    assert_eq!(placeholder_text(TITLE_ID as i32), Some(TITLE_PLACEHOLDER));
    for quiet in [BODY_ID as i32, LIST_ID as i32, STATUS_ID as i32, -1, 0] {
        assert_eq!(placeholder_text(quiet), None, "id {quiet}");
    }
    assert!(shows_placeholder(0, false));
    assert!(!shows_placeholder(0, true), "the caret is there");
    assert!(!shows_placeholder(1, false), "a query is there");
    assert!(!shows_placeholder(1, true));
}

/// A minimised or degenerate client must produce empty rectangles, never
/// inverted ones: `DeferWindowPos` would happily place a negative size.
#[test]
fn a_degenerate_client_produces_empty_rectangles_not_inverted_ones() {
    for area in [
        RECT::default(),
        RECT {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        },
        RECT {
            left: 40,
            top: 40,
            right: 30,
            bottom: 20,
        },
    ] {
        for dpi in DPIS {
            for pane in [PadPane::List, PadPane::Editor] {
                let plan = layout(area, dpi, pane, 0);
                for (name, rect) in leaves(&plan) {
                    assert!(
                        rect.right >= rect.left && rect.bottom >= rect.top,
                        "{name} inverted for a degenerate client"
                    );
                }
            }
        }
    }
}

/// The pad has no colors of its own. Every one it paints comes from the
/// shared palette, which is what keeps it and the candidate popup one
/// product rather than two that resemble each other.
#[test]
fn the_pad_paints_only_with_the_shared_palette() {
    // Split so the assertion below is not itself a match.
    let literal = concat!("COLORREF", "(0x");
    let call = concat!("rgb", "(");
    let source = include_str!("pad.rs");
    for (number, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        assert!(
            !code.contains(literal) && !code.contains(call),
            "pad.rs:{} names a color of its own: {line}",
            number + 1
        );
    }
    // And the values themselves are the candidate popup's, not a copy.
    // Resolved from explicit inputs, so a machine with high contrast
    // switched on grades the same as one without.
    assert_eq!(
        crate::theme::resolve_palette(AppearanceTheme::Light, false, true),
        crate::theme::light_palette()
    );
    assert_eq!(
        crate::theme::resolve_palette(AppearanceTheme::Dark, false, true),
        crate::theme::dark_palette()
    );
}

/// The sort control cycles and always says which order it is in.
#[test]
fn the_sort_control_names_the_order_it_cycles_to() {
    let mut state_sort = crate::pad_storage::PadSort::default();
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(state_sort.label());
        state_sort = state_sort.next();
    }
    assert_eq!(seen, ["更新順", "作成順", "名前順"]);
    assert_eq!(state_sort, crate::pad_storage::PadSort::default());
}
