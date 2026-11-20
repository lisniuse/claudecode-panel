use crate::controls::{Controls, SEARCH_ID};
use crate::dialogs;
use crate::i18n::{self, Preference};
use crate::paint;
use crate::sessions::{self, Project, Session, Snapshot};
use crate::settings::{self, Preferences};
use crate::terminal::{self, AgentTerminals};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::{Dwm::*, Gdi::*};
use windows_sys::Win32::System::{
    LibraryLoader::{GetModuleHandleW, GetProcAddress},
    Threading::*,
};
use windows_sys::Win32::UI::{Controls::MARGINS, HiDpi::*, Shell::*, WindowsAndMessaging::*};
use windows_sys::{s, w};

const CLASS: *const u16 = w!("ClaudeCodePanelWindow");
const TRAY_EVENT: u32 = WM_APP + 1;
const OPEN_PANEL: u32 = WM_APP + 2;
const TRAY_KEY_SELECT: u32 = NIN_SELECT + 1;
const EDGE_DELAY: Duration = Duration::from_millis(160);
const HIDE_DELAY: Duration = Duration::from_millis(320);
const TRAY_ENTRY_GRACE: Duration = Duration::from_secs(3);
const IDLE_TIMER_MS: u32 = 32;
const FALLBACK_TIMER_MS: u32 = 15;
const FRAME_INTERVAL_MS: i32 = 5;
const WINDOW_INSET: i32 = 14;
const EDGE_WIDTH: i32 = 12;
const TOOLTIP_DELAY: Duration = Duration::from_millis(380);

thread_local! {
    static STATE: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy)]
pub(crate) enum RowKind {
    Project(usize, usize),
    Session(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolbarButton {
    Compact,
    Startup,
    Agent,
}

impl ToolbarButton {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Startup, Self::Agent];
}

pub(crate) struct Row {
    pub kind: RowKind,
    pub y: i32,
    pub height: i32,
}

pub(crate) struct Panel {
    hwnd: HWND,
    controls: Controls,
    updates: Receiver<Snapshot>,
    refresh: SyncSender<()>,
    taskbar_created: u32,
    pub snapshot: Snapshot,
    collapsed: HashSet<String>,
    search_collapsed: HashSet<String>,
    query: String,
    pub rows: Vec<Row>,
    pub selected: Option<(String, String)>,
    pub hover: Option<usize>,
    pub preferences: Preferences,
    pub agent_mode: bool,
    agent_terminals: AgentTerminals,
    pub toolbar_hover: Option<ToolbarButton>,
    hover_name_right: i32,
    path_hover_since: Option<Instant>,
    path_hover_blocked: bool,
    pub loaded: bool,
    pub backdrop_tint: u32,
    pub surface: RefCell<Option<paint::Surface>>,
    pub paint_dirty: Cell<bool>,
    pub scroll: i32,
    pub content_height: i32,
    pub width: i32,
    pub height: i32,
    pub shown_width: i32,
    pub slide_offscreen: bool,
    pub dpi: i32,
    frame: RECT,
    work_area: RECT,
    screen_left: i32,
    progress: f32,
    opening: bool,
    edge_since: Option<(HMONITOR, Instant)>,
    outside_since: Option<Instant>,
    enter_by: Option<Instant>,
    edge_bridge: bool,
    edge_blocked: bool,
    last_tick: Instant,
    frame_timer: HANDLE,
    animating: bool,
}

impl Panel {
    fn new(hwnd: HWND, frame_timer: HANDLE, controls: Controls, preferences: Preferences) -> Self {
        let (updates, refresh) = sessions::watch();
        Self {
            hwnd,
            controls,
            updates,
            refresh,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            snapshot: Snapshot::default(),
            collapsed: HashSet::new(),
            search_collapsed: HashSet::new(),
            query: String::new(),
            rows: Vec::new(),
            selected: None,
            hover: None,
            preferences,
            agent_mode: false,
            agent_terminals: AgentTerminals::default(),
            toolbar_hover: None,
            hover_name_right: 0,
            path_hover_since: None,
            path_hover_blocked: false,
            loaded: false,
            backdrop_tint: 255,
            surface: RefCell::new(None),
            paint_dirty: Cell::new(true),
            scroll: 0,
            content_height: 0,
            width: 0,
            height: 0,
            shown_width: 0,
            slide_offscreen: false,
            dpi: 96,
            frame: RECT::default(),
            work_area: RECT::default(),
            screen_left: 0,
            progress: 0.0,
            opening: false,
            edge_since: None,
            outside_since: None,
            enter_by: None,
            edge_bridge: false,
            edge_blocked: false,
            last_tick: Instant::now(),
            frame_timer,
            animating: false,
        }
    }

    pub fn px(&self, value: i32) -> i32 {
        (value * self.dpi + 48) / 96
    }

    pub fn list_top(&self) -> i32 {
        self.search_rect().bottom + self.px(16)
    }

    pub fn search_rect(&self) -> RECT {
        RECT {
            left: self.px(18),
            right: self.toolbar_rect(ToolbarButton::Compact).left - self.px(8),
            top: self.px(19),
            bottom: self.px(51),
        }
    }

