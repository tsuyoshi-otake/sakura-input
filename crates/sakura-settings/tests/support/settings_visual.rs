//! Opt-in screenshots of the real, isolated native payload, using the same
//! physical pointer path as the behavior tests. No user configuration is saved.

use super::*;
use std::path::Path;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};

#[link(name = "user32")]
unsafe extern "system" {
    fn PrintWindow(window: HWND, dc: HDC, flags: u32) -> i32;
}

struct CaptureSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for CaptureSurface {
    fn drop(&mut self) {
        // SAFETY: restore the borrowed object before deleting our own bitmap/DC.
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

fn capture(root: HWND, directory: &Path, name: &str) {
    sleep(INPUT_SETTLING);
    // Paint pending child invalidations before asking the native window to copy
    // its client area. This capture does not depend on taking pointer focus.
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
            Some(root),
            None,
            None,
            windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN
                | windows::Win32::Graphics::Gdi::RDW_UPDATENOW,
        );
    }
    let rect = window_rect(root);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    assert!((1..=4096).contains(&width) && (1..=4096).contains(&height));
    let mut surface = CaptureSurface {
        // SAFETY: creates a compatible memory DC owned by the capture guard.
        dc: unsafe { CreateCompatibleDC(None) },
        bitmap: HBITMAP::default(),
        previous: HGDIOBJ::default(),
    };
    assert!(!surface.dc.is_invalid());
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = null_mut();
    // SAFETY: DIB storage lives until the guard is dropped, after it is written.
    surface.bitmap = unsafe {
        CreateDIBSection(
            Some(surface.dc),
            &info,
            DIB_RGB_COLORS,
            &mut pixels,
            None,
            0,
        )
    }
    .expect("capture DIB");
    // SAFETY: the live DIB is selected into the guard-owned compatible DC.
    surface.previous = unsafe { SelectObject(surface.dc, surface.bitmap.into()) };
    // SAFETY: only the known, live fixture window paints into the memory DC.
    assert_ne!(
        unsafe { PrintWindow(root, surface.dc, 2) },
        0,
        "native capture"
    );
    let length = (width * height * 4) as usize;
    // SAFETY: the DIB dimensions above allocate exactly this many readable bytes.
    let data = unsafe { std::slice::from_raw_parts(pixels.cast::<u8>(), length) };
    let mut bmp = Vec::with_capacity(54 + length);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + length) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-height).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    bmp.extend_from_slice(data);
    std::fs::write(directory.join(format!("{name}.bmp")), bmp).expect("write native screenshot");
    let mut report = format!("window={width}x{height} dpi={}\n", unsafe {
        // SAFETY: read-only query on the live fixture.
        GetDpiForWindow(root)
    });
    fn visit(window: HWND, report: &mut String) {
        for child in direct_children(window) {
            if is_visible(child) {
                // Never record edit values (including the user's registry-backed AI endpoint).
                let class = class_name(child);
                let text = if class == "Static" || class == "Button" {
                    status_text(child)
                } else {
                    String::new()
                };
                report.push_str(&format!("{class} {:?} {text}\n", window_rect(child)));
                visit(child, report);
            }
        }
    }
    visit(root, &mut report);
    std::fs::write(directory.join(format!("{name}.txt")), report).expect("write control evidence");
}

fn leaf_items(tree: HWND, first: HTREEITEM, output: &mut Vec<HTREEITEM>) {
    let mut item = first;
    while item.0 != 0 {
        let child = tree_item_relative(tree, TVGN_CHILD as usize, item);
        if child.0 == 0 {
            output.push(item);
        } else {
            leaf_items(tree, child, output);
        }
        item = tree_item_relative(tree, TVGN_NEXT as usize, item);
    }
}

#[test]
#[ignore = "native visual capture; requires SAKURA_SETTINGS_SCREENSHOTS"]
fn capture_initial_compact_settings() {
    let _desktop = desktop_test_guard();
    let output = PathBuf::from(
        std::env::var_os("SAKURA_SETTINGS_SCREENSHOTS").expect("screenshot directory"),
    );
    std::fs::create_dir_all(&output).expect("create screenshot directory");
    let _foreground = ForegroundRestore::capture();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    sleep(INPUT_SETTLING);
    capture(root, &output, "basic");
}

