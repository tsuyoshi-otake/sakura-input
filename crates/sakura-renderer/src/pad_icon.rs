//! The pad's pictograms, drawn rather than typed.
//!
//! The bar's controls are icons in the design, and the obvious way to get an
//! icon without a resource file is to put a symbol character in the button's
//! text. That does not survive contact with Windows font fallback: the shipped
//! UI font does not carry every arrow the design wants, so the emoji font wins
//! the fallback for those and the bar grows two colored pictures among four
//! monochrome ones. A variation selector does not help — fallback happens
//! because the glyph is absent, not because a presentation was requested.
//!
//! So the pad draws them. Each icon is a handful of strokes on a 32-unit
//! square that is scaled into whatever box the caller gives it, which means
//! one set of coordinates describes the whole set at every DPI, the strokes
//! resolve through [`crate::theme`] like everything else the pad paints, and
//! no glyph can arrive in a color the palette did not choose.
//!
//! # Why the icons are drawn four times too large
//!
//! GDI has no antialiasing. `LineTo` and `Polygon` set whole pixels, so a
//! circle eighteen pixels across comes out as a lumpy polygon and a diagonal
//! comes out as a staircase — at this size that is most of what the eye sees.
//!
//! Each icon is therefore drawn at [`SUPERSAMPLE`] times its final size onto
//! an offscreen sheet, and the sheet is averaged down into a premultiplied
//! BGRA stamp that is `AlphaBlend`ed onto the caller's DC. Averaging sixteen
//! hard pixels into one is exactly the coverage an antialiasing rasterizer
//! would have computed, so the strokes land with soft edges, curves read as
//! curves, and the geometry can be described in real numbers instead of being
//! snapped to whichever pixels happened to be available.
//!
//! The alternative was GDI+, which antialiases natively. It is a bigger
//! runtime in a process that has a ten-mebibyte private-working-set budget,
//! and it would have been a second drawing model beside the GDI the rest of
//! the pad uses. A 72-by-72 scratch bitmap is cheaper than either.
//!
//! The drawn shape is not the accessible name. Every button keeps real window
//! text — `削除`, not a symbol — so UI Automation reads the pad's controls out
//! as words whatever is painted on them.

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::Win32::Foundation::{COLORREF, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    ExtCreatePen, LineTo, MoveToEx, Polygon, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, BS_SOLID, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    LOGBRUSH, PS_ENDCAP_ROUND, PS_GEOMETRIC, PS_JOIN_ROUND, PS_SOLID,
};

use crate::theme::scaled;

/// The square every icon below is described on.
const GRID: f32 = 32.0;

/// How much larger than its final size an icon is drawn before being averaged
/// down. Four is sixteen samples per finished pixel, which is enough that a
/// stroke edge steps in gradations the eye reads as a smooth line.
const SUPERSAMPLE: i32 = 4;

/// A stroke's width, in grid units.
///
/// Slightly over a pixel at 96 DPI, which is about the stem of the UI font at
/// the same size — a drawn face has to sit at the weight of the text around
/// it, and antialiased strokes read heavier than aliased ones did at the same
/// width because every one of their pixels is covered rather than snapped.
const STROKE: f32 = 2.0;

/// How big an icon is at 96 DPI.
///
/// Small enough to sit inside a 32-logical-pixel button with the frame still
/// reading as a frame, large enough that a three-stroke shape is still three
/// distinguishable strokes.
pub(crate) const ICON_96: i32 = 18;

/// What the pad's controls show.
///
/// One variant per thing the pad does, not one per shape: `Copy` is two sheets
/// because the control copies the memo, and `Trash` is the only icon whose
/// caller paints it in the danger role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PadIcon {
    Menu,
    Search,
    Plus,
    Sort,
    Sync,
    Copy,
    Trash,
}

/// The side of an icon's box at this DPI.
pub(crate) fn size(dpi: u32) -> i32 {
    scaled(ICON_96, dpi).max(8)
}