    pub fn toolbar_rect(&self, button: ToolbarButton) -> RECT {
        let offset = match button {
            ToolbarButton::Compact => 70,
            ToolbarButton::Startup => 35,
            ToolbarButton::Agent => 0,
        };
        RECT {
            left: self.width - self.px(48 + offset),
            right: self.width - self.px(18 + offset),
            top: self.px(19),
            bottom: self.px(51),
        }
    }

    fn toolbar_at(&self, point: POINT) -> Option<ToolbarButton> {
        ToolbarButton::ALL
            .into_iter()
            .find(|button| contains(self.toolbar_rect(*button), point))
    }

    pub fn visible_search_rect(&self) -> RECT {
        let offset = self.width - self.paint_width();
        let mut rect = self.search_rect();
        rect.left -= offset;
        rect.right -= offset;
        rect
    }

    pub fn list_bottom(&self) -> i32 {
        let bottom = if self.snapshot.error.is_some() && !self.rows.is_empty() {
            43
        } else {
            12
        };
        self.height - self.px(bottom)
    }

    pub fn search_corner_diameter(&self) -> i32 {
        self.px(14)
    }

    pub fn max_scroll(&self) -> i32 {
        (self.content_height - (self.list_bottom() - self.list_top()).max(0)).max(0)
    }

    fn redraw(&self) {
        self.paint_dirty.set(true);
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe { InvalidateRect(self.hwnd, null(), 0) };
    }

    fn redraw_list(&self) {
        self.paint_dirty.set(true);
        let rect = RECT {
            left: 0,
            top: self.list_top(),
            right: self.paint_width(),
            bottom: self.height,
        };
        unsafe { InvalidateRect(self.hwnd, &rect, 0) };
    }

    fn redraw_toolbar(&self) {
        self.paint_dirty.set(true);
        let mut rect = self.toolbar_rect(ToolbarButton::Compact);
        rect.right = self.toolbar_rect(ToolbarButton::Agent).right;
        let offset = self.width - self.paint_width();
        rect.left -= offset;
        rect.right -= offset;
        unsafe { InvalidateRect(self.hwnd, &rect, 0) };
    }

    fn rebuild_rows(&mut self) {
        let (project_height, session_height) = if self.preferences.compact {
            (self.px(30), self.px(36))
        } else {
            (self.px(37), self.px(43))
        };
        let mut y = 0;
        self.rows.clear();
        let collapsed = if self.query.is_empty() {
            &self.collapsed
        } else {
            &self.search_collapsed
        };
        for (index, project) in self.snapshot.projects.iter().enumerate() {
            let matches = matching_sessions(project, &self.query);
            if matches.is_empty() {
                continue;
            }
            self.rows.push(Row {
                kind: RowKind::Project(index, matches.len()),
                y,
                height: project_height,
            });
            y += project_height;
            if !collapsed.contains(&project.key) {
                for session in matches {
                    self.rows.push(Row {
                        kind: RowKind::Session(index, session),
                        y,
                        height: session_height,
                    });
                    y += session_height;
                }
            }
        }
        self.content_height = y;
        self.scroll = self.scroll.min(self.max_scroll());
        self.hover = None;
        self.clear_path_tip();
    }

    pub fn is_searching(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn active_collapsed(&self) -> &HashSet<String> {
        if self.is_searching() {
            &self.search_collapsed
        } else {
            &self.collapsed
        }
    }

    fn update_query(&mut self) {
        let query = self.controls.query().trim().to_lowercase();
        if self.query != query {
            self.query = query;
            self.search_collapsed.clear();
            self.scroll = 0;
            self.rebuild_rows();
            self.redraw_list();
        }
    }

    fn receive_updates(&mut self) {
        let Ok(snapshot) = self.updates.try_recv() else {
            return;
        };
        if snapshot.error.is_some() && self.loaded {
            self.snapshot.error = snapshot.error;
            self.redraw_list();
            return;
        }
        let current_keys: HashSet<_> = self
            .snapshot
            .projects
            .iter()
            .map(|p| p.key.as_str())
            .collect();
        for (index, project) in snapshot.projects.iter().enumerate() {
            if index != 0 && !current_keys.contains(project.key.as_str()) {
                self.collapsed.insert(project.key.clone());
            }
        }
        self.snapshot = snapshot;
        self.loaded = true;
        self.rebuild_rows();
        self.redraw_list();
    }

    fn place_on(&mut self, monitor: HMONITOR) {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..MONITORINFO::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        self.dpi = dpi_x as i32;
        self.work_area = info.rcWork;
        self.screen_left = info.rcMonitor.left;
        let inset = self.px(WINDOW_INSET);
        let left = info.rcWork.left + inset;
        let top = info.rcWork.top + inset;
        self.width = self.px(420).min((info.rcWork.right - left - inset).max(1));
        self.height = (info.rcWork.bottom - top - inset).max(1);
        self.frame = RECT {
            left,
            top,
            right: left + self.width,
            bottom: top + self.height,
        };
        let swept_area = RECT {
            left: self.screen_left - self.width,
            right: self.screen_left,
            ..self.frame
        };
        self.slide_offscreen = !contains_monitor(swept_area);
        self.rebuild_rows();
        self.redraw();
    }

    fn open(&mut self, monitor: HMONITOR, from_tray: bool) {
        if self.shown_width == 0 {
            self.place_on(monitor);
        }
        let now = Instant::now();
        if !self.opening {
            self.last_tick = now;
        }
        self.opening = true;
        self.outside_since = None;
        self.edge_since = None;
        self.enter_by = from_tray.then_some(now + TRAY_ENTRY_GRACE);
        self.edge_bridge = !from_tray;
    }

    fn open_from_tray(&mut self) {
        if !self.opening
            && let Some(point) = cursor()
        {
            self.open(
                unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) },
                true,
            );
        }
    }