/// Rendering evidence uses native selection messages so it can run while the
/// owner uses the mouse. Physical pointer/keyboard behavior has separate tests.
#[test]
#[ignore = "native visual matrix; requires SAKURA_SETTINGS_SCREENSHOTS"]
fn capture_settings_layout_matrix() {
    use windows::Win32::UI::Controls::TVM_SELECTITEM;
    use windows::Win32::UI::WindowsAndMessaging::{
        CBN_SELCHANGE, CB_SETCURSEL, LBN_SELCHANGE, LB_SETCURSEL, WM_COMMAND, WM_VSCROLL,
    };
    let _desktop = desktop_test_guard();
    let output = PathBuf::from(
        std::env::var_os("SAKURA_SETTINGS_SCREENSHOTS").expect("screenshot directory"),
    );
    std::fs::create_dir_all(&output).expect("create screenshot directory");
    let _foreground = ForegroundRestore::capture();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let tree = find_direct_child(root, "SysTreeView32").expect("tree");
    let topics = find_direct_child(root, "ListBox").expect("topic list");
    let mut items = Vec::new();
    leaf_items(
        tree,
        tree_item_relative(tree, TVGN_ROOT as usize, HTREEITEM::default()),
        &mut items,
    );
    let display = input_topic_panel_with_heading(root, "表示");
    let theme = find_direct_child(display, "ComboBox").expect("theme combo");
    for (theme_name, index) in [("light", 1), ("dark", 2)] {
        unsafe {
            let _ = SendMessageW(theme, CB_SETCURSEL, Some(WPARAM(index)), None);
            let _ = SendMessageW(
                root,
                WM_COMMAND,
                Some(WPARAM((CBN_SELCHANGE as usize) << 16)),
                Some(LPARAM(theme.0 as isize)),
            );
        }
        select_native_category(root, 0);
        for (index, item) in items.iter().enumerate() {
            unsafe {
                let _ = SendMessageW(
                    tree,
                    TVM_SELECTITEM,
                    Some(WPARAM(TVGN_CARET as usize)),
                    Some(LPARAM(item.0)),
                );
            }
            assert_eq!(selected_input_tree_item(tree), *item);
            capture(root, &output, &format!("{theme_name}-input-{index:02}"));
            if matches!(index, 2 | 4 | 6 | 10) {
                let viewport = input_topic_outer(root);
                unsafe {
                    let _ = SendMessageW(viewport, WM_VSCROLL, Some(WPARAM(7)), None);
                }
                capture(
                    root,
                    &output,
                    &format!("{theme_name}-input-{index:02}-bottom"),
                );
            }
        }
        for (category, (name, heading, count)) in [
            ("辞書", "ユーザー辞書", 2),
            ("学習", "学習履歴", 2),
            ("診断", "詳細設定・診断", 1),
            ("更新", "更新の確認", 3),
        ]
        .into_iter()
        .enumerate()
        {
            select_native_category(root, category + 1);
            let outer = page_outer_with_topic(root, heading);
            assert!(is_visible(outer), "{name} category must be visible");
            for index in 0..count {
                unsafe {
                    let _ = SendMessageW(topics, LB_SETCURSEL, Some(WPARAM(index)), None);
                    let _ = SendMessageW(
                        root,
                        WM_COMMAND,
                        Some(WPARAM((LBN_SELCHANGE as usize) << 16)),
                        Some(LPARAM(topics.0 as isize)),
                    );
                }
                assert_eq!(list_value(topics, LB_GETCURSEL), index);
                capture(root, &output, &format!("{theme_name}-{name}-{index:02}"));
                unsafe {
                    let _ = SendMessageW(outer, WM_VSCROLL, Some(WPARAM(7)), None);
                }
                capture(
                    root,
                    &output,
                    &format!("{theme_name}-{name}-{index:02}-bottom"),
                );
            }
        }
    }
}

