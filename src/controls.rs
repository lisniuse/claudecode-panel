use crate::i18n;
use crate::paint;
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows_sys::Win32::UI::{Controls::*, Shell::*, WindowsAndMessaging::*};
use windows_sys::w;

pub(crate) const SEARCH_ID: usize = 101;
const MAX_QUERY: usize = 256;

pub(crate) struct Controls {
    pub edit: HWND,
    tip: HWND,
    tool: TTTOOLINFOW,
    tip_text: Vec<u16>,
    font: HFONT,
    font_size: i32,
    edit_shape: (i32, i32, i32),
    tip_visible: bool,
}

impl Controls {
    pub fn new(owner: HWND, instance: HINSTANCE) -> io::Result<Self> {
        unsafe {
            let edit = CreateWindowExW(
                0,
                w!("EDIT"),
                null(),
                WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL as u32,
                0,
                0,
                1,
                1,
                owner,
                SEARCH_ID as HMENU,
                instance,
                null(),
            );
            if edit.is_null() {
                return Err(io::Error::last_os_error());
            }
            SendMessageW(edit, EM_SETLIMITTEXT, MAX_QUERY, 0);
            set_search_cue(edit);
            SetWindowSubclass(edit, Some(search_proc), 1, 0);

            let init = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            };
            InitCommonControlsEx(&init);
            let tip = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX | TTS_NOANIMATE | TTS_NOFADE,
                0,
                0,
                0,
                0,
                owner,
                null_mut(),
                instance,
                null(),
            );
            if tip.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut tip_text = vec![0u16];
            let tool = TTTOOLINFOW {
                cbSize: size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_TRACK | TTF_ABSOLUTE | TTF_TRANSPARENT,
                hwnd: owner,
                uId: 1,
                lpszText: tip_text.as_mut_ptr(),
                ..TTTOOLINFOW::default()
            };
            if SendMessageW(tip, TTM_ADDTOOLW, 0, (&raw const tool) as LPARAM) == 0 {
                return Err(io::Error::other(tr!(
                    "Could not create the folder path tooltip",
                    "无法创建文件夹路径提示"
                )));
            }
            SendMessageW(tip, TTM_SETTIPBKCOLOR, paint::INPUT_BACKGROUND as usize, 0);
            SendMessageW(tip, TTM_SETTIPTEXTCOLOR, paint::INPUT_TEXT as usize, 0);
            Ok(Self {
                edit,
                tip,
                tool,
                tip_text,
                font: null_mut(),
                font_size: 0,
                edit_shape: (0, 0, 0),
                tip_visible: false,
            })
        }
    }

    pub fn update_language(&self) {
        set_search_cue(self.edit);
    }

    pub fn layout(
        &mut self,
        rect: RECT,
        font_size: i32,
        padding: i32,
        tip_width: i32,
        radius: i32,
    ) {
        unsafe {
            let mut flags = SWP_NOACTIVATE | SWP_NOZORDER;
            if font_size != self.font_size {
                let font = paint::create_control_font(font_size);
                if !font.is_null() {
                    let line_height = font_line_height(self.edit, font);
                    SetWindowSubclass(self.edit, Some(search_proc), 1, line_height as usize);
                    SendMessageW(self.edit, WM_SETFONT, font as usize, 0);
                    SendMessageW(self.tip, WM_SETFONT, font as usize, 1);
                    if !self.font.is_null() {
                        DeleteObject(self.font);
                    }
                    self.font = font;
                    self.font_size = font_size;
                    flags |= SWP_FRAMECHANGED;
                }
                SendMessageW(self.tip, TTM_SETMAXTIPWIDTH, 0, tip_width as LPARAM);
                let margins = RECT {
                    left: padding,
                    top: padding / 2,
                    right: padding,
                    bottom: padding / 2,
                };
                SendMessageW(self.tip, TTM_SETMARGIN, 0, (&raw const margins) as LPARAM);
                let edit_margins = (padding as u32) << 16 | padding as u32;
                SendMessageW(
                    self.edit,
                    EM_SETMARGINS,
                    (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
                    edit_margins as LPARAM,
                );
            }
            let (width, height) = (
                (rect.right - rect.left).max(1),
                (rect.bottom - rect.top).max(1),
            );
            if self.edit_shape != (width, height, radius) {
                let region = CreateRoundRectRgn(0, 0, width, height, radius, radius);
                if !region.is_null() {
                    if SetWindowRgn(self.edit, region, 1) != 0 {
                        self.edit_shape = (width, height, radius);
                    } else {
                        DeleteObject(region);
                    }
                }
            }
            SetWindowPos(
                self.edit,
                null_mut(),
                rect.left,
                rect.top,
                width,
                height,
                flags,
            );
        }
    }

    pub fn query(&self) -> String {
        let mut buffer = [0u16; MAX_QUERY + 1];
        let len = unsafe { GetWindowTextW(self.edit, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..len as usize])
    }

    pub fn show_tip(&mut self, path: &str, point: POINT) {
        if self.tip_visible {
            return;
        }
        let mut text: Vec<_> = path.encode_utf16().chain([0]).collect();
        self.tool.lpszText = text.as_mut_ptr();
        let position = (point.x as u16 as u32) | ((point.y as u16 as u32) << 16);
        unsafe {
            SendMessageW(
                self.tip,
                TTM_UPDATETIPTEXTW,
                0,
                (&raw const self.tool) as LPARAM,
            );
            SendMessageW(self.tip, TTM_TRACKPOSITION, 0, position as LPARAM);
            SendMessageW(
                self.tip,
                TTM_TRACKACTIVATE,
                1,
                (&raw const self.tool) as LPARAM,
            );
        }
        self.tip_text = text;
        self.tip_visible = true;
    }

    pub fn hide_tip(&mut self) {
        if self.tip_visible {
            unsafe {
                SendMessageW(
                    self.tip,
                    TTM_TRACKACTIVATE,
                    0,
                    (&raw const self.tool) as LPARAM,
                )
            };
            self.tip_visible = false;
        }
    }
}