    fn close(&mut self) {
        self.opening = false;
        self.edge_blocked = true;
        self.edge_bridge = false;
        self.enter_by = None;
        self.hover = None;
        self.set_toolbar_hover(None);
        self.clear_path_tip();
        self.redraw_list();
    }

    fn clear_path_tip(&mut self) {
        self.path_hover_since = None;
        self.controls.hide_tip();
    }

    fn set_hover(&mut self, hover: Option<usize>) -> bool {
        if self.hover == hover {
            return false;
        }
        self.hover = hover;
        self.clear_path_tip();
        self.path_hover_blocked = false;
        self.hover_name_right = match hover.map(|index| self.rows[index].kind) {
            Some(RowKind::Project(project, _)) => {
                let name = &self.snapshot.projects[project].name;
                (self.px(44)
                    + paint::folder_name_width(self.hwnd, name, self.px(paint::FOLDER_FONT_SIZE)))
                .min(self.width - self.px(64))
            }
            _ => 0,
        };
        true
    }

    fn update_hover(&mut self, point: POINT, now: Instant) {
        let local = POINT {
            x: point.x - self.frame.left,
            y: point.y - self.frame.top,
        };
        let hover = self.row_at(local);
        if self.set_hover(hover) {
            self.redraw_list();
        }
        let toolbar = self.toolbar_at(local);
        self.set_toolbar_hover(toolbar);
        if let Some(button) = toolbar {
            let since = *self.path_hover_since.get_or_insert(now);
            if now.duration_since(since) >= TOOLTIP_DELAY {
                let text = match button {
                    ToolbarButton::Compact if self.preferences.compact => {
                        tr!("Switch to comfortable layout", "切换为舒展布局")
                    }
                    ToolbarButton::Compact => tr!("Switch to compact layout", "切换为紧凑布局"),
                    ToolbarButton::Startup if self.preferences.startup => {
                        tr!("Disable startup on sign-in", "关闭开机启动")
                    }
                    ToolbarButton::Startup => tr!("Enable startup on sign-in", "开启开机启动"),
                    ToolbarButton::Agent if self.agent_mode => {
                        tr!("Turn off Agent mode", "关闭 Agent 模式")
                    }
                    ToolbarButton::Agent => tr!("Turn on Agent mode", "开启 Agent 模式"),
                };
                let anchor = POINT {
                    x: self.frame.right - self.px(188),
                    y: self.frame.top + self.toolbar_rect(button).bottom + self.px(5),
                };
                self.controls.show_tip(text, anchor);
            }
            return;
        }
        if let Some(index) = hover
            && let RowKind::Project(project, _) = self.rows[index].kind
            && (self.px(44)..self.hover_name_right).contains(&local.x)
        {
            if self.path_hover_blocked {
                return;
            }
            let since = *self.path_hover_since.get_or_insert(now);
            if now.duration_since(since) >= TOOLTIP_DELAY {
                let row = &self.rows[index];
                let top = row.y + self.list_top() - self.scroll + self.frame.top;
                let below = top + row.height;
                let y = if below + self.px(64) <= self.frame.bottom {
                    below
                } else {
                    top - self.px(52)
                };
                let anchor = POINT {
                    x: self.frame.left + self.px(42),
                    y,
                };
                self.controls
                    .show_tip(&self.snapshot.projects[project].path, anchor);
            }
        } else {
            self.path_hover_blocked = false;
            self.clear_path_tip();
        }
    }

    fn set_toolbar_hover(&mut self, button: Option<ToolbarButton>) {
        if self.toolbar_hover != button {
            self.toolbar_hover = button;
            self.clear_path_tip();
            self.redraw_toolbar();
        }
    }