fn select_native_category(root: HWND, index: usize) {
    use windows::Win32::UI::Controls::TCM_GETCURSEL;
    let tabs = find_direct_child(root, "SysTabControl32").expect("native category tabs");
    for _ in 0..5 {
        let selected = unsafe { SendMessageW(tabs, TCM_GETCURSEL, None, None) }.0 as usize;
        if selected == index {
            return;
        }
        let key = if selected < index { 0x27 } else { 0x25 };
        // Native tabs own the arrow-to-selection notification path. This sends
        // no borrowed cross-process notification pointer and moves no cursor.
        unsafe {
            let _ = SendMessageW(
                tabs,
                windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN,
                Some(WPARAM(key)),
                None,
            );
        }
    }
    assert_eq!(
        unsafe { SendMessageW(tabs, TCM_GETCURSEL, None, None) }.0 as usize,
        index
    );
}

#[test]
#[ignore = "requires a native Windows UI Automation desktop"]
fn native_tabs_expose_selected_names_and_keyboard_focus_to_uia() {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationSelectionPattern, UIA_SelectionPatternId,
        UIA_TabControlTypeId,
    };
    let _desktop = desktop_test_guard();
    let _foreground = ForegroundRestore::capture();
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .expect("initialize UIA apartment");
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _apartment = Apartment;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .expect("UIA client");
    let tabs = find_direct_child(root, "SysTabControl32").expect("native tabs");
    let element = unsafe { automation.ElementFromHandle(tabs) }.expect("native tab provider");
    assert_eq!(
        unsafe { element.CurrentControlType() }.expect("tab control type"),
        UIA_TabControlTypeId
    );
    assert!(unsafe { element.CurrentIsKeyboardFocusable() }
        .expect("tab focusability")
        .as_bool());
    let selection: IUIAutomationSelectionPattern =
        unsafe { element.GetCurrentPatternAs(UIA_SelectionPatternId) }.expect("selection provider");
    for (index, name) in ["入力・変換", "辞書", "学習", "診断", "更新"]
        .into_iter()
        .enumerate()
    {
        select_native_category(root, index);
        let selected = unsafe { selection.GetCurrentSelection() }.expect("selected tabs");
        assert_eq!(unsafe { selected.Length() }.expect("selection length"), 1);
        let item = unsafe { selected.GetElement(0) }.expect("selected item");
        assert_eq!(
            unsafe { item.CurrentName() }
                .expect("selected name")
                .to_string(),
            name
        );
    }
}

#[test]
#[ignore = "requires an interactive desktop and SAKURA_SETTINGS_SCREENSHOTS output directory"]
fn capture_native_settings_pages() {
    use windows::Win32::UI::Controls::TVM_SELECTITEM;
    let _desktop = desktop_test_guard();
    let output = PathBuf::from(
        std::env::var_os("SAKURA_SETTINGS_SCREENSHOTS").expect("screenshot directory"),
    );
    std::fs::create_dir_all(&output).expect("create screenshot directory");
    let fixture = SettingsFixture::launch();
    let root = fixture.wait_for_window();
    let _foreground = ForegroundRestore::capture();
    let cursor = CursorRestore::capture();
    raise_fixture_for_input(root);
    let tree = find_direct_child(root, "SysTreeView32").expect("input tree");
    let mut items = Vec::new();
    leaf_items(
        tree,
        tree_item_relative(tree, TVGN_ROOT as usize, HTREEITEM::default()),
        &mut items,
    );
    for (index, item) in items.into_iter().enumerate() {
        ensure_tree_item_visible(tree, item);
        unsafe {
            let _ = SendMessageW(
                tree,
                TVM_SELECTITEM,
                Some(WPARAM(TVGN_CARET as usize)),
                Some(LPARAM(item.0)),
            );
        }
        wait_until(&format!("selected captured input topic {index}"), || {
            selected_input_tree_item(tree) == item
        });
        sleep(INPUT_SETTLING);
        capture(root, &output, &format!("input-{index:02}"));
    }
    for (category, (name, count)) in [("辞書", 2), ("学習", 2), ("診断", 1), ("更新", 3)]
        .into_iter()
        .enumerate()
    {
        click_category_tab(&cursor, root, category + 1);
        let topics = find_direct_child(root, "ListBox").expect("topic list");
        wait_until("category topics shown", || is_visible(topics));
        for index in 0..count {
            click_topic_item(&cursor, topics, index);
            wait_until("selected captured topic", || {
                list_value(topics, LB_GETCURSEL) == index
            });
            sleep(INPUT_SETTLING);
            capture(root, &output, &format!("{name}-{index:02}"));
        }
    }
}