/// Centers an icon's square box inside `within`.
pub(crate) fn box_in(within: RECT, dpi: u32) -> RECT {
    let side = size(dpi)
        .min(within.right.saturating_sub(within.left))
        .min(within.bottom.saturating_sub(within.top))
        .max(0);
    let left = within.left + (within.right.saturating_sub(within.left) - side) / 2;
    let top = within.top + (within.bottom.saturating_sub(within.top) - side) / 2;
    RECT {
        left,
        top,
        right: left + side,
        bottom: top + side,
    }
}

/// Draws `icon` inside `bounds` in `color`.
///
/// `bounds` is the icon's own square box, not the control's rectangle; use
/// [`box_in`] to get one from a control. Nothing is drawn if any of the
/// offscreen objects cannot be made: a missing icon is a worse answer than a
/// blank button, but a half-drawn one on a DC left in a foreign state is worse
/// than both.
pub(crate) fn draw(dc: HDC, bounds: RECT, icon: PadIcon, color: COLORREF) {
    let side = bounds
        .right
        .saturating_sub(bounds.left)
        .min(bounds.bottom.saturating_sub(bounds.top));
    if side < 4 {
        return;
    }
    let Some(sheet) = Surface::new(dc, side * SUPERSAMPLE) else {
        return;
    };
    let Some(stamp) = Surface::new(dc, side) else {
        return;
    };
    if !sheet.paint(icon) {
        return;
    }
    sheet.resolve_into(&stamp, color);
    stamp.blend(dc, bounds.left, bounds.top);
}

/// One square 32-bit top-down DIB and the memory DC it is selected into.
///
/// Both the oversized sheet an icon is drawn on and the finished stamp that is
/// blended out of are this: the difference is only which one is written by GDI
/// and which one by the loop below.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    side: i32,
}