    fn move_window(&mut self, new_size: bool) {
        let x = if self.slide_offscreen {
            let inset = i64::from(self.frame.left - self.screen_left);
            let revealed_inset = i64::from(self.shown_width) * inset / i64::from(self.width);
            self.screen_left + revealed_inset as i32 + self.shown_width - self.width
        } else {
            self.frame.left
        };
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        if self.slide_offscreen && !new_size {
            flags |= SWP_NOSIZE;
        }
        if new_size || !self.slide_offscreen {
            let (font, padding, tip_width) = (self.px(15), self.px(8), self.px(350));
            self.controls.layout(
                self.visible_search_rect(),
                font,
                padding,
                tip_width,
                self.search_corner_diameter(),
            );
        }
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                self.frame.top,
                self.paint_width(),
                self.height,
                flags,
            );
        }
    }

    pub fn paint_width(&self) -> i32 {
        if self.slide_offscreen {
            self.width
        } else {
            self.shown_width
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.receive_updates();
        let terminal = terminal_bounds(self.work_area, self.frame, self.px(8));
        if let Err(error) = self
            .agent_terminals
            .update(terminal, self.agent_mode && self.opening)
        {
            show_owned_error(self.hwnd, &error);
        }

        if let Some(point) = cursor() {
            if self.opening {
                if contains(self.frame, point) {
                    self.outside_since = None;
                    self.enter_by = None;
                    self.edge_bridge = false;
                    if self.progress == 1.0 {
                        self.update_hover(point, now);
                    }
                } else if self.agent_mode {
                    if self.set_hover(None) {
                        self.redraw_list();
                    }
                    self.set_toolbar_hover(None);
                } else if self.edge_bridge
                    && (self.screen_left..self.frame.left).contains(&point.x)
                    && (self.frame.top..self.frame.bottom).contains(&point.y)
                {
                    self.outside_since = None;
                } else if self.enter_by.is_none_or(|deadline| now >= deadline) {
                    self.clear_path_tip();
                    self.edge_bridge = false;
                    let since = self.outside_since.get_or_insert(now);
                    if now.duration_since(*since) >= HIDE_DELAY {
                        self.close();
                    }
                }
            } else if self.progress == 0.0 {
                let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..MONITORINFO::default()
                };
                if !monitor.is_null()
                    && unsafe { GetMonitorInfoW(monitor, &mut info) } != 0
                    && point.x - info.rcMonitor.left < EDGE_WIDTH
                    && point.y >= info.rcWork.top + self.px(WINDOW_INSET)
                    && point.y < info.rcWork.bottom - self.px(WINDOW_INSET)
                {
                    if !self.edge_blocked {
                        match self.edge_since {
                            Some((last_monitor, since)) if last_monitor == monitor => {
                                if now.duration_since(since) >= EDGE_DELAY {
                                    if fullscreen_at_edge(&info) {
                                        self.edge_since = None;
                                        self.edge_blocked = true;
                                    } else {
                                        self.open(monitor, false);
                                    }
                                }
                            }
                            _ => self.edge_since = Some((monitor, now)),
                        }
                    }
                } else {
                    self.edge_since = None;
                    self.edge_blocked = false;
                }
            }
        }

        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let step = elapsed.as_secs_f32() / 0.23;
        self.progress = if self.opening {
            (self.progress + step).min(1.0)
        } else {
            (self.progress - step).max(0.0)
        };
        let eased = self.progress * self.progress * (3.0 - 2.0 * self.progress);
        let visible_width = (eased * self.width as f32).round() as i32;
        if visible_width != self.shown_width {
            let was_hidden = self.shown_width == 0;
            self.shown_width = visible_width;
            if visible_width == 0 {
                unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            } else {
                self.move_window(was_hidden);
                if was_hidden {
                    self.configure_backdrop();
                } else if !self.slide_offscreen {
                    self.invalidate();
                }
            }
        }
        let animating = if self.opening {
            self.progress < 1.0
        } else {
            self.progress > 0.0
        };
        if self.animating != animating {
            self.animating = animating;
            self.update_frame_timer();
        }
    }

    fn update_frame_timer(&self) {
        unsafe {
            if self.animating {
                if !self.frame_timer.is_null()
                    && SetWaitableTimer(self.frame_timer, &-1, FRAME_INTERVAL_MS, None, null(), 0)
                        != 0
                {
                    KillTimer(self.hwnd, 1);
                } else {
                    SetTimer(self.hwnd, 1, FALLBACK_TIMER_MS, None);
                }
            } else {
                if !self.frame_timer.is_null() {
                    CancelWaitableTimer(self.frame_timer);
                }
                SetTimer(self.hwnd, 1, IDLE_TIMER_MS, None);
            }
        }
    }

    fn row_at(&self, point: POINT) -> Option<usize> {
        if point.x < self.px(12)
            || point.x >= self.width - self.px(13)
            || point.y < self.list_top()
            || point.y >= self.list_bottom()
        {
            return None;
        }
        let y = point.y - self.list_top() + self.scroll;
        let index = self.rows.partition_point(|row| row.y + row.height <= y);
        (index < self.rows.len()).then_some(index)
    }

    fn click(&mut self, lparam: LPARAM, double: bool) {
        let point = POINT {
            x: lparam as i16 as i32 + self.width - self.paint_width(),
            y: (lparam >> 16) as i16 as i32,
        };
        if let Some(button) = self.toolbar_at(point) {
            if !double {
                self.toggle_preference(button);
            }
            return;
        }
        if let Some(index) = self.row_at(point) {
            match self.rows[index].kind {
                RowKind::Project(index, _) if !double => {
                    let key = &self.snapshot.projects[index].key;
                    let collapsed = if self.query.is_empty() {
                        &mut self.collapsed
                    } else {
                        &mut self.search_collapsed
                    };
                    if !collapsed.remove(key) {
                        collapsed.insert(key.clone());
                    }
                    self.rebuild_rows();
                    self.set_hover(self.row_at(point));
                    self.path_hover_blocked = true;
                    self.redraw_list();
                }
                RowKind::Session(project, session) => {
                    let project = &self.snapshot.projects[project];
                    let session = &project.sessions[session];
                    self.selected = Some((project.key.clone(), session.id.clone()));
                    self.redraw_list();
                    if self.agent_mode && !double {
                        let result = session
                            .cwd
                            .as_deref()
                            .ok_or_else(|| {
                                tr!(
                                    "This session has no recorded working directory.",
                                    "这条会话没有记录原工作目录。"
                                )
                                .to_owned()
                            })
                            .and_then(|cwd| self.agent_terminals.open(cwd, &session.id));
                        if let Err(error) = result {
                            show_owned_error(self.hwnd, &error);
                        }
                    } else if !self.agent_mode && double {
                        if let Err(message) = resume(session) {
                            show_error(&message);
                        } else {
                            self.close();
                        }
                    }
                }
                _ => {}
            }
        } else if !double && point.x >= self.width - self.px(13) {
            let height = self.list_bottom() - self.list_top();
            if height > 0 && (self.list_top()..self.list_bottom()).contains(&point.y) {
                self.scroll = ((point.y - self.list_top()) as i64 * self.max_scroll() as i64
                    / height as i64) as i32;
                self.hover = None;
                self.clear_path_tip();
                self.redraw_list();
            }
        }
    }

    fn row_menu(&mut self, lparam: LPARAM) {
        let mut point = POINT {
            x: lparam as i16 as i32,
            y: (lparam >> 16) as i16 as i32,
        };
        let local = POINT {
            x: point.x + self.width - self.paint_width(),
            y: point.y,
        };
        let Some(row) = self.row_at(local) else {
            return;
        };
        let kind = self.rows[row].kind;
        let items = match kind {
            RowKind::Project(..) => [tr!("New session", "新建会话"), tr!("Delete", "删除")],
            RowKind::Session(..) => [
                tr!("Delete session", "删除会话"),
                tr!("Rename session", "重命名会话"),
            ],
        };
        self.clear_path_tip();
        let choice = unsafe {
            ClientToScreen(self.hwnd, &mut point);
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            append_menu(menu, MF_STRING, 1, items[0]);
            append_menu(menu, MF_STRING, 2, items[1]);
            SetForegroundWindow(self.hwnd);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.hwnd,
                null(),
            );
            DestroyMenu(menu);
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            choice
        };
        self.outside_since = None;
        let result = self.run_row_action(kind, choice);
        if let Err(message) = result {
            show_owned_error(self.hwnd, &message);
        }
        self.redraw();
    }

    fn run_row_action(&mut self, kind: RowKind, choice: i32) -> Result<(), String> {
        match (kind, choice) {
            (RowKind::Project(project, _), 1) => {
                let project = &self.snapshot.projects[project];
                let cwd = project
                    .sessions
                    .iter()
                    .find_map(|session| session.cwd.as_deref())
                    .ok_or(tr!(
                        "Could not find the working directory for this folder.",
                        "找不到该文件夹的工作目录。"
                    ))?;
                terminal::launch(cwd, &[])?;
                self.close();
            }
            (RowKind::Project(project, _), 2) => {
                let project = &self.snapshot.projects[project];
                let message = trf!(
                    "Delete the {} local Claude Code sessions in “{}”?\n\nSession records and their supporting data will go to the Recycle Bin when supported. Project files will be kept.",
                    "删除 {} 条本地 Claude Code 会话（「{}」）？\n\n会话记录及其附属数据将移至回收站（若支持）。项目目录和项目文件会保留。",
                    project.sessions.len(),
                    project.name,
                );
                self.delete_sessions(&project.sessions, &message)?;
            }
            (RowKind::Session(project, session), 1) => {
                let session = &self.snapshot.projects[project].sessions[session];
                let message = trf!(
                    "Delete the session “{}”?\n\nIts local records and supporting data will go to the Recycle Bin when supported.",
                    "删除会话「{}」？\n\n本地会话记录及其附属数据将移至回收站（若支持）。",
                    session.title,
                );
                self.delete_sessions(std::slice::from_ref(session), &message)?;
            }
            (RowKind::Session(project, session), 2) => {
                let session = &self.snapshot.projects[project].sessions[session];
                let title = dialogs::rename_prompt(self.hwnd, &session.title).map_err(|error| {
                    trf!(
                        "Could not open the rename dialog: {error}",
                        "无法打开重命名窗口：{error}"
                    )
                })?;
                if let Some(title) = title.filter(|title| title != &session.title) {
                    sessions::rename(session, &title).map_err(|error| {
                        trf!(
                            "Could not rename the session: {error}",
                            "重命名失败：{error}"
                        )
                    })?;
                    let _ = self.refresh.try_send(());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn delete_sessions(&self, sessions: &[Session], message: &str) -> Result<(), String> {
        if !dialogs::confirm_delete(self.hwnd, message) {
            return Ok(());
        }
        let result = sessions::deletion_paths(sessions)
            .map_err(|error| {
                trf!(
                    "Could not locate the session records to delete: {error}",
                    "无法定位要删除的会话记录：{error}"
                )
            })
            .and_then(|paths| dialogs::recycle(self.hwnd, &paths));
        // Refresh after cancellation or partial success as well.
        let _ = self.refresh.try_send(());
        result
    }

    fn tray_menu(&mut self) {
        let Some(point) = cursor() else { return };
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            let toggle = if self.opening {
                tr!("Hide panel", "隐藏面板")
            } else {
                tr!("Open panel", "打开面板")
            };
            append_menu(menu, MF_STRING, 1, toggle);
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_menu(
                menu,
                MF_STRING
                    | if self.preferences.compact {
                        MF_CHECKED
                    } else {
                        0
                    },
                3,
                tr!("Compact layout", "紧凑布局"),
            );
            append_menu(
                menu,
                MF_STRING
                    | if self.preferences.startup {
                        MF_CHECKED
                    } else {
                        0
                    },
                4,
                tr!("Start on sign-in", "开机启动"),
            );
            let languages = CreatePopupMenu();
            if !languages.is_null() {
                for (id, preference, label) in [
                    (5, Preference::System, tr!("System default", "跟随系统")),
                    (6, Preference::English, "English"),
                    (7, Preference::Chinese, "简体中文"),
                ] {
                    let checked = if self.preferences.language == preference {
                        MF_CHECKED
                    } else {
                        0
                    };
                    append_menu(languages, MF_STRING | checked, id, label);
                }
                append_menu(
                    menu,
                    MF_POPUP | MF_STRING,
                    languages as usize,
                    tr!("Language", "语言"),
                );
            }
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_menu(menu, MF_STRING, 2, tr!("Exit", "退出"));
            SetForegroundWindow(self.hwnd);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.hwnd,
                null(),
            );
            DestroyMenu(menu);
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            match choice {
                1 if self.opening => self.close(),
                1 => self.open_from_tray(),
                2 => {
                    PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                }
                3 => self.toggle_preference(ToolbarButton::Compact),
                4 => self.toggle_preference(ToolbarButton::Startup),
                5 => self.change_language(Preference::System),
                6 => self.change_language(Preference::English),
                7 => self.change_language(Preference::Chinese),
                _ => {}
            }
        }
    }

    fn toggle_preference(&mut self, button: ToolbarButton) {
        self.clear_path_tip();
        let mut next = self.preferences;
        match button {
            ToolbarButton::Compact => next.compact = !next.compact,
            ToolbarButton::Startup => next.startup = !next.startup,
            ToolbarButton::Agent => {
                self.agent_mode = !self.agent_mode;
                self.outside_since = None;
                if !self.agent_mode {
                    self.agent_terminals.cancel_pending();
                }
                self.redraw_toolbar();
                return;
            }
        }
        if self.save_preferences(next) && button == ToolbarButton::Compact {
            self.rebuild_rows();
            self.redraw_list();
        }
        self.redraw_toolbar();
    }

    fn change_language(&mut self, language: Preference) {
        if !self.save_preferences(Preferences {
            language,
            ..self.preferences
        }) {
            return;
        }
        i18n::select(language);
        self.controls.update_language();
        let title = app_title();
        unsafe { SetWindowTextW(self.hwnd, title.as_ptr()) };
        tray_icon(self.hwnd, NIM_MODIFY);
        self.clear_path_tip();
        let _ = self.refresh.try_send(());
        self.redraw();
    }

    fn save_preferences(&mut self, next: Preferences) -> bool {
        if self.preferences == next {
            return true;
        }
        match settings::update(self.preferences, next) {
            Ok(()) => {
                self.preferences = next;
                true
            }
            Err(error) => {
                show_owned_error(
                    self.hwnd,
                    &trf!("Could not save settings: {error}", "无法保存设置：{error}"),
                );
                false
            }
        }
    }

    fn configure_backdrop(&mut self) {
        let dark = 1i32;
        let attribute = DWMWA_SYSTEMBACKDROP_TYPE as u32;
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        unsafe {
            let disable_transitions = 1i32;
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED as u32,
                (&raw const disable_transitions).cast(),
                4,
            );
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                (&raw const dark).cast(),
                4,
            );
            let none = DWMSBT_NONE;
            DwmSetWindowAttribute(self.hwnd, attribute, (&raw const none).cast(), 4);
            let extended = DwmExtendFrameIntoClientArea(self.hwnd, &margins) >= 0;
            let acrylic = DWMSBT_TRANSIENTWINDOW;
            self.backdrop_tint = if extended && enable_accent_acrylic(self.hwnd) {
                // The accent already supplies the tint. Leave the rest of the client transparent.
                0
            } else if extended
                && DwmSetWindowAttribute(self.hwnd, attribute, (&raw const acrylic).cast(), 4) >= 0
            {
                62
            } else {
                255
            };
            let rounded = DWMWCP_ROUND;
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                (&raw const rounded).cast(),
                4,
            );
        }
        self.redraw();
    }
}

