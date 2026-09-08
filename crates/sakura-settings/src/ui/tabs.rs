//! Standard tab navigation, with dark painting over the same native geometry.
//! User32 still owns hit testing, arrow keys, focus, selection and accessibility.
use super::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, SelectObject, HGDIOBJ, PAINTSTRUCT};
use windows::Win32::UI::Controls::{TCM_GETITEMRECT, TCM_GETITEMW};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{WM_GETFONT, WM_NCDESTROY, WM_PAINT};

pub(super) fn install(window: HWND) -> WindowsResult<()> {
    // SAFETY: the callback stores no borrowed state and is removed at teardown.
    if unsafe { SetWindowSubclass(window, Some(procedure), 144, 0) }.as_bool() {
        Ok(())
    } else {
        Err(windows::core::Error::from_thread())
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        // SAFETY: this is the subclass's terminal notification for `window`;
        // removing the exact registered callback prevents later dispatch.
        unsafe {
            let _ = RemoveWindowSubclass(window, Some(procedure), 144);
        }
    }
    // SAFETY: the subclass receives a live tab HWND and only queries its
    // User32 parent; no ownership is transferred.
    let pointer = unsafe { GetParent(window) }
        .ok()
        .map(|root| {
            // SAFETY: the root stores the App pointer on this same UI thread.
            (unsafe { GetWindowLongPtrW(root, GWLP_USERDATA) }) as *const App
        })
        .unwrap_or(std::ptr::null());
    if !pointer.is_null() {
        // SAFETY: the pointer is installed for the root App lifetime and this
        // callback runs synchronously on its owning UI thread.
        let app = unsafe { &*pointer };
        if app.theme.dark && !app.theme.high_contrast {
            if message == WM_ERASEBKGND {
                return LRESULT(1);
            }
            if message == WM_PAINT {
                let mut paint = PAINTSTRUCT::default();
                // SAFETY: WM_PAINT supplies a live window; `paint` remains live
                // until the matching EndPaint below.
                let dc = unsafe { BeginPaint(window, &mut paint) };
                draw(window, dc, app);
                // SAFETY: balances the successful BeginPaint in this branch.
                unsafe {
                    let _ = EndPaint(window, &paint);
                }
                return LRESULT(0);
            }
            if message == windows::Win32::UI::WindowsAndMessaging::WM_PRINTCLIENT {
                draw(window, HDC(wparam.0 as *mut c_void), app);
                return LRESULT(0);
            }
        }
    }
    // SAFETY: unhandled messages and untouched scalar parameters are forwarded
    // exactly once to the native subclass chain.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

fn draw(window: HWND, dc: HDC, app: &App) {
    let Some(brushes) = app.theme.brushes.as_ref() else {
        return;
    };
    // SAFETY: paint/print supplies a live DC and the native tab synchronously
    // copies its item geometry and labels into bounded local buffers.
    unsafe {
        let mut bounds = RECT::default();
        let _ = GetClientRect(window, &mut bounds);
        let _ = FillRect(dc, &bounds, brushes.surface);
        let selected = SendMessageW(window, TCM_GETCURSEL, None, None).0;
        let font = SendMessageW(window, WM_GETFONT, None, None);
        let previous = SelectObject(dc, HGDIOBJ(font.0 as *mut c_void));
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, app.theme.ink());
        for index in 0..PANEL_COUNT {
            let mut rect = RECT::default();
            if SendMessageW(
                window,
                TCM_GETITEMRECT,
                Some(WPARAM(index)),
                Some(LPARAM((&mut rect as *mut RECT) as isize)),
            )
            .0 == 0
            {
                continue;
            }
            if index == 0 {
                let body = RECT {
                    top: rect.bottom,
                    ..bounds
                };
                let _ = FrameRect(dc, &body, brushes.button_border);
            }
            let active = selected == index as isize;
            let fill = if active {
                brushes.surface
            } else {
                brushes.button
            };
            let _ = FillRect(dc, &rect, fill);
            let _ = FrameRect(dc, &rect, brushes.button_border);
            if active {
                let join = RECT {
                    left: rect.left + 1,
                    top: rect.bottom - 1,
                    right: rect.right - 1,
                    bottom: rect.bottom + 1,
                };
                let _ = FillRect(dc, &join, brushes.surface);
            }
            let mut label = [0u16; 64];
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: PWSTR(label.as_mut_ptr()),
                cchTextMax: label.len() as i32,
                ..Default::default()
            };
            if SendMessageW(
                window,
                TCM_GETITEMW,
                Some(WPARAM(index)),
                Some(LPARAM((&mut item as *mut TCITEMW) as isize)),
            )
            .0 != 0
            {
                let length = label
                    .iter()
                    .position(|value| *value == 0)
                    .unwrap_or(label.len());
                let _ = DrawTextW(
                    dc,
                    &mut label[..length],
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }
            if active && windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() == window {
                let focus = RECT {
                    left: rect.left + 3,
                    top: rect.top + 3,
                    right: rect.right - 3,
                    bottom: rect.bottom - 3,
                };
                let _ = FrameRect(dc, &focus, brushes.accent);
            }
        }
        let _ = SelectObject(dc, previous);
    }
}
