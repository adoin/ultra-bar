use super::{Click, Command, Snapshot};
use crate::visual::text::draw_text as DrawTextW;
use crate::visual::{self, Surface, Tip, Tooltips, wide};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{Arc, mpsc::SyncSender},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{
        Controls::{WM_MOUSEHOVER, WM_MOUSELEAVE},
        HiDpi::GetSystemMetricsForDpi,
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

#[derive(Clone)]
enum Action {
    Icon(String),
    Previous,
    Next,
}
struct Hit {
    rect: RECT,
    action: Action,
    label: String,
}
struct State {
    hover: super::Hover,
    cancelled_click: bool,
    context: Option<super::context_menu::Handoff>,
    hold_inactive: bool,
    drag: Option<(String, POINT, bool)>,
    ghost: Option<super::drag_image::Ghost>,
    drop_target: bool,
    dropped: Option<(String, POINT)>,
    hwnd: HWND,
    owner: HWND,
    sender: SyncSender<Command>,
    snapshot: Arc<Snapshot>,
    excluded: Vec<String>,
    dpi: i32,
    font: HFONT,
    glass: bool,
    page: usize,
    rows: usize,
    hits: Vec<Hit>,
    tips: Option<Tooltips>,
    anchor: RECT,
}
pub struct Popup {
    state: Box<State>,
}
impl Drop for Popup {
    fn drop(&mut self) {
        unsafe {
            if !self.state.hwnd.is_null() {
                DestroyWindow(self.state.hwnd);
            }
            if !self.state.font.is_null() {
                ultra_bar_native_ui::fonts::delete_object(self.state.font);
            }
        }
    }
}

impl Popup {
    pub unsafe fn context_click(&mut self, key: &str) {
        unsafe {
            if IsWindowVisible(self.state.hwnd) != 0 {
                arm_context(&mut self.state, key);
            }
        }
    }
    pub unsafe fn drag_feedback(&mut self, point: POINT) {
        unsafe {
            let active = self.contains(point);
            if active != self.state.drop_target {
                self.state.drop_target = active;
                InvalidateRect(self.state.hwnd, null(), 0);
            }
        }
    }
    pub unsafe fn end_drag(&mut self) {
        unsafe {
            self.state.drop_target = false;
            InvalidateRect(self.state.hwnd, null(), 0);
        }
    }
    pub unsafe fn hide(&mut self) {
        unsafe {
            PostMessageW(self.state.hwnd, WM_CANCELMODE, 0, 0);
            ShowWindow(self.state.hwnd, SW_HIDE);
        }
    }
    pub unsafe fn contains(&self, point: POINT) -> bool {
        unsafe {
            let mut rect: RECT = zeroed();
            IsWindowVisible(self.state.hwnd) != 0
                && GetWindowRect(self.state.hwnd, &mut rect) != 0
                && point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
        }
    }
    pub fn take_drop(&mut self) -> Option<(String, POINT)> {
        self.state.dropped.take()
    }
    pub unsafe fn open_for_drag(&mut self, dpi: i32) {
        unsafe {
            if IsWindowVisible(self.state.hwnd) == 0 {
                self.toggle_with_activation(dpi, false);
            }
        }
    }
    pub unsafe fn settings(&mut self) {
        unsafe {
            settings_menu(&mut self.state);
        }
    }
    pub unsafe fn new(owner: HWND, sender: SyncSender<Command>) -> Self {
        unsafe {
            let class = wide("UltraBar.TrayPopup");
            let instance = GetModuleHandleW(null());
            RegisterClassW(&WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..zeroed()
            });
            let mut state = Box::new(State {
                hover: super::Hover::default(),
                cancelled_click: false,
                context: None,
                hold_inactive: false,
                drag: None,
                ghost: None,
                drop_target: false,
                dropped: None,
                hwnd: null_mut(),
                owner,
                sender,
                snapshot: Arc::new(Snapshot::empty()),
                excluded: vec![],
                dpi: 96,
                font: null_mut(),
                glass: false,
                page: 0,
                rows: 6,
                hits: vec![],
                tips: None,
                anchor: zeroed(),
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.as_ptr(),
                wide("系统托盘").as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner,
                null_mut(),
                instance,
                (&mut *state as *mut State).cast(),
            );
            state.hwnd = hwnd;
            state.glass = visual::enable_glass(hwnd);
            state.tips = Some(Tooltips::new(hwnd));
            Self { state }
        }
    }
    pub unsafe fn update(&mut self, snapshot: Arc<Snapshot>, excluded: Vec<String>) {
        if Arc::ptr_eq(&self.state.snapshot, &snapshot) && self.state.excluded == excluded {
            return;
        }
        let previous: Vec<_> = indices(&self.state)
            .iter()
            .map(|i| self.state.snapshot.items[*i].key.clone())
            .collect();
        self.state.snapshot = snapshot;
        self.state.excluded = excluded;
        let relayout = previous
            != indices(&self.state)
                .iter()
                .map(|i| self.state.snapshot.items[*i].key.clone())
                .collect::<Vec<_>>();
        if relayout {
            self.state.hover.leave(&self.state.sender);
        }
        unsafe {
            if IsWindowVisible(self.state.hwnd) != 0 {
                if relayout {
                    position(&mut self.state);
                }
                InvalidateRect(self.state.hwnd, null(), 0);
            }
        }
    }
    pub unsafe fn toggle(&mut self, dpi: i32) {
        unsafe {
            self.toggle_with_activation(dpi, true);
        }
    }
    unsafe fn toggle_with_activation(&mut self, dpi: i32, activate: bool) {
        unsafe {
            if self.state.hwnd.is_null() {
                return;
            }
            KillTimer(self.state.hwnd, 11);
            if IsWindowVisible(self.state.hwnd) != 0 {
                ShowWindow(self.state.hwnd, SW_HIDE);
                return;
            }
            self.state.dpi = dpi;
            if !self.state.font.is_null() {
                ultra_bar_native_ui::fonts::delete_object(self.state.font);
            }
            self.state.font = ultra_bar_native_ui::fonts::create_font(
                -13 * dpi / 96,
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
                wide("Segoe UI").as_ptr(),
            );
            GetWindowRect(self.state.owner, &mut self.state.anchor);
            self.state.page = 0;
            position(&mut self.state);
            let _ = self.state.sender.try_send(Command::Refresh);
            ShowWindow(
                self.state.hwnd,
                if activate { SW_SHOW } else { SW_SHOWNOACTIVATE },
            );
            if activate {
                SetForegroundWindow(self.state.hwnd);
            }
            InvalidateRect(self.state.hwnd, null(), 0);
        }
    }
}

fn indices(state: &State) -> Vec<usize> {
    let mut output: Vec<_> = state
        .snapshot
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| !state.excluded.contains(&item.key))
        .map(|(index, _)| index)
        .collect();
    output.sort_by_key(|index| {
        state
            .snapshot
            .preferences
            .entries
            .iter()
            .position(|entry| entry.id == state.snapshot.items[*index].preference_key)
            .unwrap_or(usize::MAX)
    });
    output
}
unsafe fn position(state: &mut State) {
    unsafe {
        let unit = |n: i32| n * state.dpi / 96;
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromRect(&state.anchor, MONITOR_DEFAULTTONEAREST),
            &mut info,
        );
        let work = info.rcWork;
        let pitch = unit(30) + crate::settings::ITEM_GAP;
        state.rows = ((work.bottom - work.top - unit(48)) / pitch).clamp(1, 6) as usize;
        let available = indices(state).len();
        let capacity = state.rows * 8;
        state.page = state.page.min(available.saturating_sub(1) / capacity);
        let rows = available.clamp(1, capacity).div_ceil(8);
        let width = (available.clamp(1, 8) as i32 * pitch + unit(16))
            .max(unit(128))
            .min(work.right - work.left);
        let height =
            unit(16) + rows as i32 * pitch + if available > capacity { unit(28) } else { 0 };
        let x = (state.anchor.right - width - unit(8)).max(work.left);
        let y = (state.anchor.top - height - unit(8)).max(work.top);
        SetWindowPos(
            state.hwnd,
            HWND_TOPMOST,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        );
    }
}

