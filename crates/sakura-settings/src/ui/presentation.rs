//! Settings presentation: logical geometry, font ownership and native form rows.
//!
//! Business operations and persisted values remain in the settings controller.
//! Every page declares one content extent; resizing only lays out the visible
//! page (O(visible controls)), and scrolling never creates or destroys controls.

use super::*;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
    FONT_WEIGHT, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    GetScrollInfo, CB_SETITEMHEIGHT, SB_HORZ, SB_VERT, SCROLLINFO, SIF_ALL, SIF_PAGE, SIF_POS,
    SIF_RANGE,
};

pub const CONTENT_WIDTH: i32 = 552;
// Forms are authored on one logical grid; the compact viewport keeps their
// horizontal proportions and uses a 4/5 vertical rhythm.
const COMPACT_WIDTH: i32 = 376;
fn compact_y(value: i32, scale: u32) -> i32 {
    scale_dpi_value(value * 4 / 5, 96, scale)
}
pub const FIELD_LEFT: i32 = 248;
pub const FIELD_WIDTH: i32 = CONTENT_WIDTH - FIELD_LEFT;
pub const CONTROL_HEIGHT: i32 = 34;
// Page names already appear in navigation. Keep a short heading, then put
// the form directly beneath it instead of reserving an introductory paragraph.
const FORM_TOP_REDUCTION: i32 = 56;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoxRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl BoxRect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn scaled(self, scale: u32, content_width: i32) -> Self {
        let px = |value| compact_y(value, scale);
        // Horizontal proportions have a bounded range. Below the minimum form
        // width the viewport scrolls instead of shrinking labels and fields.
        let x = i64::from(self.x) * i64::from(content_width) / i64::from(CONTENT_WIDTH);
        let right =
            i64::from(self.x + self.width) * i64::from(content_width) / i64::from(CONTENT_WIDTH);
        Self::new(x as i32, px(self.y), (right - x) as i32, px(self.height))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    Body,
    Caption,
    Title,
    Section,
}

impl TextRole {
    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug)]
struct Fonts([HFONT; 4]);