fn contains_monitor(area: RECT) -> bool {
    unsafe extern "system" fn mark_found(
        _: HMONITOR,
        _: HDC,
        _: *mut RECT,
        context: LPARAM,
    ) -> i32 {
        unsafe { *(context as *mut bool) = true };
        0
    }

    let mut found = false;
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            &area,
            Some(mark_found),
            (&raw mut found) as LPARAM,
        );
    }
    found
}

fn enable_accent_acrylic(hwnd: HWND) -> bool {
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }
    #[repr(C)]
    struct CompositionAttribute {
        attribute: u32,
        data: *mut c_void,
        size: usize,
    }
    // This compatibility entry point keeps the effect on a non-activating popup.
    // Resolve it at runtime so unsupported Windows builds can use the DWM backdrop.
    unsafe {
        let user32 = GetModuleHandleW(w!("user32.dll"));
        let Some(address) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            return false;
        };
        let set_attribute = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "system" fn(HWND, *mut CompositionAttribute) -> i32,
        >(address);
        let mut policy = AccentPolicy {
            state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
            flags: 0,
            gradient_color: 0x99261e19,
            animation_id: 0,
        };
        let mut attribute = CompositionAttribute {
            attribute: 19, // WCA_ACCENT_POLICY
            data: (&raw mut policy).cast(),
            size: size_of::<AccentPolicy>(),
        };
        set_attribute(hwnd, &mut attribute) != 0
    }
}

