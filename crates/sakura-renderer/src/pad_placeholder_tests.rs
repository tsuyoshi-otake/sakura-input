use windows::Win32::UI::WindowsAndMessaging::WM_KEYUP;

use super::*;

/// A memo with no title is called the same thing in both panes. If these
/// ever parted, naming a memo `無題` would look like doing nothing while
/// leaving it alone would look like naming it.
#[test]
fn the_title_hint_is_what_the_list_calls_an_untitled_memo() {
    assert_eq!(TITLE_PLACEHOLDER, pad_list::UNTITLED);
}

/// Ctrl+A reaches the three fields that hold text, and nothing else.
#[test]
fn ctrl_a_selects_all_in_the_pads_fields_only() {
    let a = VK_A.0 as usize;
    for id in [SEARCH_ID as i32, TITLE_ID as i32, BODY_ID as i32] {
        assert!(selects_all(WM_KEYDOWN, a, true, false, id), "id {id}");
        assert!(
            !selects_all(WM_KEYDOWN, a, false, false, id),
            "Ctrl was not held"
        );
        assert!(
            !selects_all(WM_KEYDOWN, a, true, true, id),
            "AltGr+A is a character"
        );
        assert!(
            !selects_all(WM_KEYUP, a, true, false, id),
            "the release repeats the press"
        );
        assert!(
            !selects_all(WM_KEYDOWN, VK_A.0 as usize + 1, true, false, id),
            "another key entirely"
        );
    }
    for other in [LIST_ID as i32, NEW_ID as i32, STATUS_ID as i32, -1, 0] {
        assert!(
            !selects_all(WM_KEYDOWN, a, true, false, other),
            "id {other}"
        );
    }
}
