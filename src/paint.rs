use crate::panel::{Panel, RowKind, ToolbarButton};
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::time::SystemTime;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;

const WHITE: u32 = rgb(246, 246, 248);
const TEXT: u32 = rgb(223, 225, 232);
const MUTED: u32 = rgb(164, 168, 184);
const ACCENT: u32 = rgb(245, 173, 131);
const LINE: u32 = rgb(61, 63, 75);
const HOVER: u32 = rgb(36, 38, 49);
const SELECTED: u32 = rgb(66, 49, 44);
pub(crate) const FOLDER_FONT_SIZE: i32 = 15;
pub(crate) const INPUT_BACKGROUND: u32 = rgb(39, 39, 48);
pub(crate) const INPUT_TEXT: u32 = WHITE;

const fn rgb(red: u32, green: u32, blue: u32) -> u32 {
    red | green << 8 | blue << 16
}

pub fn paint(hwnd: HWND, panel: &Panel) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let target = BeginPaint(hwnd, &mut paint);
        if !target.is_null() && panel.shown_width > 0 && panel.height > 0 {
            render(target, panel, paint.rcPaint);
        }
        EndPaint(hwnd, &paint);
    }
}

fn render(target: HDC, panel: &Panel, dirty: RECT) {
    let left = dirty.left.max(0);
    let top = dirty.top.max(0);
    let right = dirty.right.min(panel.paint_width());
    let bottom = dirty.bottom.min(panel.height);
    if left >= right || top >= bottom {
        return;
    }
    let mut cache = panel.surface.borrow_mut();
    if cache
        .as_ref()
        .is_none_or(|surface| (surface.width, surface.height) != (panel.width, panel.height))
    {
        *cache = Surface::new(target, panel.width, panel.height);
        panel.paint_dirty.set(true);
    }
    let Some(surface) = cache.as_mut() else {
        return;
    };
    if panel.paint_dirty.replace(false) {
        surface.draw(panel);
    }
    unsafe {
        // Clip out the native edit without changing the popup's window styles.
        let search = panel.visible_search_rect();
        if left < search.right && right > search.left && top < search.bottom && bottom > search.top
        {
            let diameter = panel.search_corner_diameter();
            let shape = CreateRoundRectRgn(
                search.left,
                search.top,
                search.right,
                search.bottom,
                diameter,
                diameter,
            );
            if shape.is_null() {
                ExcludeClipRect(target, search.left, search.top, search.right, search.bottom);
            } else {
                ExtSelectClipRgn(target, shape, RGN_DIFF);
                DeleteObject(shape);
            }
        }
        BitBlt(
            target,
            left,
            top,
            right - left,
            bottom - top,
            surface.native.dc,
            left + panel.width - panel.paint_width(),
            top,
            SRCCOPY,
        );
    }
}

pub(crate) struct Surface {
    native: Dib,
    supersampled: Option<Dib>,
    chevrons: Option<Chevrons>,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(target: HDC, width: i32, height: i32) -> Option<Self> {
        let native = Dib::new(target, width, height)?;
        let supersampled = Dib::new(target, width * 2, height * 2).filter(|bitmap| unsafe {
            // Preserve the pixel layout while giving small type a larger
            // outline to rasterize before reducing it to screen resolution.
            SetMapMode(bitmap.dc, MM_ANISOTROPIC) != 0
                && SetWindowExtEx(bitmap.dc, 1, 1, null_mut()) != 0
                && SetViewportExtEx(bitmap.dc, 2, 2, null_mut()) != 0
        });
        Some(Self {
            native,
            supersampled,
            chevrons: None,
            width,
            height,
        })
    }

    fn draw(&mut self, panel: &Panel) {
        let arrow_size = panel.px(16);
        if self
            .chevrons
            .as_ref()
            .is_none_or(|arrows| arrows.size != arrow_size)
        {
            self.chevrons = Some(Chevrons::new(self.native.dc, arrow_size));
        }
        let arrows = self.chevrons.as_ref().unwrap();
        if let Some(bitmap) = &mut self.supersampled {
            bitmap.draw(panel, arrows);
            downsample_2x(
                bitmap.pixels(),
                self.native.pixels_mut(),
                self.width as usize,
            );
        } else {
            self.native.draw(panel, arrows);
        }
        apply_alpha(self.native.pixels_mut(), panel.backdrop_tint);
    }
}

struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pixels: *mut u32,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(target: HDC, width: i32, height: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(target);
            if dc.is_null() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..BITMAPINFOHEADER::default()
                },
                ..BITMAPINFO::default()
            };
            let mut pixels: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
            if bitmap.is_null() || pixels.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            if old.is_null() {
                DeleteObject(bitmap);
                DeleteDC(dc);
                return None;
            }
            SetBkMode(dc, TRANSPARENT as i32);
            Some(Self {
                dc,
                bitmap,
                old,
                pixels: pixels.cast(),
                width,
                height,
            })
        }
    }

    fn draw(&mut self, panel: &Panel, arrows: &Chevrons) {
        self.pixels_mut().fill(0);
        Canvas {
            dc: self.dc,
            panel,
            arrows,
        }
        .draw();
        unsafe { GdiFlush() };
    }

    fn pixels(&self) -> &[u32] {
        unsafe {
            std::slice::from_raw_parts(self.pixels, self.width as usize * self.height as usize)
        }
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe {
            std::slice::from_raw_parts_mut(self.pixels, self.width as usize * self.height as usize)
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

struct Chevron(Dib);

struct Chevrons {
    size: i32,
    collapsed: Option<Chevron>,
    expanded: Option<Chevron>,
}

impl Chevrons {
    fn new(dc: HDC, size: i32) -> Self {
        Self {
            size,
            collapsed: Chevron::new(dc, size, false, MUTED),
            expanded: Chevron::new(dc, size, true, ACCENT),
        }
    }
}

impl Chevron {
    fn new(dc: HDC, size: i32, expanded: bool, color: u32) -> Option<Self> {
        static SVG: OnceLock<Option<resvg::usvg::Tree>> = OnceLock::new();
        let tree = SVG
            .get_or_init(|| {
                resvg::usvg::Tree::from_data(
                    include_bytes!("../assets/folder-chevron.svg"),
                    &resvg::usvg::Options::default(),
                )
                .ok()
            })
            .as_ref()?;
        let raster_size = size * 2;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(raster_size as u32, raster_size as u32)?;
        let scale = raster_size as f32 / tree.size().width();
        let transform = if expanded {
            resvg::tiny_skia::Transform::from_row(0.0, scale, -scale, 0.0, raster_size as f32, 0.0)
        } else {
            resvg::tiny_skia::Transform::from_scale(scale, scale)
        };
        resvg::render(tree, transform, &mut pixmap.as_mut());
        let mut bitmap = Dib::new(dc, raster_size, raster_size)?;
        for (out, rgba) in bitmap
            .pixels_mut()
            .iter_mut()
            .zip(pixmap.data().as_chunks::<4>().0)
        {
            let alpha = u32::from(rgba[3]);
            let red = (color & 255) * alpha / 255;
            let green = ((color >> 8) & 255) * alpha / 255;
            let blue = ((color >> 16) & 255) * alpha / 255;
            *out = alpha << 24 | red << 16 | green << 8 | blue;
        }
        Some(Self(bitmap))
    }

    fn draw(&self, dc: HDC, x: i32, y: i32, size: i32) {
        unsafe {
            AlphaBlend(
                dc,
                x,
                y,
                size,
                size,
                self.0.dc,
                0,
                0,
                self.0.width,
                self.0.height,
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

fn downsample_2x(source: &[u32], target: &mut [u32], width: usize) {
    for (pair, row) in source
        .chunks_exact(width * 4)
        .zip(target.chunks_exact_mut(width))
    {
        let (top, bottom) = pair.split_at(width * 2);
        let (top, _) = top.as_chunks::<2>();
        let (bottom, _) = bottom.as_chunks::<2>();
        for (destination, (top, bottom)) in row.iter_mut().zip(top.iter().zip(bottom)) {
            let samples = [top[0], top[1], bottom[0], bottom[1]];
            let channel = |shift: u32| {
                (samples
                    .iter()
                    .map(|pixel| (pixel >> shift) & 255)
                    .sum::<u32>()
                    + 2)
                    / 4
            };
            *destination = channel(16) << 16 | channel(8) << 8 | channel(0);
        }
    }
}

pub(crate) fn create_font(size: i32, weight: i32) -> HFONT {
    create_font_with_quality(size, weight, ANTIALIASED_QUALITY)
}

pub(crate) fn create_control_font(size: i32) -> HFONT {
    create_font_with_quality(size, 400, CLEARTYPE_QUALITY)
}

fn create_font_with_quality(size: i32, weight: i32, quality: u8) -> HFONT {
    let face = font_face();
    unsafe {
        CreateFontW(
            -size,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            quality as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        )
    }
}

pub(crate) fn font_face() -> &'static [u16] {
    static FACE: OnceLock<Vec<u16>> = OnceLock::new();
    FACE.get_or_init(preferred_face)
}

fn preferred_face() -> Vec<u16> {
    unsafe extern "system" fn found_font(
        _: *const LOGFONTW,
        _: *const TEXTMETRICW,
        _: u32,
        found: LPARAM,
    ) -> i32 {
        unsafe { *(found as *mut bool) = true };
        0
    }

    let fallback = "Microsoft YaHei";
    let dc = unsafe { GetDC(null_mut()) };
    let mut face = fallback;
    if !dc.is_null() {
        for candidate in [
            "Source Han Sans SC",
            "Source Han Sans CN",
            "思源黑体 SC",
            "思源黑体 CN",
            "思源黑体",
            "Source Han Sans",
        ] {
            let mut request = LOGFONTW {
                lfCharSet: DEFAULT_CHARSET,
                ..LOGFONTW::default()
            };
            for (character, slot) in candidate.encode_utf16().zip(&mut request.lfFaceName) {
                *slot = character;
            }
            let mut found = false;
            unsafe {
                EnumFontFamiliesExW(
                    dc,
                    &request,
                    Some(found_font),
                    (&raw mut found) as LPARAM,
                    0,
                )
            };
            if found {
                face = candidate;
                break;
            }
        }
        unsafe { ReleaseDC(null_mut(), dc) };
    }
    face.encode_utf16().chain([0]).collect()
}

pub(crate) fn folder_name_width(hwnd: HWND, name: &str, size: i32) -> i32 {
    let name: Vec<_> = name.encode_utf16().collect();
    let font = create_font(size * 2, 600);
    let mut measured = SIZE::default();
    unsafe {
        let dc = GetDC(hwnd);
        if !dc.is_null() {
            let previous = GetCurrentObject(dc, OBJ_FONT as u32);
            if !font.is_null() {
                SelectObject(dc, font);
            }
            GetTextExtentPoint32W(dc, name.as_ptr(), name.len() as i32, &mut measured);
            SelectObject(dc, previous);
            ReleaseDC(hwnd, dc);
        }
        if !font.is_null() {
            DeleteObject(font);
        }
    }
    (measured.cx + 1) / 2
}

fn apply_alpha(pixels: &mut [u32], tint_alpha: u32) {
    // GDI leaves the alpha byte unset. Treat painted RGB as premultiplied
    // coverage and composite it over a dark tint before presenting to DWM.
    for pixel in pixels {
        let blue = *pixel & 255;
        let green = (*pixel >> 8) & 255;
        let red = (*pixel >> 16) & 255;
        let coverage = red.max(green).max(blue);
        let tint = tint_alpha * (255 - coverage) / 255;
        *pixel = (coverage + tint) << 24
            | (red + 21 * tint / 255) << 16
            | (green + 22 * tint / 255) << 8
            | (blue + 29 * tint / 255);
    }
}

#[cfg(test)]
#[test]
fn small_text_retains_partial_alpha_on_the_glass_surface() {
    let mut surface = Surface::new(null_mut(), 160, 32).unwrap();
    let bitmap = surface.supersampled.as_mut().unwrap();
    bitmap.pixels_mut().fill(0);
    let text: Vec<_> = "思源黑体 Claude Code".encode_utf16().collect();
    unsafe {
        let font = create_font(13, 400);
        let previous = SelectObject(bitmap.dc, font);
        SetTextColor(bitmap.dc, rgb(255, 255, 255));
        assert_ne!(
            TextOutW(bitmap.dc, 0, 0, text.as_ptr(), text.len() as i32),
            0
        );
        GdiFlush();
        SelectObject(bitmap.dc, previous);
        DeleteObject(font);
    }
    downsample_2x(bitmap.pixels(), surface.native.pixels_mut(), 160);
    apply_alpha(surface.native.pixels_mut(), 0);
    let partial = surface
        .native
        .pixels()
        .iter()
        .filter(|pixel| (1..255).contains(&(*pixel >> 24)))
        .count();
    assert!(partial > 50, "found only {partial} antialiased pixels");
}

#[cfg(test)]
#[test]
fn svg_chevrons_point_right_and_down_and_blend_into_the_glass_canvas() {
    let mut surface = Surface::new(null_mut(), 20, 20).unwrap();
    let bitmap = surface.supersampled.as_mut().unwrap();
    let arrows = Chevrons::new(bitmap.dc, 16);
    let collapsed = arrows.collapsed.unwrap();
    let expanded = arrows.expanded.unwrap();
    let span = |arrow: &Chevron| {
        let (mut left, mut top, mut right, mut bottom) = (32, 32, 0, 0);
        for (index, pixel) in arrow.0.pixels().iter().enumerate() {
            if pixel >> 24 != 0 {
                let (x, y) = (index % 32, index / 32);
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
        (right - left, bottom - top)
    };
    let (width, height) = span(&collapsed);
    assert!(width < height);
    assert_eq!(span(&expanded), (height, width));
    bitmap.pixels_mut().fill(0);
    collapsed.draw(bitmap.dc, 1, 1, 16);
    unsafe { GdiFlush() };
    assert!(bitmap.pixels().iter().any(|pixel| pixel & 0x00ff_ffff != 0));
}

struct Canvas<'a> {
    dc: HDC,
    panel: &'a Panel,
    arrows: &'a Chevrons,
}

impl Canvas<'_> {
    fn draw(&self) {
        let panel = self.panel;
        let px = |value| panel.px(value);
        let label = create_font(px(FOLDER_FONT_SIZE), 600);
        let regular = create_font(px(15), 400);
        let small = create_font(px(13), 400);
        let old = unsafe { GetCurrentObject(self.dc, OBJ_FONT as u32) };
        let input = panel.search_rect();
        self.fill_rounded(
            rect(
                input.left - 1,
                input.top - 1,
                input.right + 1,
                input.bottom + 1,
            ),
            panel.search_corner_diameter() + 2,
            LINE,
        );
        self.draw_toolbar();

        if panel.rows.is_empty() {
            self.select(regular);
            let message = panel.snapshot.error.as_deref().unwrap_or(
                match (panel.loaded, panel.is_searching()) {
                    (false, _) => tr!("Loading local sessions…", "正在读取本地会话…"),
                    (true, true) => {
                        tr!("No matching folders or sessions", "没有匹配的文件夹或会话")
                    }
                    (true, false) => tr!(
                        "No local Claude Code sessions found",
                        "没有找到本地 Claude Code 会话"
                    ),
                },
            );
            self.text(
                message,
                rect(
                    px(22),
                    panel.list_top() + px(5),
                    panel.width - px(22),
                    panel.list_top() + px(37),
                ),
                TEXT,
                DT_LEFT,
            );
        } else if panel.list_bottom() > panel.list_top() {
            unsafe {
                let saved = SaveDC(self.dc);
                IntersectClipRect(
                    self.dc,
                    0,
                    panel.list_top(),
                    panel.width,
                    panel.list_bottom(),
                );
                self.draw_rows(label, regular, small);
                RestoreDC(self.dc, saved);
            }
            self.draw_scrollbar();
        }

        if let Some(message) = panel
            .snapshot
            .error
            .as_deref()
            .filter(|_| !panel.rows.is_empty())
        {
            let footer = panel.list_bottom() + px(3);
            self.fill(rect(px(22), footer, panel.width - px(22), footer + 1), LINE);
            self.select(small);
            self.text(
                message,
                rect(
                    px(23),
                    footer + px(4),
                    panel.width - px(23),
                    panel.height - px(5),
                ),
                MUTED,
                DT_LEFT,
            );
        }

        unsafe {
            SelectObject(self.dc, old);
            for font in [label, regular, small] {
                DeleteObject(font);
            }
        }
    }

    fn draw_toolbar(&self) {
        let panel = self.panel;
        let px = |value| panel.px(value);
        for button in ToolbarButton::ALL {
            let bounds = panel.toolbar_rect(button);
            let active = match button {
                ToolbarButton::Compact => panel.preferences.compact,
                ToolbarButton::Startup => panel.preferences.startup,
                ToolbarButton::Agent => panel.agent_mode,
            };
            let hovered = panel.toolbar_hover == Some(button);
            if active || hovered {
                self.fill_rounded(bounds, px(11), if active { SELECTED } else { HOVER });
            }
            let color = if active {
                ACCENT
            } else if hovered {
                WHITE
            } else {
                MUTED
            };
            let x = (bounds.left + bounds.right) / 2;
            let y = (bounds.top + bounds.bottom) / 2;
            match button {
                ToolbarButton::Compact => {
                    let step = px(if active { 3 } else { 5 });
                    let thickness = px(2);
                    for offset in [-step, 0, step] {
                        let top = y + offset - thickness / 2;
                        self.fill_rounded(
                            rect(x - px(7), top, x + px(7), top + thickness),
                            thickness,
                            color,
                        );
                    }
                }
                ToolbarButton::Startup => {
                    let radius = px(7);
                    self.fill_rounded(rect(x - px(1), y - px(8), x + px(1), y), px(2), color);
                    unsafe {
                        let pen = CreatePen(PS_SOLID, px(2), color);
                        if !pen.is_null() {
                            let previous = SelectObject(self.dc, pen);
                            Arc(
                                self.dc,
                                x - radius,
                                y - px(5),
                                x + radius,
                                y + px(8),
                                x - px(4),
                                y - px(7),
                                x + px(4),
                                y - px(7),
                            );
                            SelectObject(self.dc, previous);
                            DeleteObject(pen);
                        }
                    }
                }
                ToolbarButton::Agent => {
                    self.fill_rounded(
                        rect(x - px(8), y - px(5), x + px(8), y + px(7)),
                        px(5),
                        color,
                    );
                    self.fill_rounded(
                        rect(x - px(6), y - px(3), x + px(6), y + px(5)),
                        px(3),
                        if active { SELECTED } else { INPUT_BACKGROUND },
                    );
                    for eye in [x - px(4), x + px(2)] {
                        self.fill_rounded(rect(eye, y, eye + px(2), y + px(2)), px(2), color);
                    }
                    self.fill(rect(x - px(1), y - px(8), x + px(1), y - px(5)), color);
                }
            }
        }
    }

    fn draw_rows(&self, label: HFONT, regular: HFONT, small: HFONT) {
        let panel = self.panel;
        let px = |value| panel.px(value);
        let first = panel
            .rows
            .partition_point(|row| row.y + row.height <= panel.scroll);
        let limit = panel.width - px(24);
        let arrow_size = self.arrows.size;
        let (link_y, title_top, title_bottom, activity_top, activity_bottom) =
            if panel.preferences.compact {
                (11, 0, 22, 18, 36)
            } else {
                (13, 2, 25, 23, 42)
            };
        for (index, row) in panel.rows.iter().enumerate().skip(first) {
            let y = row.y + panel.list_top() - panel.scroll;
            if y >= panel.list_bottom() {
                break;
            }
            let hover = panel.hover == Some(index);
            match row.kind {
                RowKind::Project(index, count) => {
                    let project = &panel.snapshot.projects[index];
                    if hover {
                        self.fill_rounded(
                            rect(px(12), y + px(3), limit + px(5), y + row.height - px(3)),
                            px(14),
                            HOVER,
                        );
                    }
                    let collapsed = panel.active_collapsed().contains(&project.key);
                    let arrow = if collapsed {
                        &self.arrows.collapsed
                    } else {
                        &self.arrows.expanded
                    };
                    if let Some(arrow) = arrow {
                        arrow.draw(
                            self.dc,
                            px(29) - arrow_size / 2,
                            y + (row.height - arrow_size) / 2,
                            arrow_size,
                        );
                    }
                    self.select(label);
                    self.text(
                        &project.name,
                        rect(px(44), y, limit - px(40), y + row.height),
                        WHITE,
                        DT_LEFT,
                    );
                    self.select(small);
                    self.text(
                        &count.to_string(),
                        rect(limit - px(38), y, limit, y + row.height),
                        MUTED,
                        DT_RIGHT,
                    );
                }
                RowKind::Session(project, session_index) => {
                    let project = &panel.snapshot.projects[project];
                    let session = &project.sessions[session_index];
                    let selected = panel
                        .selected
                        .as_ref()
                        .is_some_and(|(key, id)| key == &project.key && id == &session.id);
                    if selected || hover {
                        self.fill_rounded(
                            rect(px(40), y + px(2), limit + px(5), y + row.height - px(2)),
                            px(14),
                            if selected { SELECTED } else { HOVER },
                        );
                    }
                    self.fill(rect(px(30), y, px(30) + 1, y + row.height), LINE);
                    self.fill(
                        rect(px(30), y + px(link_y), px(45), y + px(link_y) + 1),
                        if selected { ACCENT } else { LINE },
                    );
                    self.select(regular);
                    self.text(
                        &session.title,
                        rect(px(50), y + px(title_top), limit, y + px(title_bottom)),
                        if selected { WHITE } else { TEXT },
                        DT_LEFT,
                    );
                    self.select(small);
                    self.text(
                        &activity(session.modified),
                        rect(px(50), y + px(activity_top), limit, y + px(activity_bottom)),
                        MUTED,
                        DT_LEFT,
                    );
                }
            }
        }
    }

    fn draw_scrollbar(&self) {
        let panel = self.panel;
        if panel.max_scroll() == 0 {
            return;
        }
        let top = panel.list_top();
        let height = panel.list_bottom() - top;
        let thumb = ((height as i64 * height as i64 / panel.content_height as i64) as i32)
            .max(panel.px(25))
            .min(height);
        let y = top
            + (panel.scroll as i64 * (height - thumb) as i64 / panel.max_scroll() as i64) as i32;
        let x = panel.width - panel.px(9);
        self.fill(rect(x, top, x + panel.px(3), top + height), LINE);
        self.fill(rect(x, y, x + panel.px(3), y + thumb), MUTED);
    }

    fn select(&self, font: HFONT) {
        if !font.is_null() {
            unsafe { SelectObject(self.dc, font) };
        }
    }

    fn fill(&self, rect: RECT, color: u32) {
        unsafe {
            let brush = CreateSolidBrush(color);
            if !brush.is_null() {
                FillRect(self.dc, &rect, brush);
                DeleteObject(brush);
            }
        }
    }

    fn fill_rounded(&self, rect: RECT, diameter: i32, color: u32) {
        unsafe {
            let brush = CreateSolidBrush(color);
            if !brush.is_null() {
                let previous_brush = SelectObject(self.dc, brush);
                let previous_pen = SelectObject(self.dc, GetStockObject(NULL_PEN));
                RoundRect(
                    self.dc,
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom,
                    diameter,
                    diameter,
                );
                SelectObject(self.dc, previous_pen);
                SelectObject(self.dc, previous_brush);
                DeleteObject(brush);
            }
        }
    }

    fn text(&self, text: &str, mut rect: RECT, color: u32, align: u32) {
        let text: Vec<_> = text.encode_utf16().collect();
        if text.is_empty() {
            return;
        }
        unsafe {
            SetTextColor(self.dc, color);
            DrawTextW(
                self.dc,
                text.as_ptr(),
                text.len() as i32,
                &mut rect,
                align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }
    }
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn activity(modified: SystemTime) -> String {
    let minutes = modified.elapsed().unwrap_or_default().as_secs() / 60;
    match minutes {
        0 => tr!("Just now", "刚刚").into(),
        1..60 => relative_time(minutes, "minute", "minutes", "分钟"),
        60..1440 => relative_time(minutes / 60, "hour", "hours", "小时"),
        _ => relative_time(minutes / 1440, "day", "days", "天"),
    }
}

fn relative_time(count: u64, singular: &str, plural: &str, unit: &str) -> String {
    let english = if count == 1 { singular } else { plural };
    trf!("{count} {english} ago", "{count} {unit}前")
}