fn cursor() -> Option<POINT> {
    let mut point = POINT::default();
    (unsafe { GetCursorPos(&mut point) } != 0).then_some(point)
}

fn matching_sessions(project: &Project, query: &str) -> Vec<usize> {
    if query.is_empty() || project.name.to_lowercase().contains(query) {
        return (0..project.sessions.len()).collect();
    }
    project
        .sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| {
            session
                .title
                .to_lowercase()
                .contains(query)
                .then_some(index)
        })
        .collect()
}

fn contains(rect: RECT, point: POINT) -> bool {
    (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
}

fn fullscreen_at_edge(monitor: &MONITORINFO) -> bool {
    unsafe {
        let mut state = 0;
        if SHQueryUserNotificationState(&mut state) >= 0 && state == QUNS_RUNNING_D3D_FULL_SCREEN {
            return true;
        }
        let foreground = GetForegroundWindow();
        if foreground.is_null()
            || foreground == GetShellWindow()
            || foreground == GetDesktopWindow()
            || IsIconic(foreground) != 0
        {
            return false;
        }
        let mut owner = 0;
        GetWindowThreadProcessId(foreground, &mut owner);
        if owner == std::process::id() {
            return false;
        }
        let mut bounds = RECT::default();
        if DwmGetWindowAttribute(
            foreground,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut bounds).cast(),
            size_of::<RECT>() as u32,
        ) < 0
            && GetWindowRect(foreground, &mut bounds) == 0
        {
            return false;
        }
        let style = GetWindowLongPtrW(foreground, GWL_STYLE) as u32;
        let maximized_with_caption = IsZoomed(foreground) != 0 && style & WS_CAPTION == WS_CAPTION;
        is_fullscreen_bounds(bounds, monitor.rcMonitor, maximized_with_caption)
    }
}