impl Fonts {
    fn new(scale: u32) -> WindowsResult<Self> {
        let mut fonts = Self([HFONT::default(); 4]);
        for (index, (height, weight)) in [(13, 400), (12, 400), (18, 600), (13, 600)]
            .into_iter()
            .enumerate()
        {
            // SAFETY: font creation copies these scalar parameters and the
            // static family name. The Fonts guard owns every successful handle.
            let font = unsafe {
                CreateFontW(
                    -scale_dpi_value(height, 96, scale),
                    0,
                    0,
                    0,
                    FONT_WEIGHT(weight).0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH.0.into(),
                    windows::core::w!("Yu Gothic UI"),
                )
            };
            if font.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            fonts.0[index] = font;
        }
        Ok(fonts)
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for font in self.0.iter().filter(|font| !font.is_invalid()) {
            // SAFETY: old fonts are released only after controls use the new
            // set, or after the root and all of its controls have been destroyed.
            unsafe {
                let _ = DeleteObject((*font).into());
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Plain,
    Combo,
    Scrollable,
}

#[derive(Debug)]
struct Node {
    window: HWND,
    rect: BoxRect,
    role: TextRole,
    kind: Kind,
    last_rect: Option<BoxRect>,
}

#[derive(Debug)]
pub struct Page {
    pub window: HWND,
    pub viewport: HWND,
    height: i32,
    nodes: Vec<Node>,
    scroll_x: i32,
    scroll_y: i32,
    view_width: i32,
    view_height: i32,
    content_width: i32,
}

#[derive(Debug)]
pub struct Presentation {
    pages: Vec<Page>,
    shell_fonts: Vec<(HWND, TextRole)>,
    fonts: Fonts,
    pub scale: u32,
}

impl Presentation {
    pub fn new(dpi: u32) -> WindowsResult<Self> {
        let scale = effective_scale(dpi);
        Ok(Self {
            pages: Vec::new(),
            shell_fonts: Vec::new(),
            fonts: Fonts::new(scale)?,
            scale,
        })
    }

    pub fn shell_font(&mut self, window: HWND, role: TextRole) {
        self.shell_fonts.push((window, role));
        set_font(window, self.fonts.0[role.index()]);
    }

    pub fn page(
        &mut self,
        viewport: HWND,
        title: &str,
        _description: &str,
        height: i32,
    ) -> WindowsResult<PageBuilder<'_>> {
        let height = height - FORM_TOP_REDUCTION;
        let window = topic_panel(viewport, 0, 0, CONTENT_WIDTH, height)?;
        self.pages.push(Page {
            window,
            viewport,
            height,
            nodes: Vec::new(),
            scroll_x: 0,
            scroll_y: 0,
            view_width: 0,
            view_height: 0,
            content_width: CONTENT_WIDTH,
        });
        let mut page = PageBuilder {
            page: self.pages.last_mut().expect("just inserted page"),
        };
        page.text(
            title,
            BoxRect::new(0, 0, CONTENT_WIDTH, 36),
            TextRole::Title,
        )?;
        Ok(page)
    }

    pub fn refresh_fonts(&mut self, dpi: u32) -> WindowsResult<()> {
        let scale = effective_scale(dpi);
        let replacement = if self.scale != scale {
            Some(Fonts::new(scale)?)
        } else {
            None
        };
        let fonts = replacement.as_ref().unwrap_or(&self.fonts);
        for &(window, role) in &self.shell_fonts {
            set_font(window, fonts.0[role.index()]);
        }
        for page in &mut self.pages {
            for node in &mut page.nodes {
                set_font(node.window, fonts.0[node.role.index()]);
                if let Kind::Combo = node.kind {
                    // SAFETY: documented scalar item-height messages for a live
                    // native dropdown. -1 changes the closed selection field.
                    unsafe {
                        let _ = SendMessageW(
                            node.window,
                            CB_SETITEMHEIGHT,
                            Some(WPARAM(usize::MAX)),
                            Some(LPARAM(scale_dpi_value(23, 96, scale) as isize)),
                        );
                        let _ = SendMessageW(
                            node.window,
                            CB_SETITEMHEIGHT,
                            Some(WPARAM(0)),
                            Some(LPARAM(scale_dpi_value(23, 96, scale) as isize)),
                        );
                    }
                }
                node.last_rect = None;
            }
        }
        self.scale = scale;
        // All controls stopped borrowing the old font set before it is dropped.
        if let Some(fonts) = replacement {
            self.fonts = fonts;
        }
        Ok(())
    }

    pub fn role(&self, window: HWND) -> Option<TextRole> {
        self.shell_fonts
            .iter()
            .find(|(hwnd, _)| *hwnd == window)
            .map(|(_, role)| *role)
            .or_else(|| {
                self.pages
                    .iter()
                    .flat_map(|page| &page.nodes)
                    .find(|node| node.window == window)
                    .map(|node| node.role)
            })
    }

    pub fn reset_scroll(&mut self) {
        for page in &mut self.pages {
            page.scroll_x = 0;
            page.scroll_y = 0;
        }
    }

    pub fn reflow(&mut self, viewport: HWND) {
        let scale = self.scale;
        let Some(page) = self
            .pages
            .iter_mut()
            .find(|page| page.viewport == viewport && has_visible_style(page.window))
        else {
            return;
        };
        let mut rect = RECT::default();
        // Start from the area without either scrollbar. Starting from the
        // previous page's reduced client area can make two unneeded bars keep
        // one another alive when returning to a short page.
        let (style, bar_width, bar_height) = unsafe {
            let _ = GetClientRect(viewport, &mut rect);
            use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
            use windows::Win32::UI::WindowsAndMessaging::{SM_CXVSCROLL, SM_CYHSCROLL};
            (
                GetWindowLongPtrW(viewport, GWL_STYLE) as u32,
                GetSystemMetricsForDpi(SM_CXVSCROLL, window_dpi(viewport)),
                GetSystemMetricsForDpi(SM_CYHSCROLL, window_dpi(viewport)),
            )
        };
        let full_width = rect.right
            + if style & WS_VSCROLL.0 != 0 {
                bar_width
            } else {
                0
            };
        let full_height = rect.bottom
            + if style & WS_HSCROLL.0 != 0 {
                bar_height
            } else {
                0
            };
        let minimum_width = scale_dpi_value(COMPACT_WIDTH, 96, scale);
        let height = compact_y(page.height, scale);
        let (view_width, view_height) = viewport_extent(
            full_width,
            full_height,
            minimum_width,
            height,
            bar_width,
            bar_height,
        );
        page.view_width = view_width;
        page.view_height = view_height;
        page.content_width = view_width.clamp(minimum_width, scale_dpi_value(640, 96, scale));
        page.scroll_x = publish_scroll(
            viewport,
            SB_HORZ,
            page.content_width,
            view_width,
            page.scroll_x,
        );
        page.scroll_y = publish_scroll(viewport, SB_VERT, height, view_height, page.scroll_y);
        position_page(page, scale);
    }

    pub fn scroll(&mut self, viewport: HWND, horizontal: bool, command: u16, wheel: Option<i32>) {
        let scale = self.scale;
        let Some(page) = self
            .pages
            .iter_mut()
            .find(|page| page.viewport == viewport && has_visible_style(page.window))
        else {
            return;
        };
        let bar = if horizontal { SB_HORZ } else { SB_VERT };
        let mut info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..Default::default()
        };
        // SAFETY: a synchronous read of the viewport's native scroll state.
        if unsafe { GetScrollInfo(viewport, bar, &mut info) }.is_err() {
            return;
        }
        let line = scale_dpi_value(40, 96, scale);
        let position = if let Some(delta) = wheel {
            info.nPos.saturating_add(delta)
        } else {
            match command {
                0 => info.nPos - line,
                1 => info.nPos + line,
                2 => info.nPos - info.nPage as i32,
                3 => info.nPos + info.nPage as i32,
                4 | 5 => info.nTrackPos,
                6 => info.nMin,
                7 => info.nMax,
                _ => return,
            }
        };
        let position = publish_scroll(viewport, bar, info.nMax + 1, info.nPage as i32, position);
        if horizontal {
            page.scroll_x = position;
        } else {
            page.scroll_y = position;
        }
        position_page(page, scale);
    }

    pub fn reveal_focus(&mut self, focus: HWND) {
        let scale = self.scale;
        for page in &mut self.pages {
            if !has_visible_style(page.window) || !has_visible_style(page.viewport) {
                continue;
            }
            // A ComboBox can focus its internal edit child; resolve that child
            // to the registered field without relying on displayed text.
            let Some(node) = page.nodes.iter().find(|node| {
                node.window == focus
                    || unsafe {
                        // SAFETY: read-only ancestry query for a live focus HWND.
                        windows::Win32::UI::WindowsAndMessaging::IsChild(node.window, focus)
                            .as_bool()
                    }
            }) else {
                continue;
            };
            let rect = node_rect(node, scale, page.content_width, page.view_height);
            let margin = scale_dpi_value(8, 96, scale);
            let x = reveal_offset(page.scroll_x, page.view_width, rect.x, rect.width, margin);
            let y = reveal_offset(page.scroll_y, page.view_height, rect.y, rect.height, margin);
            if x == page.scroll_x && y == page.scroll_y {
                return;
            }
            page.scroll_x = publish_scroll(
                page.viewport,
                SB_HORZ,
                page.content_width,
                page.view_width,
                x,
            );
            page.scroll_y = publish_scroll(
                page.viewport,
                SB_VERT,
                compact_y(page.height, scale),
                page.view_height,
                y,
            );
            position_page(page, scale);
            return;
        }
    }
}

fn viewport_extent(
    width: i32,
    height: i32,
    content_width: i32,
    content_height: i32,
    bar_width: i32,
    bar_height: i32,
) -> (i32, i32) {
    let (mut horizontal, mut vertical) = (false, false);
    for _ in 0..2 {
        horizontal |= content_width > width - if vertical { bar_width } else { 0 };
        vertical |= content_height > height - if horizontal { bar_height } else { 0 };
    }
    (
        (width - if vertical { bar_width } else { 0 }).max(1),
        (height - if horizontal { bar_height } else { 0 }).max(1),
    )
}

fn reveal_offset(current: i32, viewport: i32, start: i32, length: i32, margin: i32) -> i32 {
    if start < current + margin {
        (start - margin).max(0)
    } else if start + length > current + viewport - margin {
        (start + length + margin - viewport).max(0)
    } else {
        current
    }
}

fn publish_scroll(
    viewport: HWND,
    bar: windows::Win32::UI::WindowsAndMessaging::SCROLLBAR_CONSTANTS,
    extent: i32,
    visible: i32,
    position: i32,
) -> i32 {
    let position = position.clamp(0, (extent - visible).max(0));
    let info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: (extent - 1).max(0),
        nPage: visible.max(1) as u32,
        nPos: position,
        ..Default::default()
    };
    // SAFETY: the native scrollbar copies this stack value synchronously.
    unsafe {
        SetScrollInfo(viewport, bar, &info, true);
    }
    position
}