fn set_search_cue(edit: HWND) {
    let cue = i18n::wide(tr!("Search folders or sessions…", "搜索文件夹或会话…"));
    unsafe {
        SendMessageW(edit, EM_SETCUEBANNER, 1, cue.as_ptr() as LPARAM);
        InvalidateRect(edit, null(), 0);
    }
}

impl Drop for Controls {
    fn drop(&mut self) {
        if !self.font.is_null() {
            unsafe { DeleteObject(self.font) };
        }
    }
}

unsafe extern "system" fn search_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    line_height: usize,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCALCSIZE if line_height > 0 => {
                // The first rectangle is the new client area in both forms
                // of WM_NCCALCSIZE. The edit lays out both texts inside it.
                let rect = &mut *(lparam as *mut RECT);
                let spare = (rect.bottom - rect.top - line_height as i32).max(0);
                rect.top += spare / 2;
                rect.bottom -= spare - spare / 2;
                return 0;
            }
            WM_NCPAINT => {
                paint_search_padding(hwnd);
                return 0;
            }
            WM_NCHITTEST => return HTCLIENT as LRESULT,
            WM_LBUTTONDOWN => {
                // An explicit click gives the edit focus and lets Windows
                // handle the IME and native editing shortcuts.
                let owner = GetParent(hwnd);
                if GetForegroundWindow() != owner {
                    SetForegroundWindow(owner);
                }
                if GetFocus() != hwnd {
                    SetFocus(hwnd);
                }
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

fn font_line_height(hwnd: HWND, font: HFONT) -> i32 {
    let mut metrics = TEXTMETRICW::default();
    unsafe {
        let dc = GetDC(hwnd);
        if !dc.is_null() {
            let previous = SelectObject(dc, font);
            GetTextMetricsW(dc, &mut metrics);
            SelectObject(dc, previous);
            ReleaseDC(hwnd, dc);
        }
    }
    metrics.tmHeight
}

fn paint_search_padding(hwnd: HWND) {
    unsafe {
        let mut window = RECT::default();
        let mut client = RECT::default();
        let mut origin = POINT::default();
        if GetWindowRect(hwnd, &mut window) == 0
            || GetClientRect(hwnd, &mut client) == 0
            || ClientToScreen(hwnd, &mut origin) == 0
        {
            return;
        }
        let dc = GetWindowDC(hwnd);
        if dc.is_null() {
            return;
        }
        let x = origin.x - window.left;
        let y = origin.y - window.top;
        ExcludeClipRect(dc, x, y, x + client.right, y + client.bottom);
        let bounds = RECT {
            left: 0,
            top: 0,
            right: window.right - window.left,
            bottom: window.bottom - window.top,
        };
        SetDCBrushColor(dc, paint::INPUT_BACKGROUND);
        FillRect(dc, &bounds, GetStockObject(DC_BRUSH));
        ReleaseDC(hwnd, dc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_search_edit_centers_the_font_line() {
        unsafe {
            let host = CreateWindowExW(
                0,
                w!("STATIC"),
                null(),
                WS_POPUP,
                0,
                0,
                220,
                80,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            assert!(!host.is_null());
            let edit = CreateWindowExW(
                0,
                w!("EDIT"),
                null(),
                WS_CHILD | ES_AUTOHSCROLL as u32,
                0,
                0,
                200,
                32,
                host,
                null_mut(),
                null_mut(),
                null(),
            );
            assert!(!edit.is_null());
            let font = paint::create_control_font(15);
            assert!(!font.is_null());
            let line = font_line_height(edit, font);
            assert!(line > 0);
            SendMessageW(edit, WM_SETFONT, font as usize, 0);
            assert_ne!(
                SetWindowSubclass(edit, Some(search_proc), 1, line as usize),
                0
            );
            for extra in [6, 7] {
                let height = line + extra;
                assert_ne!(
                    SetWindowPos(
                        edit,
                        null_mut(),
                        0,
                        0,
                        200,
                        height,
                        SWP_FRAMECHANGED | SWP_NOZORDER
                    ),
                    0
                );
                let mut window = RECT::default();
                let mut client = RECT::default();
                let mut origin = POINT::default();
                GetWindowRect(edit, &mut window);
                GetClientRect(edit, &mut client);
                ClientToScreen(edit, &mut origin);
                assert_eq!(origin.y - window.top, extra / 2);
                assert_eq!(client.bottom, line);
            }
            DestroyWindow(host);
            DeleteObject(font);
        }
    }
}