fn is_fullscreen_bounds(window: RECT, screen: RECT, maximized_with_caption: bool) -> bool {
    // Allow up to two physical pixels for compositor/driver rounding.
    !maximized_with_caption
        && window.left <= screen.left + 2
        && window.top <= screen.top + 2
        && window.right >= screen.right - 2
        && window.bottom >= screen.bottom - 2
}

fn terminal_bounds(work: RECT, panel: RECT, gap: i32) -> RECT {
    RECT {
        left: (panel.right + gap).clamp(work.left, work.right),
        ..work
    }
}

fn resume(session: &Session) -> Result<(), String> {
    let cwd = session.cwd.as_ref().ok_or(tr!(
        "This session has no recorded working directory.",
        "这条会话没有记录原工作目录。"
    ))?;
    terminal::launch(cwd, &["--resume", &session.id])
}

pub fn show_error(message: &str) {
    show_owned_error(null_mut(), message);
}

fn show_owned_error(owner: HWND, message: &str) {
    let message = i18n::wide(message);
    let title = app_title();
    unsafe {
        MessageBoxW(
            owner,
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

fn app_title() -> Vec<u16> {
    i18n::wide(tr!("Claude Code Panel", "Claude Code 面板"))
}

fn append_menu(menu: HMENU, flags: u32, id: usize, label: &str) {
    let label = i18n::wide(label);
    unsafe { AppendMenuW(menu, flags, id, label.as_ptr()) };
}

fn tray_icon(hwnd: HWND, operation: u32) {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: TRAY_EVENT,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        ..NOTIFYICONDATAW::default()
    };
    for (target, value) in data.szTip.iter_mut().zip(app_title()) {
        *target = value;
    }
    unsafe {
        if Shell_NotifyIconW(operation, &data) != 0 && operation == NIM_ADD {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return 1,
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        WM_CTLCOLOREDIT => unsafe {
            let dc = wparam as HDC;
            SetBkColor(dc, paint::INPUT_BACKGROUND);
            SetTextColor(dc, paint::INPUT_TEXT);
            SetDCBrushColor(dc, paint::INPUT_BACKGROUND);
            return GetStockObject(DC_BRUSH) as LRESULT;
        },
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            return 0;
        }
        WM_DESTROY => {
            tray_icon(hwnd, NIM_DELETE);
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        _ => {}
    }

    let handled = STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return false;
        };
        let Some(panel) = state.as_mut() else {
            return false;
        };
        match msg {
            WM_TIMER if wparam == 1 => panel.tick(),
            WM_PAINT => paint::paint(hwnd, panel),
            WM_COMMAND if wparam & 0xffff == SEARCH_ID && (wparam >> 16) as u32 == EN_CHANGE => {
                panel.update_query();
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => panel.click(lparam, msg == WM_LBUTTONDBLCLK),
            WM_RBUTTONUP => panel.row_menu(lparam),
            WM_MOUSEWHEEL => {
                let delta = (wparam >> 16) as i16 as i32;
                panel.scroll =
                    (panel.scroll - delta * panel.px(108) / 120).clamp(0, panel.max_scroll());
                panel.hover = None;
                panel.clear_path_tip();
                panel.redraw_list();
            }
            OPEN_PANEL => panel.open_from_tray(),
            TRAY_EVENT => match lparam as u32 & 0xffff {
                WM_LBUTTONDOWN | WM_LBUTTONUP | NIN_SELECT | TRAY_KEY_SELECT => {
                    panel.open_from_tray()
                }
                WM_CONTEXTMENU | WM_RBUTTONUP => panel.tray_menu(),
                _ => {}
            },
            WM_DWMCOMPOSITIONCHANGED | WM_SETTINGCHANGE => panel.configure_backdrop(),
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                if panel.shown_width > 0 {
                    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                    panel.place_on(monitor);
                    panel.shown_width = panel.shown_width.min(panel.width);
                    panel.move_window(true);
                }
            }
            _ if msg == panel.taskbar_created => {
                tray_icon(hwnd, NIM_ADD);
            }
            _ => return false,
        }
        true
    });
    if handled {
        0
    } else {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

pub fn run(preferences: Preferences) -> io::Result<()> {
    unsafe {
        let mutex = CreateMutexW(null(), 0, w!("Local\\ClaudeCodePanel.Singleton"));
        let mutex_status = GetLastError();
        if mutex.is_null() {
            return Err(io::Error::from_raw_os_error(mutex_status as i32));
        }
        if mutex_status == ERROR_ALREADY_EXISTS {
            if !std::env::args_os()
                .skip(1)
                .any(|arg| arg == settings::STARTUP_ARG)
            {
                let hwnd = FindWindowW(CLASS, null());
                if !hwnd.is_null() {
                    PostMessageW(hwnd, OPEN_PANEL, 0, 0);
                }
            }
            CloseHandle(mutex);
            return Ok(());
        }
        let result = message_loop(preferences);
        CloseHandle(mutex);
        result
    }
}

unsafe fn message_loop(preferences: Preferences) -> io::Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            style: CS_DBLCLKS | CS_DROPSHADOW,
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpfnWndProc: Some(window_proc),
            lpszClassName: CLASS,
            ..WNDCLASSW::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(io::Error::last_os_error());
        }
        let title = app_title();
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }
        let controls = match Controls::new(hwnd, instance) {
            Ok(controls) => controls,
            Err(error) => {
                DestroyWindow(hwnd);
                return Err(error);
            }
        };
        let timer = CreateWaitableTimerExW(
            null(),
            null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
        );
        let mut panel = Panel::new(hwnd, timer, controls, preferences);
        panel.configure_backdrop();
        STATE.with(|state| state.replace(Some(panel)));
        tray_icon(hwnd, NIM_ADD);
        let result = if SetTimer(hwnd, 1, IDLE_TIMER_MS, None) == 0 {
            let error = io::Error::last_os_error();
            DestroyWindow(hwnd);
            Err(error)
        } else {
            pump_messages(hwnd, timer)
        };
        STATE.with(|state| state.replace(None));
        if !timer.is_null() {
            CloseHandle(timer);
        }
        result
    }
}