impl Surface {
    fn new(reference: HDC, side: i32) -> Option<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: side,
                // Negative height is a top-down bitmap, so row zero is the top
                // one and the loops below can index it the way they read.
                biHeight: -side,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        // SAFETY: the header describes the exact allocation being asked for,
        // `bits` receives a pointer GDI owns for as long as the bitmap does,
        // and every object made here is released in `Drop` on every path.
        unsafe {
            let dc = CreateCompatibleDC(Some(reference));
            if dc.is_invalid() {
                return None;
            }
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            else {
                let _ = DeleteDC(dc);
                return None;
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                return None;
            }
            let previous = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast::<u32>(),
                side,
            })
        }
    }

    fn pixels(&self) -> usize {
        (self.side as usize) * (self.side as usize)
    }

    /// Draws `icon` onto this surface as black on white.
    ///
    /// Color belongs to the stamp, not to the sheet: what is wanted here is
    /// coverage, and coverage is the same shape whatever it is later painted
    /// in.
    fn paint(&self, icon: PadIcon) -> bool {
        // SAFETY: the bitmap is this surface's own, was created with exactly
        // this many pixels, and nothing else holds the pointer.
        unsafe {
            std::slice::from_raw_parts_mut(self.bits, self.pixels()).fill(0x00FF_FFFF);
        }
        let brush = LOGBRUSH {
            lbStyle: BS_SOLID,
            lbColor: COLORREF(0),
            lbHatch: 0,
        };
        let width = ((STROKE * self.side as f32 / GRID).round() as i32).max(1) as u32;
        // SAFETY: the log brush lives across the call, and the pen is selected
        // into this surface's own DC and deleted before returning.
        let pen = unsafe {
            ExtCreatePen(
                PS_GEOMETRIC | PS_SOLID | PS_ENDCAP_ROUND | PS_JOIN_ROUND,
                width,
                &brush,
                None,
            )
        };
        if pen.is_invalid() {
            return false;
        }
        // SAFETY: the brush is this scope's own, both objects are selected
        // into this surface's own DC, and both are restored and deleted below
        // on every path out of this function.
        unsafe {
            let fill = CreateSolidBrush(COLORREF(0));
            let restore_pen = SelectObject(self.dc, pen.into());
            let restore_fill = SelectObject(self.dc, fill.into());
            figures(
                icon,
                &Ink {
                    dc: self.dc,
                    scale: self.side as f32 / GRID,
                },
            );
            SelectObject(self.dc, restore_fill);
            SelectObject(self.dc, restore_pen);
            let _ = DeleteObject(fill.into());
            let _ = DeleteObject(pen.into());
        }
        true
    }

    /// Averages this surface down into `stamp`, tinting it with `color`.
    fn resolve_into(&self, stamp: &Surface, color: COLORREF) {
        let ratio = (self.side / stamp.side).max(1);
        let samples = (ratio * ratio) as u32;
        // SAFETY: both bitmaps are owned by their surfaces and sized exactly
        // as described; neither slice outlives this call.
        let (sheet, target) = unsafe {
            (
                std::slice::from_raw_parts(self.bits, self.pixels()),
                std::slice::from_raw_parts_mut(stamp.bits, stamp.pixels()),
            )
        };
        for y in 0..stamp.side {
            for x in 0..stamp.side {
                let mut sum = 0_u32;
                for dy in 0..ratio {
                    for dx in 0..ratio {
                        let row = (y * ratio + dy) as usize * self.side as usize;
                        let pixel = sheet[row + (x * ratio + dx) as usize];
                        // White ground, black ink: the low byte is coverage
                        // read the other way round.
                        sum += 255 - (pixel & 0xFF);
                    }
                }
                target[(y * stamp.side + x) as usize] = premultiply(color, (sum / samples) as u8);
            }
        }
    }

    /// Blends this surface over whatever `dc` already holds.
    fn blend(&self, dc: HDC, x: i32, y: i32) {
        // SAFETY: both DCs are live for the call, the source is this surface's
        // own bitmap, and the blend reads exactly the rectangle it was given.
        unsafe {
            let _ = AlphaBlend(
                dc,
                x,
                y,
                self.side,
                self.side,
                self.dc,
                0,
                0,
                self.side,
                self.side,
                BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                },
            );
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: every object here was made by `new` and is still owned.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// One pixel of `color` at `alpha`, in the premultiplied BGRA that
/// `AlphaBlend` reads.
fn premultiply(color: COLORREF, alpha: u8) -> u32 {
    let alpha = alpha as u32;
    let scale = |channel: u32| (channel * alpha + 127) / 255;
    let red = scale(color.0 & 0xFF);
    let green = scale((color.0 >> 8) & 0xFF);
    let blue = scale((color.0 >> 16) & 0xFF);
    (alpha << 24) | (red << 16) | (green << 8) | blue
}

/// Draws grid-space figures onto a surface's DC.
struct Ink {
    dc: HDC,
    scale: f32,
}

impl Ink {
    fn at(&self, point: (f32, f32)) -> POINT {
        POINT {
            x: (point.0 * self.scale).round() as i32,
            y: (point.1 * self.scale).round() as i32,
        }
    }

    /// Draws a connected run of segments. A single segment is a two-point run.
    fn stroke(&self, points: &[(f32, f32)]) {
        let Some((first, rest)) = points.split_first() else {
            return;
        };
        let start = self.at(*first);
        // SAFETY: the DC is live for the whole call and the pen is selected.
        unsafe {
            let _ = MoveToEx(self.dc, start.x, start.y, None);
            for point in rest {
                let next = self.at(*point);
                let _ = LineTo(self.dc, next.x, next.y);
            }
        }
    }

    fn fill(&self, points: &[(f32, f32)]) {
        let mapped: Vec<POINT> = points.iter().map(|point| self.at(*point)).collect();
        // SAFETY: as above; `Polygon` reads exactly `mapped.len()` vertices.
        unsafe {
            let _ = Polygon(self.dc, &mapped);
        }
    }
}

/// Appends an arc, walking from `from` to `to` degrees. Zero degrees points
/// right and the angle grows clockwise, because the y axis points down.
fn arc(into: &mut Vec<(f32, f32)>, center: (f32, f32), radius: f32, from: f32, to: f32) {
    // Eight degrees a segment: at four times eighteen pixels the chord and the
    // curve differ by well under the width of the stroke covering them.
    let steps = (((to - from).abs() / 8.0).ceil() as i32).max(1);
    for step in 0..=steps {
        let degrees = from + (to - from) * (step as f32 / steps as f32);
        let (sin, cos) = degrees.to_radians().sin_cos();
        into.push((center.0 + radius * cos, center.1 + radius * sin));
    }
}

/// A closed rounded rectangle, corners first-quadrant clockwise from the top
/// left.
fn rounded_rect(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> Vec<(f32, f32)> {
    let mut points = Vec::with_capacity(32);
    arc(
        &mut points,
        (right - radius, top + radius),
        radius,
        270.0,
        360.0,
    );
    arc(
        &mut points,
        (right - radius, bottom - radius),
        radius,
        0.0,
        90.0,
    );
    arc(
        &mut points,
        (left + radius, bottom - radius),
        radius,
        90.0,
        180.0,
    );
    arc(
        &mut points,
        (left + radius, top + radius),
        radius,
        180.0,
        270.0,
    );
    if let Some(first) = points.first().copied() {
        points.push(first);
    }
    points
}

fn figures(icon: PadIcon, ink: &Ink) {
    match icon {
        // Three rules, the same weight as the rules the pad paints between
        // its bands.
        PadIcon::Menu => {
            for y in [10.0, 16.0, 22.0] {
                ink.stroke(&[(7.0, y), (25.0, y)]);
            }
        }
        // A ring and a handle at forty-five degrees, which is the one angle a
        // magnifier is ever drawn at.
        PadIcon::Search => {
            let mut ring = Vec::with_capacity(48);
            arc(&mut ring, (14.0, 14.0), 7.0, 0.0, 360.0);
            ink.stroke(&ring);
            ink.stroke(&[(19.2, 19.2), (26.0, 26.0)]);
        }
        PadIcon::Plus => {
            ink.stroke(&[(16.0, 7.0), (16.0, 25.0)]);
            ink.stroke(&[(7.0, 16.0), (25.0, 16.0)]);
        }
        // Up on the left, down on the right: the pair reads as an order, not
        // as two separate directions.
        PadIcon::Sort => {
            ink.stroke(&[(10.5, 25.0), (10.5, 7.5)]);
            ink.stroke(&[(6.0, 12.0), (10.5, 7.5), (15.0, 12.0)]);
            ink.stroke(&[(21.5, 7.0), (21.5, 24.5)]);
            ink.stroke(&[(17.0, 20.0), (21.5, 24.5), (26.0, 20.0)]);
        }
        // A ring broken at the top right, travelling clockwise, with the head
        // at the end of the sweep. Clockwise is the direction every refresh
        // control on this desktop turns.
        PadIcon::Sync => {
            let center = (16.0, 16.5);
            let radius = 9.0;
            let mut ring = Vec::with_capacity(48);
            arc(&mut ring, center, radius, 340.0, 625.0);
            ink.stroke(&ring);
            // At the top of the circle the clockwise tangent points right, so
            // the head is a triangle straddling the ring and aimed that way.
            let tip = (center.0, center.1 - radius);
            ink.fill(&[
                (tip.0 + 3.6, tip.1),
                (tip.0 - 0.8, tip.1 - 3.3),
                (tip.0 - 0.8, tip.1 + 3.3),
            ]);
        }
        // Two sheets, the back one showing only the edges the front one does
        // not cover. This is the one control whose meaning the owner found
        // unreadable as an outbound arrow: an arrow says the memo leaves, and
        // it does not — it is copied where it is.
        PadIcon::Copy => {
            ink.stroke(&rounded_rect(5.5, 11.0, 20.5, 26.5, 2.5));
            let mut back = vec![(11.0, 5.5), (23.0, 5.5)];
            arc(&mut back, (23.0, 8.0), 2.5, 270.0, 360.0);
            back.push((25.5, 19.5));
            ink.stroke(&back);
        }
        // Lid, handle, tapered body, two ribs. The ribs were left out while
        // the icons were aliased, because at one hard pixel each they closed
        // up into a smudge; averaged down they stay two lines.
        PadIcon::Trash => {
            ink.stroke(&[(5.5, 9.0), (26.5, 9.0)]);
            ink.stroke(&[(12.0, 9.0), (12.0, 5.5), (20.0, 5.5), (20.0, 9.0)]);
            ink.stroke(&[(8.5, 9.0), (10.5, 26.5), (21.5, 26.5), (23.5, 9.0)]);
            ink.stroke(&[(13.5, 13.0), (13.5, 22.5)]);
            ink.stroke(&[(18.5, 13.0), (18.5, 22.5)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(width: i32, height: i32) -> RECT {
        RECT {
            left: 40,
            top: 10,
            right: 40 + width,
            bottom: 10 + height,
        }
    }

    /// The box is square, centered, and never larger than what it is put in —
    /// an icon that overhangs its button paints over the frame beside it.
    #[test]
    fn the_box_is_square_and_inside_its_control() {
        for dpi in [96, 120, 144, 168, 192, 240] {
            for width in [10, 18, 24, 32, 48, 96] {
                for height in [10, 18, 24, 32, 48, 96] {
                    let within = client(width, height);
                    let box_of = box_in(within, dpi);
                    let side = box_of.right - box_of.left;
                    assert_eq!(side, box_of.bottom - box_of.top, "{dpi} {width}x{height}");
                    assert!(side <= width.min(height), "{dpi} {width}x{height}");
                    assert!(box_of.left >= within.left && box_of.right <= within.right);
                    assert!(box_of.top >= within.top && box_of.bottom <= within.bottom);
                }
            }
        }
    }

    /// The stamp is what `AlphaBlend` reads, and it reads premultiplied: a
    /// channel above its own alpha is the one input that makes it draw
    /// garbage rather than something merely wrong.
    #[test]
    fn a_stamp_pixel_is_never_brighter_than_its_alpha() {
        for color in [0x00_0000, 0xFF_FFFF, 0x12_3456, 0x00_80FF, 0xAB_CDEF] {
            for alpha in 0..=255_u8 {
                let pixel = premultiply(COLORREF(color), alpha);
                let stored = (pixel >> 24) as u8;
                assert_eq!(stored, alpha, "alpha survives the round trip");
                for shift in [0, 8, 16] {
                    assert!(
                        ((pixel >> shift) & 0xFF) as u8 <= alpha,
                        "{color:#08x} at {alpha}"
                    );
                }
            }
        }
    }

    /// Fully covered pixels have to come out as the color that was asked for,
    /// or every solid part of every icon is off by a rounding error.
    #[test]
    fn full_coverage_is_the_color_itself() {
        for color in [0x00_0000, 0xFF_FFFF, 0x12_3456, 0x2F_2F2F] {
            let pixel = premultiply(COLORREF(color), 255);
            let red = pixel >> 16 & 0xFF;
            let green = pixel >> 8 & 0xFF;
            let blue = pixel & 0xFF;
            assert_eq!(red, color & 0xFF);
            assert_eq!(green, color >> 8 & 0xFF);
            assert_eq!(blue, color >> 16 & 0xFF);
        }
        assert_eq!(premultiply(COLORREF(0xFF_FFFF), 0), 0);
    }

    /// Every point of an arc is on its circle, and the run is dense enough
    /// that the chords cannot be seen through the stroke covering them.
    #[test]
    fn an_arc_stays_on_its_circle() {
        for (from, to) in [(0.0, 360.0), (340.0, 625.0), (270.0, 360.0), (90.0, 90.0)] {
            let mut points = Vec::new();
            arc(&mut points, (16.0, 16.5), 9.0, from, to);
            assert!(points.len() >= 2 || from == to);
            for point in &points {
                let radius = ((point.0 - 16.0).powi(2) + (point.1 - 16.5).powi(2)).sqrt();
                assert!((radius - 9.0).abs() < 0.001, "{point:?}");
            }
            for pair in points.windows(2) {
                let chord =
                    ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt();
                assert!(chord < STROKE, "a chord wider than the stroke would show");
            }
        }
    }

    /// A rounded rectangle has to close, or the stroke leaves a notch where
    /// the path began.
    #[test]
    fn a_rounded_rectangle_closes_on_itself() {
        let path = rounded_rect(5.5, 11.0, 20.5, 26.5, 2.5);
        assert_eq!(path.first(), path.last());
        for point in &path {
            assert!(
                point.0 >= 5.5 - 0.001 && point.0 <= 20.5 + 0.001,
                "{point:?}"
            );
            assert!(
                point.1 >= 11.0 - 0.001 && point.1 <= 26.5 + 0.001,
                "{point:?}"
            );
        }
    }
}