unsafe fn arm_context(state: &mut State, key: &str) {
    unsafe {
        KillTimer(state.hwnd, 11);
        let (owner, pid) = state
            .snapshot
            .items
            .iter()
            .find(|item| item.key == key)
            .map(|item| item.endpoint.menu_owner())
            .unwrap_or((state.owner, std::process::id()));
        state.context = Some(super::context_menu::Handoff::new(owner, pid, state.hwnd));
        state.hold_inactive = true;
        SetTimer(state.hwnd, super::context_menu::TIMER, 60, None);
        ultra_bar_native_ui::diagnostics::event(
            "tray",
            "context.handoff",
            serde_json::json!({"pid":pid}),
        );
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if crate::visual::theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *ptr;
        match message {
            WM_PAINT => {
                paint(state);
                0
            }
            WM_ERASEBKGND => 1,
            WM_TIMER if w == super::HOVER_TIMER => {
                KillTimer(hwnd, super::HOVER_TIMER);
                let mut point: POINT = zeroed();
                if GetCursorPos(&mut point) != 0 {
                    ScreenToClient(hwnd, &mut point);
                    if state.hits.iter().any(|hit| {
                        point.x >= hit.rect.left
                            && point.x < hit.rect.right
                            && point.y >= hit.rect.top
                            && point.y < hit.rect.bottom
                            && matches!(&hit.action,Action::Icon(key)if state.hover.matches(key))
                    }) {
                        state.hover.open(&state.sender);
                    }
                }
                0
            }
            WM_TIMER if w == 11 => {
                KillTimer(hwnd, 11);
                if !state.hold_inactive && state.context.is_none() {
                    ShowWindow(hwnd, SW_HIDE);
                }
                0
            }
            WM_TIMER if w == super::context_menu::TIMER => {
                use super::context_menu::Decision;
                let decision = state.context.as_mut().map(|session| session.tick());
                match decision {
                    Some(Decision::Wait) => {}
                    Some(Decision::Resume) => {
                        KillTimer(hwnd, super::context_menu::TIMER);
                        state.context = None;
                        state.hold_inactive = GetForegroundWindow() != hwnd;
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        TrackMouseEvent(&mut track);
                    }
                    Some(Decision::Hide) => {
                        ultra_bar_native_ui::diagnostics::event(
                            "tray",
                            "context.closed",
                            serde_json::json!({"menu_observed":state.context.as_ref().is_some_and(|s|s.observed())}),
                        );
                        KillTimer(hwnd, super::context_menu::TIMER);
                        ShowWindow(hwnd, SW_HIDE);
                    }
                    None => {
                        KillTimer(hwnd, super::context_menu::TIMER);
                    }
                }
                0
            }
            WM_ACTIVATE if w & 0xffff != WA_INACTIVE as usize => {
                state.hold_inactive = false;
                0
            }
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize && state.drag.is_none() => {
                KillTimer(hwnd, 11);
                if !state.hold_inactive && state.context.is_none() {
                    ShowWindow(hwnd, SW_HIDE);
                }
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_KEYDOWN if w == 0x1b => {
                state.ghost = None;
                state.drag = None;
                if GetCapture() == hwnd {
                    ReleaseCapture();
                }
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_MOUSEWHEEL => {
                if (w >> 16) as u16 as i16 > 0 {
                    state.page = state.page.saturating_sub(1);
                } else {
                    state.page += 1;
                }
                position(state);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONDOWN => {
                state.context = None;
                state.hold_inactive = false;
                KillTimer(hwnd, super::context_menu::TIMER);
                state.hover.leave(&state.sender);
                state.cancelled_click = false;
                KillTimer(hwnd, 11);
                let point = POINT {
                    x: l as u16 as i16 as i32,
                    y: (l >> 16) as u16 as i16 as i32,
                };
                if let Some(key) = state
                    .hits
                    .iter()
                    .find(|hit| {
                        point.x >= hit.rect.left
                            && point.x < hit.rect.right
                            && point.y >= hit.rect.top
                            && point.y < hit.rect.bottom
                    })
                    .and_then(|hit| {
                        if let Action::Icon(key) = &hit.action {
                            Some(key.clone())
                        } else {
                            None
                        }
                    })
                {
                    state.drag = Some((key, point, false));
                    SetCapture(hwnd);
                }
                0
            }
            WM_MOUSEMOVE => {
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                let key = if state.drag.is_none() {
                    state
                        .hits
                        .iter()
                        .find(|hit| {
                            x >= hit.rect.left
                                && x < hit.rect.right
                                && y >= hit.rect.top
                                && y < hit.rect.bottom
                        })
                        .and_then(|hit| match &hit.action {
                            Action::Icon(key) => Some(key.clone()),
                            _ => None,
                        })
                } else {
                    None
                };
                let anchor = state
                    .hits
                    .iter()
                    .find(|hit| matches!(&hit.action, Action::Icon(k) if Some(k) == key.as_ref()));
                let (ax, ay) = anchor.map_or((x, y), |hit| {
                    (
                        (hit.rect.left + hit.rect.right) / 2,
                        (hit.rect.top + hit.rect.bottom) / 2,
                    )
                });
                state.hover.motion(hwnd, key, ax, ay, &state.sender);
                if let Some((_, start, active)) = &mut state.drag {
                    let x = l as u16 as i16 as i32;
                    let y = (l >> 16) as u16 as i16 as i32;
                    *active |= (x - start.x).abs()
                        >= GetSystemMetricsForDpi(SM_CXDRAG, state.dpi as u32).max(4)
                        || (y - start.y).abs()
                            >= GetSystemMetricsForDpi(SM_CYDRAG, state.dpi as u32).max(4);
                }
                if let Some((key, _, true)) = &state.drag {
                    if state.ghost.is_none()
                        && let Some(item) =
                            state.snapshot.items.iter().find(|item| item.key == *key)
                    {
                        state.ghost = super::drag_image::Ghost::new(item.icon.handle(), state.dpi);
                    }
                    if let Some(ghost) = &state.ghost {
                        ghost.follow();
                    }
                }
                0
            }
            WM_MOUSEHOVER => {
                state.hover.open(&state.sender);
                0
            }
            WM_MOUSELEAVE => {
                state.hover.leave(&state.sender);
                if state.hold_inactive && state.context.is_none() {
                    ShowWindow(hwnd, SW_HIDE);
                }
                0
            }
            WM_SHOWWINDOW if w == 0 => {
                state.context = None;
                state.hold_inactive = false;
                KillTimer(hwnd, super::context_menu::TIMER);
                state.drop_target = false;
                state.ghost = None;
                state.hover.leave(&state.sender);
                0
            }
            WM_DESTROY => {
                state.hover.leave(&state.sender);
                0
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                state.ghost = None;
                if state.drag.is_some() {
                    state.cancelled_click = true;
                }
                state.drag = None;
                if message == WM_CANCELMODE && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                0
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_LBUTTONDBLCLK => {
                if message == WM_RBUTTONUP
                    && state.drag.as_ref().is_some_and(|(_, _, active)| *active)
                {
                    state.cancelled_click = true;
                    state.drag = None;
                    state.ghost = None;
                    if GetCapture() == hwnd {
                        ReleaseCapture();
                    }
                    return 0;
                }
                state.ghost = None;
                state.hover.leave(&state.sender);
                if message == WM_LBUTTONUP && state.cancelled_click {
                    state.cancelled_click = false;
                    return 0;
                }
                if message == WM_LBUTTONUP
                    && let Some((key, _, active)) = state.drag.take()
                {
                    ReleaseCapture();
                    if active {
                        let mut point = POINT {
                            x: l as u16 as i16 as i32,
                            y: (l >> 16) as u16 as i16 as i32,
                        };
                        ClientToScreen(hwnd, &mut point);
                        state.dropped = Some((key, point));
                        PostMessageW(state.owner, crate::shell::TRAY_DROP, 0, 0);
                        ShowWindow(hwnd, SW_HIDE);
                        return 0;
                    }
                }
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                let action = state
                    .hits
                    .iter()
                    .find(|hit| {
                        x >= hit.rect.left
                            && x < hit.rect.right
                            && y >= hit.rect.top
                            && y < hit.rect.bottom
                    })
                    .map(|hit| hit.action.clone());
                if let Some(action) = action {
                    match action {
                        Action::Icon(key) => {
                            if message == WM_LBUTTONUP {
                                SetTimer(hwnd, 11, GetDoubleClickTime() + 100, None);
                            } else if message == WM_RBUTTONUP {
                                arm_context(state, &key);
                            } else {
                                KillTimer(hwnd, 11);
                                ShowWindow(hwnd, SW_HIDE);
                            }
                            let click = if message == WM_RBUTTONUP {
                                Click::Right
                            } else if message == WM_LBUTTONDBLCLK {
                                Click::Double
                            } else {
                                Click::Left
                            };
                            let _ = state.sender.try_send(Command::Click(key, click));
                        }
                        Action::Previous if message == WM_LBUTTONUP => {
                            state.page = state.page.saturating_sub(1);
                            position(state);
                            InvalidateRect(hwnd, null(), 0);
                        }
                        Action::Next if message == WM_LBUTTONUP => {
                            state.page += 1;
                            position(state);
                            InvalidateRect(hwnd, null(), 0);
                        }
                        _ => {}
                    }
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
        let old = SelectObject(dc, state.font);
        let brush = CreateSolidBrush(visual::theme::palette().background);
        FillRect(dc, &area, brush);
        ultra_bar_native_ui::fonts::delete_object(brush);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, crate::visual::theme::palette().text);
        let unit = |n: i32| n * state.dpi / 96;
        let text = |value: &str, mut rect: RECT| {
            DrawTextW(
                dc,
                wide(value).as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        };
        let items = indices(state);
        let capacity = state.rows * 8;
        state.hits.clear();
        for (position, index) in items
            .iter()
            .skip(state.page * capacity)
            .take(capacity)
            .enumerate()
        {
            let item = &state.snapshot.items[*index];
            let pitch = unit(30) + crate::settings::ITEM_GAP;
            let x = unit(8) + (position % 8) as i32 * pitch;
            let y = unit(8) + (position / 8) as i32 * pitch;
            let size = unit(16);
            DrawIconEx(
                dc,
                x + (unit(30) - size) / 2,
                y + (unit(30) - size) / 2,
                item.icon.handle(),
                size,
                size,
                0,
                null_mut(),
                DI_NORMAL,
            );
            state.hits.push(Hit {
                rect: RECT {
                    left: x,
                    top: y,
                    right: x + unit(30),
                    bottom: y + unit(30),
                },
                action: Action::Icon(item.key.clone()),
                label: item.title.clone(),
            });
        }
        if items.is_empty() {
            text(
                "当前没有可用的收纳图标",
                RECT {
                    left: unit(12),
                    top: unit(8),
                    right: area.right - unit(12),
                    bottom: area.bottom - unit(8),
                },
            );
        }
        let mut button = |left: i32, width: i32, label: &str, action: Action| {
            let rect = RECT {
                left: unit(left),
                top: area.bottom - unit(30),
                right: unit(left + width),
                bottom: area.bottom - unit(8),
            };
            let brush = CreateSolidBrush(visual::theme::palette().hover);
            FillRect(dc, &rect, brush);
            ultra_bar_native_ui::fonts::delete_object(brush);
            text(
                label,
                RECT {
                    left: rect.left + unit(5),
                    ..rect
                },
            );
            state.hits.push(Hit {
                rect,
                action,
                label: label.into(),
            });
        };
        if items.len() > capacity {
            let right = area.right * 96 / state.dpi;
            button(right - 54, 18, "‹", Action::Previous);
            button(right - 30, 18, "›", Action::Next);
        }
        SelectObject(dc, old);
        if state.drop_target {
            let brush = CreateSolidBrush(visual::theme::palette().active_border);
            FrameRect(
                surface.dc,
                &RECT {
                    left: 2,
                    top: 2,
                    right: area.right - 2,
                    bottom: area.bottom - 2,
                },
                brush,
            );
            ultra_bar_native_ui::fonts::delete_object(brush);
        }
        surface.present(state.hwnd, state.glass);
        if let Some(tips) = &mut state.tips {
            tips.sync(
                state
                    .hits
                    .iter()
                    .map(|hit| Tip {
                        bounds: (hit.rect.left, hit.rect.top, hit.rect.right, hit.rect.bottom),
                        text: hit.label.clone(),
                    })
                    .collect(),
            );
        }
        EndPaint(state.hwnd, &ps);
    }
}

unsafe fn settings_menu(state: &mut State) {
    unsafe {
        let mut entries = state.snapshot.preferences.entries.clone();
        for item in &state.snapshot.items {
            if !entries.iter().any(|entry| entry.id == item.preference_key) {
                entries.push(super::store::Preference {
                    id: item.preference_key.clone(),
                    executable: item.executable.clone(),
                    uid: item.uid,
                    title: item.title.clone(),
                    collapsed: item.collapsed,
                });
            }
        }
        let menu = CreatePopupMenu();
        if entries.is_empty() {
            AppendMenuW(
                menu,
                MF_STRING | MF_GRAYED,
                0,
                wide("暂无托盘设置，请先导入").as_ptr(),
            );
        }
        for (index, entry) in entries.iter().enumerate() {
            let child = CreatePopupMenu();
            AppendMenuW(
                child,
                MF_STRING | if !entry.collapsed { MF_CHECKED } else { 0 },
                1000 + index * 2,
                wide("显示在任务栏").as_ptr(),
            );
            AppendMenuW(
                child,
                MF_STRING | if entry.collapsed { MF_CHECKED } else { 0 },
                1001 + index * 2,
                wide("收纳到托盘").as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_POPUP,
                child as usize,
                wide(&entry.title.replace('&', "&&")).as_ptr(),
            );
        }
        let mut point: POINT = zeroed();
        GetCursorPos(&mut point);
        let menu_owner = if IsWindowVisible(state.hwnd) != 0 {
            state.hwnd
        } else {
            state.owner
        };
        SetForegroundWindow(menu_owner);
        let selected = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            0,
            menu_owner,
            null(),
        );
        DestroyMenu(menu);
        if selected >= 1000 {
            let index = (selected as usize - 1000) / 2;
            if let Some(entry) = entries.get(index) {
                let _ = state.sender.try_send(Command::SetCollapsed(
                    entry.id.clone(),
                    (selected - 1000) % 2 == 1,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn dragging_and_cancelling_own_popup_never_relay_a_tray_click() {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = wide("UltraBar.TrayDragTest");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let owner = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                wide("Tray drag test").as_ptr(),
                WS_POPUP,
                -32000,
                -32000,
                100,
                40,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            assert!(!owner.is_null());
            let (sender, receiver) = mpsc::sync_channel(8);
            let mut popup = Popup::new(owner, sender);
            popup.state.hits.push(Hit {
                rect: RECT {
                    left: 0,
                    top: 0,
                    right: 40,
                    bottom: 40,
                },
                action: Action::Icon("test-icon".into()),
                label: "Test icon".into(),
            });
            let point = |x: u16, y: u16| ((y as u32) << 16 | x as u32) as isize;
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(10, 10));
            SendMessageW(popup.state.hwnd, WM_MOUSEMOVE, 0, point(80, 10));
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(80, 10));
            assert!(
                receiver.try_recv().is_err(),
                "drag must not open the application"
            );
            let (key, destination) = popup.take_drop().expect("expected a drop");
            assert_eq!(key, "test-icon");
            let mut expected = POINT { x: 80, y: 10 };
            ClientToScreen(popup.state.hwnd, &mut expected);
            assert_eq!((destination.x, destination.y), (expected.x, expected.y));
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(10, 10));
            SendMessageW(popup.state.hwnd, WM_MOUSEMOVE, 0, point(80, 10));
            SendMessageW(popup.state.hwnd, WM_CANCELMODE, 0, 0);
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(10, 10));
            assert!(popup.take_drop().is_none());
            assert!(
                receiver.try_recv().is_err(),
                "cancelled drag must not click"
            );
            SendMessageW(popup.state.hwnd, WM_LBUTTONDOWN, 0, point(10, 10));
            SendMessageW(popup.state.hwnd, WM_LBUTTONUP, 0, point(10, 10));
            assert!(
                matches!(receiver.try_recv(),Ok(Command::Click(key,Click::Left)) if key=="test-icon")
            );
            SetWindowPos(
                popup.state.hwnd,
                HWND_TOP,
                -32000,
                -32000,
                80,
                80,
                SWP_NOACTIVATE,
            );
            ShowWindow(popup.state.hwnd, SW_SHOWNOACTIVATE);
            popup.state.hits = vec![Hit {
                rect: RECT {
                    left: 0,
                    top: 0,
                    right: 40,
                    bottom: 40,
                },
                action: Action::Icon("test-icon".into()),
                label: "Test icon".into(),
            }];
            SendMessageW(popup.state.hwnd, WM_RBUTTONUP, 0, point(10, 10));
            assert!(
                matches!(receiver.try_recv(),Ok(Command::Click(key,Click::Right)) if key=="test-icon")
            );
            assert_ne!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "right click must retain the flyout"
            );
            SendMessageW(popup.state.hwnd, WM_TIMER, 11, 0);
            SendMessageW(popup.state.hwnd, WM_ACTIVATE, WA_INACTIVE as usize, 0);
            assert_ne!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "menu focus or a queued left-click timer closed the flyout"
            );
            let menu_class = wide("UltraBar.TrayMenuTest.Popup");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                lpszClassName: menu_class.as_ptr(),
                ..zeroed()
            });
            let menu = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                menu_class.as_ptr(),
                wide("menu").as_ptr(),
                WS_POPUP | WS_VISIBLE,
                -32000,
                -32000,
                80,
                80,
                owner,
                null_mut(),
                instance,
                null(),
            );
            assert!(!menu.is_null());
            SendMessageW(
                popup.state.hwnd,
                WM_TIMER,
                super::super::context_menu::TIMER,
                0,
            );
            assert_ne!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "native popup handoff must retain the flyout"
            );
            assert!(popup.state.context.as_ref().unwrap().observed());
            ShowWindow(menu, SW_HIDE);
            popup.state.context.as_mut().unwrap().expire_for_test();
            SendMessageW(
                popup.state.hwnd,
                WM_TIMER,
                super::super::context_menu::TIMER,
                0,
            );
            assert_eq!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "ended context session must dismiss outside the flyout"
            );
            assert!(popup.state.context.is_none());
            DestroyWindow(menu);
            UnregisterClassW(menu_class.as_ptr(), instance);
            ShowWindow(popup.state.hwnd, SW_SHOWNOACTIVATE);
            SendMessageW(
                popup.state.hwnd,
                WM_TIMER,
                super::super::context_menu::TIMER,
                0,
            );
            assert_ne!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "stale context timer closed a reopened flyout"
            );
            SendMessageW(popup.state.hwnd, WM_ACTIVATE, WA_INACTIVE as usize, 0);
            assert_eq!(
                IsWindowVisible(popup.state.hwnd),
                0,
                "normal outside dismissal must remain functional"
            );
            drop(popup);
            DestroyWindow(owner);
            UnregisterClassW(class.as_ptr(), instance);
        }
    }
}
