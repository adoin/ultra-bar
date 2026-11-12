//! Event-driven window/application chooser shared by grouped and overflow tasks.
use crate::visual::text::draw_text as DrawTextW;
use crate::visual::{self, Surface, Tip, Tooltips, theme, wide};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::WM_MOUSELEAVE, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const SELECTED: u32 = WM_APP + 6;
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Select(usize),
    Close(usize),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Window { hwnd: usize, pid: u32 },
    Task(usize),
    Pin(usize),
    Launch(usize),
    CloseGroup(usize),
    CloseWindow { hwnd: usize, pid: u32 },
    ThisPc,
    SettingsCommand(usize),
    TaskbarCommand(usize),
    Jump { generation: u64, index: usize },
    Script { generation: u64, index: usize },
    RenameScript { generation: u64, index: usize },
}
impl Target {
    fn close_available(self) -> Option<bool> {
        if let Self::Window { hwnd, pid } = self {
            crate::window_commands::close_available(hwnd as HWND, pid)
        } else {
            None
        }
    }
    pub fn valid(self) -> bool {
        match self {
            Self::Task(_)
            | Self::Pin(_)
            | Self::Launch(_)
            | Self::CloseGroup(_)
            | Self::Jump { .. }
            | Self::Script { .. }
            | Self::RenameScript { .. }
            | Self::TaskbarCommand(_)
            | Self::ThisPc => true,
            Self::SettingsCommand(_) => true,
            Self::Window { hwnd, pid } | Self::CloseWindow { hwnd, pid } => unsafe {
                let mut current = 0;
                IsWindow(hwnd as HWND) != 0
                    && GetWindowThreadProcessId(hwnd as HWND, &mut current) != 0
                    && current == pid
            },
        }
    }
}
pub struct Item {
    pub target: Target,
    pub title: String,
    icon: HICON,
    pub active: bool,
    pub enabled: bool,
    glyph: Option<&'static str>,
    pub separator_before: bool,
    pub tooltip: Option<String>,
}
impl Item {
    pub unsafe fn new(target: Target, title: String, icon: HICON, active: bool) -> Self {
        unsafe {
            Self {
                target,
                title,
                icon: CopyIcon(icon),
                active,
                enabled: true,
                glyph: None,
                separator_before: false,
                tooltip: None,
            }
        }
    }
    pub fn command(target: Target, title: &str, glyph: &'static str, enabled: bool) -> Self {
        Self {
            target,
            title: title.into(),
            icon: null_mut(),
            active: false,
            enabled,
            glyph: Some(glyph),
            separator_before: false,
            tooltip: None,
        }
    }
}
impl Drop for Item {
    fn drop(&mut self) {
        unsafe {
            if !self.icon.is_null() {
                DestroyIcon(self.icon);
            }
        }
    }
}
struct State {
    hwnd: HWND,
    owner: HWND,
    key: String,
    title: String,
    items: Vec<Item>,
    selection: Option<Target>,
    dpi: i32,
    font: HFONT,
    icons: HFONT,
    glass: bool,
    anchor: RECT,
    rows: usize,
    scroll: usize,
    focused: usize,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    tips: Option<Tooltips>,
    export: Option<std::path::PathBuf>,
}
pub struct Popup {
    state: Box<State>,
}
impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            self.state.tips.take();
            if IsWindow(self.state.hwnd) != 0 {
                DestroyWindow(self.state.hwnd);
            }
            ultra_bar_native_ui::fonts::delete_object(self.state.font);
            ultra_bar_native_ui::fonts::delete_object(self.state.icons);
        }
    }
}
impl Popup {
    pub unsafe fn new(
        owner: HWND,
        key: String,
        title: String,
        items: Vec<Item>,
        anchor: RECT,
        dpi: i32,
    ) -> Option<Self> {
        unsafe {
            if items.is_empty() {
                return None;
            }
            let module = GetModuleHandleW(null());
            let class = wide("UltraBar.TaskPopup");
            RegisterClassW(&WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(wndproc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..zeroed()
            });
            let font = |family: &str, size: i32| {
                ultra_bar_native_ui::fonts::create_font(
                    -size * dpi / 96,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as _,
                    OUT_DEFAULT_PRECIS as _,
                    CLIP_DEFAULT_PRECIS as _,
                    CLEARTYPE_NATURAL_QUALITY as _,
                    DEFAULT_PITCH as _,
                    wide(family).as_ptr(),
                )
            };
            let focused = items
                .iter()
                .position(|item| item.active && item.enabled)
                .or_else(|| items.iter().position(|item| item.enabled))
                .unwrap_or(0);
            let mut state = Box::new(State {
                hwnd: null_mut(),
                owner,
                key,
                title,
                items,
                selection: None,
                dpi,
                font: font("Segoe UI", 13),
                icons: font("Segoe MDL2 Assets", 15),
                glass: false,
                anchor,
                rows: 8,
                scroll: 0,
                focused,
                hover: None,
                pressed: None,
                tips: None,
                export: None,
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.as_ptr(),
                wide(&state.title).as_ptr(),
                WS_POPUP | WS_SYSMENU,
                0,
                0,
                1,
                1,
                owner,
                null_mut(),
                module,
                (&mut *state as *mut State).cast(),
            );
            if hwnd.is_null() {
                ultra_bar_native_ui::fonts::delete_object(state.font);
                ultra_bar_native_ui::fonts::delete_object(state.icons);
                return None;
            }
            state.hwnd = hwnd;
            state.glass = visual::enable_glass(hwnd);
            state.tips = Some(Tooltips::new(hwnd));
            position(&mut state);
            Some(Self { state })
        }
    }
    pub fn is_open_for(&self, key: &str) -> bool {
        self.state.key == key && unsafe { IsWindowVisible(self.state.hwnd) != 0 }
    }
    pub unsafe fn hide(&mut self) {
        unsafe {
            ShowWindow(self.state.hwnd, SW_HIDE);
        }
    }
    pub unsafe fn show(&mut self) {
        unsafe {
            ShowWindow(self.state.hwnd, SW_SHOWNOACTIVATE);
            paint(&mut self.state);
            SetForegroundWindow(self.state.hwnd);
            SetFocus(self.state.hwnd);
            SetWindowPos(
                self.state.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
    pub unsafe fn verify_script_scroll(
        &mut self,
        directory: &std::path::Path,
        last: Target,
    ) -> anyhow::Result<()> {
        unsafe {
            anyhow::ensure!(
                self.state.rows <= 8 && self.state.items.len() > self.state.rows,
                "脚本弹层未限制最大高度"
            );
            self.state.export = Some(directory.join("scripts-first.bmp"));
            paint(&mut self.state);
            SendMessageW(
                self.state.hwnd,
                WM_MOUSEWHEEL,
                (-120i16 as u16 as usize) << 16,
                0,
            );
            anyhow::ensure!(self.state.scroll > 0, "脚本列表无法滚动");
            SendMessageW(self.state.hwnd, WM_KEYDOWN, VK_END as usize, 0);
            self.state.export = Some(directory.join("scripts-last.bmp"));
            paint(&mut self.state);
            SendMessageW(self.state.hwnd, WM_KEYDOWN, VK_RETURN as usize, 0);
            anyhow::ensure!(self.take_selection() == Some(last), "脚本列表末项无法选择");
            Ok(())
        }
    }
    pub fn take_selection(&mut self) -> Option<Target> {
        self.state.selection.take()
    }
    pub unsafe fn replace_items(&mut self, items: Vec<Item>) {
        unsafe {
            self.state.pressed = None;
            if GetCapture() == self.state.hwnd {
                ReleaseCapture();
            }
            self.state.items = items;
            self.state.scroll = 0;
            self.state.focused = self
                .state
                .items
                .iter()
                .position(|item| item.enabled)
                .unwrap_or(0);
            self.state.hover = None;
            if self.state.items.is_empty() {
                self.hide();
            } else {
                position(&mut self.state);
                InvalidateRect(self.state.hwnd, null(), 0);
            }
        }
    }
    pub unsafe fn refresh(&mut self) {
        unsafe {
            refresh(&mut self.state);
        }
    }
}
fn visible_scroll(focused: usize, scroll: usize, rows: usize, count: usize) -> usize {
    let rows = rows.max(1);
    let mut scroll = scroll.min(count.saturating_sub(rows));
    if focused < scroll {
        scroll = focused;
    } else if focused >= scroll + rows {
        scroll = focused + 1 - rows;
    }
    scroll.min(count.saturating_sub(rows))
}
fn unit(state: &State, n: i32) -> i32 {
    n * state.dpi / 96
}
fn close_rect(state: &State, row: RECT) -> RECT {
    RECT {
        left: row.right - unit(state, 34),
        right: row.right - unit(state, 6),
        top: row.top + unit(state, 6),
        bottom: row.bottom - unit(state, 6),
    }
}
fn hit(state: &State, x: i32, y: i32) -> Option<Hit> {
    if x < unit(state, 8) || y < unit(state, 8) {
        return None;
    }
    let mut rect: RECT = unsafe { zeroed() };
    unsafe { GetClientRect(state.hwnd, &mut rect) };
    if x >= rect.right - unit(state, 8) {
        return None;
    }
    let row = ((y - unit(state, 8)) / unit(state, 44).max(1)) as usize;
    if row < state.rows && state.scroll + row < state.items.len() {
        let index = state.scroll + row;
        let close = close_rect(
            state,
            RECT {
                left: unit(state, 8),
                right: rect.right - unit(state, 8),
                top: unit(state, 8 + row as i32 * 44),
                bottom: unit(state, 48 + row as i32 * 44),
            },
        );
        if state.items[index].target.close_available().is_some()
            && x >= close.left
            && x < close.right
            && y >= close.top
            && y < close.bottom
        {
            Some(Hit::Close(index))
        } else {
            Some(Hit::Select(index))
        }
    } else {
        None
    }
}
fn layout(anchor: RECT, work: RECT, dpi: i32, count: usize) -> (RECT, usize) {
    let unit = |value: i32| value * dpi / 96;
    let gap = unit(8);
    let upper = anchor
        .top
        .saturating_sub(gap)
        .min(work.bottom)
        .max(work.top);
    let lower = anchor
        .bottom
        .saturating_add(gap)
        .max(work.top)
        .min(work.bottom);
    let above = upper - work.top >= work.bottom - lower;
    let mut space = if above {
        RECT {
            bottom: upper,
            ..work
        }
    } else {
        RECT { top: lower, ..work }
    };
    if space.bottom - space.top < unit(60) {
        space = work;
    }
    let rows = ((space.bottom - space.top - unit(16)) / unit(44).max(1)).clamp(1, 8) as usize;
    let rows = rows.min(count.max(1));
    let height = unit(16 + 44 * rows as i32).min(space.bottom - space.top);
    let width = unit(392).min((work.right - work.left - gap * 2).max(1));
    let left = ((anchor.left + anchor.right - width) / 2).clamp(
        work.left + gap,
        (work.right - width - gap).max(work.left + gap),
    );
    let top = if above {
        space.bottom - height
    } else {
        space.top
    };
    (
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        },
        rows,
    )
}
unsafe fn position(state: &mut State) {
    unsafe {
        let mut monitor: MONITORINFO = zeroed();
        monitor.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(
            MonitorFromRect(&state.anchor, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        );
        let (bounds, rows) = layout(state.anchor, monitor.rcWork, state.dpi, state.items.len());
        state.rows = rows;
        state.scroll = visible_scroll(state.focused, state.scroll, state.rows, state.items.len());
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        SetWindowPos(
            state.hwnd,
            HWND_TOPMOST,
            bounds.left,
            bounds.top,
            width,
            height,
            SWP_NOACTIVATE,
        );
        SetWindowRgn(
            state.hwnd,
            CreateRoundRectRgn(0, 0, width, height, unit(state, 18), unit(state, 18)),
            1,
        );
    }
}
unsafe fn choose(state: &mut State, index: usize) {
    unsafe {
        let Some(item) = state.items.get(index) else {
            return;
        };
        if !item.enabled {
            return;
        }
        if !item.target.valid() {
            state.items.remove(index);
            if state.items.is_empty() {
                ShowWindow(state.hwnd, SW_HIDE);
            } else {
                state.focused = state.focused.min(state.items.len() - 1);
                position(state);
                InvalidateRect(state.hwnd, null(), 0);
            }
            return;
        }
        state.selection = Some(item.target);
        ShowWindow(state.hwnd, SW_HIDE);
        if !state.owner.is_null() {
            PostMessageW(state.owner, SELECTED, 0, 0);
        }
    }
}
unsafe fn refresh(state: &mut State) {
    unsafe {
        let before = state.items.len();
        state.items.retain(|item| item.target.valid());
        if before == state.items.len() {
            return;
        }
        state.hover = None;
        state.pressed = None;
        if state.items.is_empty() {
            ShowWindow(state.hwnd, SW_HIDE);
            return;
        }
        state.focused = state.focused.min(state.items.len() - 1);
        position(state);
        InvalidateRect(state.hwnd, null(), 0);
    }
}
unsafe fn close_window(state: &mut State, index: usize) {
    unsafe {
        let Some(item) = state.items.get(index) else {
            return;
        };
        if let Target::Window { hwnd, pid } = item.target {
            if item.target.valid() {
                // Request a normal close: the application retains control over save prompts.
                if !crate::window_commands::request_close(hwnd as HWND, pid) {
                    eprintln!(
                        "[task-popup] close rejected hwnd={hwnd:#x} error={}",
                        GetLastError()
                    );
                }
            } else {
                refresh(state);
            }
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *pointer;
        match message {
            WM_PAINT => {
                paint(state);
                0
            }
            WM_ERASEBKGND => 1,
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => {
                state.pressed = None;
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_MOUSEMOVE => {
                let hovered = hit(
                    state,
                    l as u16 as i16 as i32,
                    (l >> 16) as u16 as i16 as i32,
                );
                if state.hover != hovered {
                    state.hover = hovered;
                    if let Some(Hit::Select(index) | Hit::Close(index)) = hovered
                        && index < state.items.len()
                    {
                        state.focused = index;
                    }
                    InvalidateRect(hwnd, null(), 0);
                }
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
                0
            }
            WM_MOUSELEAVE => {
                state.hover = None;
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_RBUTTONUP => {
                if let Some(Hit::Select(index)) = hit(
                    state,
                    l as u16 as i16 as i32,
                    (l >> 16) as u16 as i16 as i32,
                ) && let Target::Script { generation, index } = state.items[index].target
                {
                    state.selection = Some(Target::RenameScript { generation, index });
                    ShowWindow(hwnd, SW_HIDE);
                    PostMessageW(state.owner, SELECTED, 0, 0);
                }
                0
            }
            WM_LBUTTONDOWN => {
                state.pressed = hit(
                    state,
                    l as u16 as i16 as i32,
                    (l >> 16) as u16 as i16 as i32,
                );
                if state.pressed.is_some() {
                    SetCapture(hwnd);
                }
                0
            }
            WM_LBUTTONUP => {
                let pressed = state.pressed.take();
                if GetCapture() == hwnd {
                    ReleaseCapture();
                }
                let at = hit(
                    state,
                    l as u16 as i16 as i32,
                    (l >> 16) as u16 as i16 as i32,
                );
                if let Some(index) = pressed
                    && Some(index) == at
                {
                    match index {
                        Hit::Select(index) => choose(state, index),
                        Hit::Close(index) => close_window(state, index),
                    }
                }
                0
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                state.pressed = None;
                if message == WM_CANCELMODE && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                0
            }
            WM_MOUSEWHEEL => {
                let steps = (w >> 16) as u16 as i16 as i32 / 120;
                state.scroll = if steps > 0 {
                    state.scroll.saturating_sub(steps as usize * 3)
                } else {
                    (state.scroll + steps.unsigned_abs() as usize * 3)
                        .min(state.items.len().saturating_sub(state.rows))
                };
                state.hover = None;
                state.focused = state.focused.clamp(
                    state.scroll,
                    (state.scroll + state.rows)
                        .min(state.items.len())
                        .saturating_sub(1),
                );
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_KEYDOWN => {
                if w == VK_ESCAPE as usize {
                    ShowWindow(hwnd, SW_HIDE);
                    return 0;
                }
                if w == VK_RETURN as usize {
                    choose(state, state.focused);
                    return 0;
                }
                let count = state.items.len();
                if count > 0 {
                    state.focused = match w as u16 {
                        VK_UP => state.focused.saturating_sub(1),
                        VK_DOWN => (state.focused + 1).min(count - 1),
                        VK_HOME => 0,
                        VK_END => count - 1,
                        VK_PRIOR => state.focused.saturating_sub(state.rows),
                        VK_NEXT => (state.focused + state.rows).min(count - 1),
                        _ => return DefWindowProcW(hwnd, message, w, l),
                    };
                    state.scroll = visible_scroll(state.focused, state.scroll, state.rows, count);
                    state.hover = None;
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
unsafe fn paint(state: &mut State) {
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        BeginPaint(state.hwnd, &mut ps);
        let mut area: RECT = zeroed();
        GetClientRect(state.hwnd, &mut area);
        let Some(surface) = Surface::new(area.right, area.bottom) else {
            EndPaint(state.hwnd, &ps);
            return;
        };
        let dc = surface.dc;
        let p = theme::palette();
        let background = CreateSolidBrush(p.background);
        FillRect(dc, &area, background);
        ultra_bar_native_ui::fonts::delete_object(background);
        SetBkMode(dc, TRANSPARENT as i32);
        let old = SelectObject(dc, state.font);
        let rounded = |rect: RECT, color: u32| {
            let brush = CreateSolidBrush(color);
            let pen = CreatePen(PS_SOLID, unit(state, 1).max(1), p.border);
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, pen);
            RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                unit(state, 10),
                unit(state, 10),
            );
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            ultra_bar_native_ui::fonts::delete_object(pen);
            ultra_bar_native_ui::fonts::delete_object(brush);
        };
        let text = |value: &str, mut rect: RECT, color: u32, flags: u32| {
            SetTextColor(dc, color);
            DrawTextW(
                dc,
                wide(value).as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | flags,
            );
        };
        let mut tips = vec![];
        for (row, (index, item)) in state
            .items
            .iter()
            .enumerate()
            .skip(state.scroll)
            .take(state.rows)
            .enumerate()
        {
            let rect = RECT {
                left: unit(state, 8),
                top: unit(state, 8 + row as i32 * 44),
                right: area.right - unit(state, 8),
                bottom: unit(state, 48 + row as i32 * 44),
            };
            let close_available = item.target.close_available();
            if item.separator_before && row > 0 {
                let brush = CreateSolidBrush(p.border);
                let line = RECT {
                    left: rect.left + unit(state, 10),
                    right: rect.right - unit(state, 10),
                    top: rect.top - unit(state, 3),
                    bottom: rect.top - unit(state, 2),
                };
                FillRect(dc, &line, brush);
                ultra_bar_native_ui::fonts::delete_object(brush);
            }
            if item.active {
                rounded(rect, p.active_fill);
            } else if item.enabled
                && (matches!(state.hover,Some(Hit::Select(at)|Hit::Close(at)) if at==index)
                    || state.hover.is_none() && state.focused == index)
            {
                rounded(rect, p.hover);
            }
            let size = unit(state, 24);
            if let Some(glyph) = item.glyph {
                SelectObject(dc, state.icons);
                text(
                    glyph,
                    RECT {
                        left: rect.left + unit(state, 10),
                        right: rect.left + unit(state, 34),
                        ..rect
                    },
                    if item.enabled { p.text } else { p.disabled },
                    DT_CENTER,
                );
                SelectObject(dc, state.font);
            } else {
                DrawIconEx(
                    dc,
                    rect.left + unit(state, 10),
                    (rect.top + rect.bottom - size) / 2,
                    item.icon,
                    size,
                    size,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
            }
            text(
                &item.title,
                RECT {
                    left: rect.left + unit(state, 46),
                    right: rect.right
                        - unit(state, if close_available.is_some() { 42 } else { 12 }),
                    ..rect
                },
                if item.enabled { p.text } else { p.disabled },
                DT_END_ELLIPSIS,
            );
            if close_available.is_some() {
                let close = close_rect(state, rect);
                let hovered = state.hover == Some(Hit::Close(index));
                if hovered {
                    rounded(
                        close,
                        if state.pressed == Some(Hit::Close(index)) {
                            p.selection
                        } else {
                            p.field
                        },
                    );
                }
                SelectObject(dc, state.icons);
                text(
                    "\u{e711}",
                    close,
                    if close_available == Some(false) {
                        p.disabled
                    } else if hovered {
                        p.text
                    } else {
                        p.muted
                    },
                    DT_CENTER,
                );
                SelectObject(dc, state.font);
                tips.push(Tip {
                    bounds: (close.left, close.top, close.right, close.bottom),
                    text: "关闭此窗口".into(),
                });
            }
            tips.push(Tip {
                bounds: (
                    rect.left,
                    rect.top,
                    if close_available.is_some() {
                        close_rect(state, rect).left
                    } else {
                        rect.right
                    },
                    rect.bottom,
                ),
                text: item.tooltip.clone().unwrap_or_else(|| item.title.clone()),
            });
        }
        SelectObject(dc, old);
        if state.items.len() > state.rows {
            let top = unit(state, 10);
            let height = (area.bottom - unit(state, 20)).max(1);
            let thumb_height = (height * state.rows as i32 / state.items.len() as i32)
                .max(unit(state, 22))
                .min(height);
            let offset = (height - thumb_height) * state.scroll as i32
                / (state.items.len() - state.rows) as i32;
            rounded(
                RECT {
                    left: area.right - unit(state, 5),
                    right: area.right - unit(state, 3),
                    top: top + offset,
                    bottom: top + offset + thumb_height,
                },
                p.muted,
            );
        }
        if let Some(path) = state.export.take() {
            let _ = surface.save_bmp(&path);
        }
        surface.present(state.hwnd, state.glass);
        if let Some(tooltips) = &mut state.tips {
            tooltips.sync(tips);
        }
        EndPaint(state.hwnd, &ps);
    }
}

pub fn smoke(directory: &std::path::Path) -> anyhow::Result<()> {
    unsafe {
        anyhow::ensure!(directory.is_absolute(), "预览目录必须是绝对路径");
        std::fs::create_dir_all(directory)?;
        let icon = LoadIconW(null_mut(), IDI_APPLICATION);
        let items = (0..12)
            .map(|index| {
                Item::new(
                    Target::Task(index),
                    format!(
                        "项目文档 {} — 用于核对省略显示和完整标题提示的长窗口名称",
                        index + 1
                    ),
                    icon,
                    index == 2,
                )
            })
            .collect();
        let mut popup = Popup::new(
            null_mut(),
            "smoke".into(),
            "编辑器（12）".into(),
            items,
            RECT {
                left: 900,
                top: 1000,
                right: 948,
                bottom: 1040,
            },
            96,
        )
        .ok_or_else(|| anyhow::anyhow!("无法创建窗口列表"))?;
        for item in popup.state.items.iter_mut().take(11) {
            item.target = Target::Window {
                hwnd: popup.state.hwnd as usize,
                pid: std::process::id(),
            };
        }
        for (name, rgb) in [("blue", theme::DEFAULT_ACCENT), ("purple", 0xb96df0)] {
            theme::set_accent(rgb);
            popup.state.hover = Some(Hit::Close(1));
            let path = directory.join(format!("task-popup-{name}.bmp"));
            popup.state.export = Some(path.clone());
            paint(&mut popup.state);
            anyhow::ensure!(path.exists(), "窗口列表绘制失败");
        }
        SendMessageW(popup.state.hwnd, WM_KEYDOWN, VK_END as usize, 0);
        anyhow::ensure!(
            popup.state.focused == 11 && popup.state.scroll == 4,
            "长列表键盘滚动失败"
        );
        SendMessageW(popup.state.hwnd, WM_KEYDOWN, VK_RETURN as usize, 0);
        anyhow::ensure!(
            popup.take_selection() == Some(Target::Task(11)),
            "窗口列表选择失败"
        );
        std::fs::write(
            directory.join("result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"rendered":["task-popup-blue.bmp","task-popup-purple.bmp"],"keyboard_selection":"passed","global_input":false}),
            )?,
        )?;
        Ok(())
    }
}
pub fn preview_context(
    directory: &std::path::Path,
    snapshot: &crate::jump_list::Snapshot,
) -> anyhow::Result<()> {
    unsafe {
        let icon = LoadIconW(null_mut(), IDI_APPLICATION);
        let mut items: Vec<_> = snapshot
            .wire
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let source = std::path::Path::new(&entry.icon);
                let local = entry.icon.as_bytes().get(1) == Some(&b':')
                    && source.extension().is_some_and(|ext| {
                        ["exe", "dll", "ico"]
                            .iter()
                            .any(|kind| ext.eq_ignore_ascii_case(kind))
                    });
                let own = local
                    .then(|| visual::AppIcon::from_resource(&entry.icon, entry.icon_index, 96, 24));
                Item::new(
                    Target::Jump {
                        generation: 1,
                        index,
                    },
                    entry.title.clone(),
                    own.as_ref().map(|value| value.handle).unwrap_or(icon),
                    false,
                )
            })
            .collect();
        let mut open = Item::command(Target::Launch(1), "打开应用", "\u{e8a7}", true);
        open.separator_before = !items.is_empty();
        items.extend([
            open,
            Item::command(Target::Pin(1), "固定到任务栏", "\u{e718}", true),
            Item::command(Target::CloseGroup(1), "关闭窗口", "\u{e711}", true),
        ]);
        let mut popup = Popup::new(
            null_mut(),
            "context-preview".into(),
            "应用操作".into(),
            items,
            RECT {
                left: 800,
                top: 1000,
                right: 848,
                bottom: 1040,
            },
            96,
        )
        .ok_or_else(|| anyhow::anyhow!("Cannot create menu preview"))?;
        popup.state.export = Some(directory.join("context-menu.bmp"));
        paint(&mut popup.state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_layout_excludes_the_taskbar_and_has_no_footer_at_common_dpi_scales() {
        for dpi in [96, 144, 192] {
            for height in [240, 600, 1080] {
                for count in [1, 3, 100] {
                    let work = RECT {
                        left: -1920,
                        top: 0,
                        right: 0,
                        bottom: height,
                    };
                    let anchor = RECT {
                        left: -20,
                        top: height - 40,
                        right: 0,
                        bottom: height,
                    };
                    let (bounds, rows) = layout(anchor, work, dpi, count);
                    assert!(bounds.left >= work.left && bounds.right <= work.right);
                    assert!(bounds.top >= work.top);
                    assert!(
                        bounds.bottom <= anchor.top - 8 * dpi / 96,
                        "last item overlaps taskbar"
                    );
                    assert_eq!(
                        bounds.bottom - bounds.top,
                        (16 + 44 * rows as i32) * dpi / 96,
                        "footer must not reserve space"
                    );
                }
            }
        }
        let anchor = RECT {
            left: 40,
            top: 0,
            right: 88,
            bottom: 40,
        };
        let (bounds, _) = layout(
            anchor,
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            96,
            3,
        );
        assert!(bounds.top >= anchor.bottom + 8);
    }
    #[test]
    fn disabled_commands_cannot_be_selected() {
        unsafe {
            let mut popup = fixture(1);
            popup.state.items = vec![Item::command(
                Target::Pin(1),
                "固定到任务栏",
                "\u{e718}",
                false,
            )];
            choose(&mut popup.state, 0);
            assert!(popup.take_selection().is_none());
        }
    }
    unsafe fn fixture(count: usize) -> Popup {
        unsafe {
            let icon = LoadIconW(null_mut(), IDI_APPLICATION);
            let items = (0..count)
                .map(|index| Item::new(Target::Task(index), format!("窗口 {index}"), icon, false))
                .collect();
            Popup::new(
                null_mut(),
                "test".into(),
                "应用".into(),
                items,
                RECT {
                    left: 0,
                    top: 0,
                    right: 48,
                    bottom: 40,
                },
                96,
            )
            .unwrap()
        }
    }
    #[test]
    fn keyboard_selection_scrolls_into_view_and_escape_never_selects() {
        unsafe {
            let mut popup = fixture(24);
            SendMessageW(popup.state.hwnd, WM_KEYDOWN, VK_END as usize, 0);
            assert_eq!(popup.state.focused, 23);
            assert_eq!(popup.state.scroll, 16);
            SendMessageW(popup.state.hwnd, WM_KEYDOWN, VK_RETURN as usize, 0);
            assert_eq!(popup.take_selection(), Some(Target::Task(23)));
            SendMessageW(popup.state.hwnd, WM_KEYDOWN, VK_ESCAPE as usize, 0);
            assert!(popup.take_selection().is_none());
        }
    }
    #[test]
    fn cancelled_or_cross_row_click_does_not_select() {
        unsafe {
            let mut popup = fixture(3);
            let point = |x: u32, y: u32| (y << 16 | x) as isize;
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(24, 28));
            SendMessageW(popup.state.hwnd, WM_CANCELMODE, 0, 0);
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(24, 28));
            assert!(popup.take_selection().is_none());
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(24, 28));
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(24, 72));
            assert!(popup.take_selection().is_none());
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(24, 72));
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(24, 72));
            assert_eq!(popup.take_selection(), Some(Target::Task(1)));
        }
    }
    #[test]
    fn a_closed_window_cannot_be_selected() {
        unsafe {
            let module = GetModuleHandleW(null());
            let class = wide("UltraBar.TaskPopupFixture");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Test window").as_ptr(),
                WS_POPUP,
                0,
                0,
                40,
                40,
                null_mut(),
                null_mut(),
                module,
                null(),
            );
            assert!(!hwnd.is_null());
            let mut popup = fixture(1);
            popup.state.items[0].target = Target::Window {
                hwnd: hwnd as usize,
                pid: std::process::id(),
            };
            DestroyWindow(hwnd);
            choose(&mut popup.state, 0);
            assert!(popup.take_selection().is_none());
            assert!(popup.state.items.is_empty());
            UnregisterClassW(class.as_ptr(), module);
        }
    }
    #[test]
    fn row_close_targets_only_its_window_and_preserves_an_app_that_declines_to_close() {
        unsafe {
            unsafe extern "system" fn defer(
                hwnd: HWND,
                message: u32,
                w: WPARAM,
                l: LPARAM,
            ) -> LRESULT {
                unsafe {
                    if message == WM_CLOSE {
                        SetWindowLongPtrW(
                            hwnd,
                            GWLP_USERDATA,
                            GetWindowLongPtrW(hwnd, GWLP_USERDATA) + 1,
                        );
                        0
                    } else {
                        DefWindowProcW(hwnd, message, w, l)
                    }
                }
            }
            let module = GetModuleHandleW(null());
            let class = wide("UltraBar.TaskPopupCloseFixture");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(defer),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let create = || {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide("Close fixture").as_ptr(),
                    WS_POPUP | WS_SYSMENU,
                    0,
                    0,
                    40,
                    40,
                    null_mut(),
                    null_mut(),
                    module,
                    null(),
                )
            };
            let first = create();
            let second = create();
            assert!(!first.is_null() && !second.is_null());
            let mut popup = fixture(2);
            for (item, hwnd) in popup.state.items.iter_mut().zip([first, second]) {
                item.target = Target::Window {
                    hwnd: hwnd as usize,
                    pid: std::process::id(),
                };
            }
            let mut bounds: RECT = zeroed();
            GetClientRect(popup.state.hwnd, &mut bounds);
            assert_eq!(bounds.bottom, 104, "header must not reserve space");
            let point = |x: i32, y: i32| ((y as u32) << 16 | x as u32) as isize;
            let at = point(bounds.right - 28, 28);
            assert_eq!(
                hit(&popup.state, bounds.right - 28, 28),
                Some(Hit::Close(0))
            );
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, at);
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, at);
            assert!(
                popup.take_selection().is_none(),
                "close must not activate the window"
            );
            let mut message: MSG = zeroed();
            assert_ne!(
                PeekMessageW(&mut message, first, WM_SYSCOMMAND, WM_SYSCOMMAND, PM_REMOVE),
                0
            );
            DispatchMessageW(&message);
            assert_eq!(GetWindowLongPtrW(first, GWLP_USERDATA), 1);
            assert_eq!(GetWindowLongPtrW(second, GWLP_USERDATA), 0);
            assert_eq!(
                PeekMessageW(
                    &mut message,
                    second,
                    WM_SYSCOMMAND,
                    WM_SYSCOMMAND,
                    PM_REMOVE
                ),
                0
            );
            popup.refresh();
            assert_eq!(
                popup.state.items.len(),
                2,
                "a pending save prompt must retain its row"
            );
            DestroyWindow(first);
            popup.refresh();
            assert_eq!(popup.state.items.len(), 1);
            assert_eq!(
                popup.state.items[0].target,
                Target::Window {
                    hwnd: second as usize,
                    pid: std::process::id()
                }
            );
            DestroyWindow(second);
            popup.refresh();
            assert!(popup.state.items.is_empty());
            UnregisterClassW(class.as_ptr(), module);
        }
    }
}