fn position_page(page: &mut Page, scale: u32) {
    move_window(
        page.window,
        BoxRect::new(
            -page.scroll_x,
            -page.scroll_y,
            page.content_width,
            compact_y(page.height, scale),
        ),
    );
    for node in &mut page.nodes {
        let mut rect = node_rect(node, scale, page.content_width, page.view_height);
        if let Kind::Combo = node.kind {
            rect.height = scale_dpi_value(240, 96, scale);
        }
        if node.last_rect != Some(rect) {
            move_window(node.window, rect);
            node.last_rect = Some(rect);
        }
    }
}

fn node_rect(node: &Node, scale: u32, width: i32, viewport_height: i32) -> BoxRect {
    let mut rect = node.rect.scaled(scale, width);
    if let Kind::Scrollable = node.kind {
        // Lists and diagnostic text have their own native scrollbars. Keep the
        // complete control reachable instead of making its bottom controls
        // permanently taller than the surrounding form viewport.
        rect.height = rect.height.min(
            (viewport_height - scale_dpi_value(16, 96, scale)).max(scale_dpi_value(40, 96, scale)),
        );
    }
    rect
}

pub fn move_window(window: HWND, rect: BoxRect) {
    // SAFETY: only owned live HWNDs enter the presentation model. Resizing and
    // scrolling preserve focus and z-order and never alter settings values.
    unsafe {
        let _ = SetWindowPos(
            window,
            None,
            rect.x,
            rect.y,
            rect.width.max(1),
            rect.height.max(1),
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

fn set_font(window: HWND, font: HFONT) {
    // SAFETY: the presentation owns the borrowed font through window teardown.
    unsafe {
        let _ = SendMessageW(
            window,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

pub fn effective_scale(dpi: u32) -> u32 {
    let mut percent = 100u32;
    let mut bytes = size_of::<u32>() as u32;
    // SAFETY: bounded read of the current Windows accessibility text scale;
    // an absent/invalid value preserves the standard size.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Accessibility"),
            windows::core::w!("TextScaleFactor"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut percent as *mut u32).cast()),
            Some(&mut bytes),
        )
    };
    if result.is_err() {
        percent = 100;
    }
    dpi.max(96).saturating_mul(percent.clamp(100, 225)) / 100
}

pub struct PageBuilder<'a> {
    page: &'a mut Page,
}

impl PageBuilder<'_> {
    pub fn window(&self) -> HWND {
        self.page.window
    }

    fn record(&mut self, window: HWND, mut rect: BoxRect, role: TextRole, kind: Kind) -> HWND {
        if rect.y >= 100 {
            rect.y -= FORM_TOP_REDUCTION;
        }
        self.page.nodes.push(Node {
            window,
            rect,
            role,
            kind,
            last_rect: None,
        });
        window
    }

    pub fn text(&mut self, text: &str, rect: BoxRect, role: TextRole) -> WindowsResult<HWND> {
        let window = label(
            self.page.window,
            text,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
        )?;
        Ok(self.record(window, rect, role, Kind::Plain))
    }

    pub fn section(&mut self, text: &str, y: i32) -> WindowsResult<HWND> {
        self.text(
            text,
            BoxRect::new(0, y, CONTENT_WIDTH, 24),
            TextRole::Section,
        )
    }

    pub fn helper(&mut self, text: &str, y: i32) -> WindowsResult<HWND> {
        self.text(
            text,
            BoxRect::new(0, y, CONTENT_WIDTH, 40),
            TextRole::Caption,
        )
    }

    pub fn row_label(&mut self, text: &str, y: i32) -> WindowsResult<HWND> {
        self.text(
            text,
            BoxRect::new(0, y + 6, FIELD_LEFT - 24, 24),
            TextRole::Body,
        )
    }

    pub fn row_combo(&mut self, text: &str, y: i32) -> WindowsResult<HWND> {
        self.row_label(text, y)?;
        self.combo(FIELD_LEFT, y, FIELD_WIDTH)
    }

    pub fn combo(&mut self, x: i32, y: i32, width: i32) -> WindowsResult<HWND> {
        let window = combo(self.page.window, x, y, width, 240)?;
        Ok(self.record(
            window,
            BoxRect::new(x, y, width, CONTROL_HEIGHT),
            TextRole::Body,
            Kind::Combo,
        ))
    }

    pub fn edit(&mut self, text: &str, rect: BoxRect, readonly: bool) -> WindowsResult<HWND> {
        let window = edit(
            self.page.window,
            text,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            readonly,
        )?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Plain))
    }

    pub fn password(&mut self, rect: BoxRect) -> WindowsResult<HWND> {
        let window = password_edit(self.page.window, rect.x, rect.y, rect.width, rect.height)?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Plain))
    }

    pub fn button(&mut self, text: &str, rect: BoxRect) -> WindowsResult<HWND> {
        let window = button(
            self.page.window,
            text,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            false,
        )?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Plain))
    }

    pub fn checkbox(&mut self, text: &str, rect: BoxRect) -> WindowsResult<HWND> {
        let window = checkbox(
            self.page.window,
            text,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
        )?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Plain))
    }

    pub fn radio(&mut self, text: &str, rect: BoxRect, first: bool) -> WindowsResult<HWND> {
        let window = radio(
            self.page.window,
            text,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            first,
        )?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Plain))
    }

    pub fn list(&mut self, rect: BoxRect) -> WindowsResult<HWND> {
        let window = listbox(self.page.window, rect.x, rect.y, rect.width, rect.height)?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Scrollable))
    }

    pub fn multiline(&mut self, rect: BoxRect) -> WindowsResult<HWND> {
        let window = multiline_readonly(self.page.window, rect.x, rect.y, rect.width, rect.height)?;
        Ok(self.record(window, rect, TextRole::Body, Kind::Scrollable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NativeFixture {
        app: Box<App>,
        owner: HWND,
    }
    impl NativeFixture {
        fn new() -> Self {
            register_window_class().expect("register settings classes");
            let window = create_main_window().expect("create settings root");
            let owner = unsafe { GetWindow(window, GW_OWNER) }.expect("owned settings window");
            let mut app = Box::new(App::new(window).expect("create settings form"));
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, (&mut *app as *mut App) as isize);
            }
            Self { app, owner }
        }
    }
    impl Drop for NativeFixture {
        fn drop(&mut self) {
            // Destroy all windows before their borrowed font and brush owners.
            unsafe {
                let _ = DestroyWindow(self.app.window);
                let _ = DestroyWindow(self.owner);
            }
        }
    }
    fn screen_rect(window: HWND) -> RECT {
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(window, &mut rect).expect("live control rectangle");
        }
        rect
    }

    #[test]
    fn every_native_form_keeps_actions_separate_and_reveals_keyboard_targets_at_supported_dpi() {
        let mut fixture = NativeFixture::new();
        let app = &mut fixture.app;
        let topics: [&[usize]; 5] = [
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            &[0, 1],
            &[0, 1],
            &[0],
            &[0, 1, 2],
        ];
        for dpi in [96, 120, 144, 192] {
            let width = scale_dpi_value(WINDOW_WIDTH, 96, dpi).min(1366);
            let height = scale_dpi_value(WINDOW_HEIGHT, 96, dpi).min(728);
            app.apply_dpi_change(
                dpi,
                Some(RECT {
                    left: 16,
                    top: 16,
                    right: 16 + width,
                    bottom: 16 + height,
                }),
            );
            let root = screen_rect(app.window);
            let mut previous_right = 0;
            for button in [app.ok, app.cancel, app.apply] {
                let rect = screen_rect(button);
                assert!(
                    rect.left > previous_right
                        && rect.right < root.right
                        && rect.bottom < root.bottom,
                    "actions fit at {dpi}"
                );
                previous_right = rect.right;
            }
            for (category, values) in topics.iter().enumerate() {
                app.show_panel(category);
                for &topic in *values {
                    app.show_topic_controls(topic);
                    let viewport = app.panels[category];
                    let view = screen_rect(viewport);
                    assert!(
                        view.bottom < screen_rect(app.ok).top,
                        "page stays above actions"
                    );
                    assert!(
                        screen_rect(app.input_tree).right < view.left,
                        "navigation and page stay separate"
                    );
                    let page = app
                        .presentation
                        .pages
                        .iter()
                        .find(|page| page.viewport == viewport && has_visible_style(page.window))
                        .expect("one visible form");
                    let fields: Vec<_> = page
                        .nodes
                        .iter()
                        .filter(|node| unsafe {
                            GetWindowLongPtrW(node.window, GWL_STYLE) as u32 & WS_TABSTOP.0 != 0
                        })
                        .map(|node| node.window)
                        .collect();
                    assert!(!fields.is_empty());
                    let first_field_top = fields
                        .iter()
                        .map(|field| screen_rect(*field).top)
                        .min()
                        .expect("at least one form field");
                    assert!(
                        first_field_top - view.top <= scale_dpi_value(72, 96, dpi),
                        "form content starts near its heading at dpi={dpi}, category={category}, topic={topic}"
                    );
                    for field in fields {
                        app.presentation.reveal_focus(field);
                        let rect = screen_rect(field);
                        let view = screen_rect(viewport);
                        assert!(
                            rect.left >= view.left
                                && rect.top >= view.top
                                && rect.right <= view.right
                                && rect.bottom <= view.bottom,
                            "focus target {:?} stays reachable at dpi={dpi}, category={category}, topic={topic}: {rect:?}, {view:?}",
                            window_text(field)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn short_page_releases_both_scrollbars_and_long_page_reserves_both_axes() {
        assert_eq!(viewport_extent(400, 300, 384, 294, 17, 17), (400, 300));
        assert_eq!(viewport_extent(400, 300, 384, 576, 17, 17), (383, 283));
        assert_eq!(viewport_extent(420, 300, 384, 576, 17, 17), (403, 300));
    }

    #[test]
    fn keyboard_focus_scrolls_into_view_without_moving_visible_controls() {
        assert_eq!(reveal_offset(0, 200, 300, 34, 8), 142);
        assert_eq!(reveal_offset(142, 200, 300, 34, 8), 142);
        assert_eq!(reveal_offset(142, 200, 52, 34, 8), 44);
        assert_eq!(reveal_offset(0, 200, 0, 34, 8), 0);
    }

    #[test]
    fn form_columns_stay_separate_at_all_supported_scales() {
        for scale in [96, 120, 144, 192, 288, 432] {
            for width in [CONTENT_WIDTH, 640, 720] {
                let physical_width = scale_dpi_value(width, 96, scale);
                let label = BoxRect::new(0, 6, FIELD_LEFT - 24, 24).scaled(scale, physical_width);
                let field = BoxRect::new(FIELD_LEFT, 0, FIELD_WIDTH, CONTROL_HEIGHT)
                    .scaled(scale, physical_width);
                assert!(label.x + label.width < field.x);
                assert_eq!(field.x + field.width, physical_width);
                assert!(field.height >= compact_y(CONTROL_HEIGHT, 96));
            }
        }
    }
}