fn pump_messages(hwnd: HWND, timer: HANDLE) -> io::Result<()> {
    let timer_count = u32::from(!timer.is_null());
    unsafe {
        let mut message = MSG::default();
        loop {
            let event = MsgWaitForMultipleObjectsEx(
                timer_count,
                &timer,
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if event == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            if timer_count != 0 && event == WAIT_OBJECT_0 {
                SendMessageW(hwnd, WM_TIMER, 1, 0);
            }
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    return Ok(());
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn fullscreen_requires_the_monitor_bounds_and_ignores_captioned_maximized_windows() {
        let screen = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let work = RECT {
            bottom: 1040,
            ..screen
        };
        let other_display = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert!(is_fullscreen_bounds(screen, screen, false));
        assert!(!is_fullscreen_bounds(work, screen, false));
        assert!(!is_fullscreen_bounds(other_display, screen, false));
        assert!(!is_fullscreen_bounds(screen, screen, true));
    }

    #[test]
    fn agent_terminal_reserves_panel_space_on_a_secondary_monitor() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let panel = RECT {
            left: -1906,
            top: 14,
            right: -1486,
            bottom: 1026,
        };
        let bounds = terminal_bounds(work, panel, 8);
        assert_eq!(
            (bounds.left, bounds.top, bounds.right, bounds.bottom),
            (-1478, 0, 0, 1040)
        );
    }

    #[test]
    fn filters_session_titles_and_keeps_all_sessions_for_a_folder_match() {
        let project = Project {
            key: "project".into(),
            name: "桌面 Panel".into(),
            path: "D:\\private-root\\Panel".into(),
            sessions: ["修复动画", "Review UI"]
                .into_iter()
                .map(|title| Session {
                    id: String::new(),
                    title: title.into(),
                    cwd: None,
                    modified: SystemTime::UNIX_EPOCH,
                    transcripts: Vec::new(),
                })
                .collect(),
        };
        assert_eq!(matching_sessions(&project, "panel"), [0, 1]);
        assert_eq!(matching_sessions(&project, "review"), [1]);
        assert_eq!(matching_sessions(&project, "动画"), [0]);
        assert!(matching_sessions(&project, "private-root").is_empty());
    }
}
