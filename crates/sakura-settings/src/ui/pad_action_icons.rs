//! Small GDI line icons used by Sakura Pad action controls.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, DeleteObject, LineTo, MoveToEx, SelectObject, HDC, HGDIOBJ, PS_SOLID,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PadActionIcon {
    Open,
    Lock,
    Protection,
}

/// Draws a crisp, DPI-scaled vector icon inside `rect` using the caller's color.
/// The caller owns the device context and remains responsible for disabled-state colors.
pub(super) fn draw_pad_action_icon(dc: HDC, rect: RECT, kind: PadActionIcon, color: COLORREF) {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 || dc.0.is_null() {
        return;
    }

    let pen_width = (width.min(height) / 16).max(1);
    // SAFETY: `dc` is supplied by the caller for the duration of this draw call.
    unsafe {
        let pen = CreatePen(PS_SOLID, pen_width, color);
        if pen.0.is_null() {
            return;
        }
        let previous = SelectObject(dc, HGDIOBJ(pen.0));
        if previous.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(pen.0));
            return;
        }

        let points: &[&[(i32, i32)]] = match kind {
            // Open notebook: two visible pages and a central binding.
            PadActionIcon::Open => &[
                &[(2, 4), (6, 3), (8, 4), (8, 13), (6, 12), (2, 13), (2, 4)],
                &[(8, 4), (10, 3), (14, 4), (14, 13), (10, 12), (8, 13)],
                &[(4, 6), (6, 6)],
                &[(10, 6), (12, 6)],
            ],
            // Closed shackle over a rounded-looking squared body.
            PadActionIcon::Lock => &[
                &[
                    (4, 7),
                    (4, 5),
                    (5, 3),
                    (7, 2),
                    (9, 2),
                    (11, 3),
                    (12, 5),
                    (12, 7),
                ],
                &[
                    (3, 7),
                    (13, 7),
                    (14, 8),
                    (14, 14),
                    (13, 15),
                    (3, 15),
                    (2, 14),
                    (2, 8),
                    (3, 7),
                ],
                &[(8, 10), (8, 12)],
            ],
            // An empty shield denotes protection settings without implying
            // that the Pad is already protected.
            PadActionIcon::Protection => &[
                &[
                    (8, 1),
                    (14, 3),
                    (14, 7),
                    (13, 10),
                    (11, 13),
                    (8, 15),
                    (5, 13),
                    (3, 10),
                    (2, 7),
                    (2, 3),
                    (8, 1),
                ],
            ],
        };

        for path in points {
            if let Some(&(x, y)) = path.first() {
                let _ = MoveToEx(dc, map(x, rect.left, width), map(y, rect.top, height), None);
                for &(x, y) in &path[1..] {
                    let _ = LineTo(dc, map(x, rect.left, width), map(y, rect.top, height));
                }
            }
        }

        let _ = SelectObject(dc, previous);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

fn map(value: i32, origin: i32, extent: i32) -> i32 {
    origin + value * extent / 16
}
