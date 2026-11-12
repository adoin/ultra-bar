//! Event-driven glass panel shared by service plugins. Backend work never runs on the UI thread.
pub mod statistics;
use std::{
    io::BufReader,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, SyncSender},
};
use ultra_bar_native_ui::text::draw_text as DrawTextW;
use ultra_bar_native_ui::{Surface, Tip, Tooltips, enable_glass, theme, wide};
pub use ultra_bar_plugin_sdk::{HostEvent, Page, PluginEvent, send};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
pub enum Command {
    Editor(ultra_bar_plugin_sdk::editors::EditorEvent),
    Host(HostEvent),
    Query(String),
    Refresh,
    SettingsSaved(Vec<String>),
    Ignored,
}
#[derive(Clone, Default)]
pub struct Row {
    pub title: String,
    pub detail: String,
    pub path: Option<String>,
}
#[derive(Clone)]
pub struct Panel {
    pending_settings: std::sync::Arc<std::sync::atomic::AtomicU64>,
    data: std::sync::Arc<std::sync::OnceLock<std::path::PathBuf>>,
    owner: isize,
    statistics: SyncSender<statistics::Dataset>,
    hwnd: isize,
    output: SyncSender<(Vec<Row>, String)>,
    settings: SyncSender<Vec<ultra_bar_native_ui::form::Field>>,
}
impl Panel {
    pub fn editor(&self, operation: ultra_bar_plugin_sdk::editors::Operation) {
        let data = Box::into_raw(Box::new(operation));
        unsafe {
            if PostMessageW(self.hwnd as HWND, WM_APP + 19, 0, data as isize) == 0 {
                drop(Box::from_raw(data));
            }
        }
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd as _) != 0 }
    }
    pub fn statistics(&self, data: statistics::Dataset) {
        let _ = self.statistics.try_send(data);
        unsafe {
            PostMessageW(self.hwnd as _, WM_APP + 6, 0, 0);
        }
    }
    pub fn settings(&self, fields: Vec<ultra_bar_native_ui::form::Field>) {
        if let Some(data) = self.data.get() {
            let ack = self
                .pending_settings
                .swap(0, std::sync::atomic::Ordering::SeqCst);
            if let Err(error) = ultra_bar_native_ui::settings_bridge::publish_ack(
                data,
                self.owner,
                fields.clone(),
                String::new(),
                (ack != 0).then_some(ack),
            ) {
                eprintln!("[settings] {error}");
            }
        }
        let _ = send(&PluginEvent::Capabilities {
            settings: !fields.is_empty(),
        });
        let _ = self.settings.try_send(fields);
        unsafe {
            PostMessageW(self.hwnd as _, WM_APP + 5, 0, 0);
        }
    }
    pub fn settings_error(&self, error: impl Into<String>) {
        if let Some(data) = self.data.get() {
            let ack = self
                .pending_settings
                .swap(0, std::sync::atomic::Ordering::SeqCst);
            let _ = ultra_bar_native_ui::settings_bridge::error_ack(
                data,
                self.owner,
                error.into(),
                (ack != 0).then_some(ack),
            );
        }
    }
    pub fn settings_command(&self, command: Command) -> Command {
        if let Command::Host(HostEvent::SettingsValues { revision, values }) = command {
            self.pending_settings
                .store(revision, std::sync::atomic::Ordering::SeqCst);
            if self
                .data
                .get()
                .and_then(|p| ultra_bar_native_ui::settings_bridge::load(p).ok())
                .is_some_and(|s| s.revision == revision)
                && values.len() <= 32
                && values.iter().all(|v| v.len() <= 65536)
            {
                Command::SettingsSaved(values)
            } else {
                self.settings_error("设置已在其他位置更改，请重新载入后再保存");
                Command::Ignored
            }
        } else {
            command
        }
    }
    pub fn update(&self, rows: Vec<Row>, status: impl Into<String>) {
        let _ = self.output.try_send((rows, status.into()));
        unsafe {
            PostMessageW(self.hwnd as _, WM_APP + 1, 0, 0);
        }
    }
}
struct State {
    popover: ultra_bar_native_ui::popover::Popover,
    dashboard: Option<statistics::View>,
    statistics_rx: Receiver<statistics::Dataset>,
    settings: Vec<ultra_bar_native_ui::form::Field>,
    settings_rx: Receiver<Vec<ultra_bar_native_ui::form::Field>>,
    tips: Option<Tooltips>,
    edit: HWND,
    font: HFONT,
    icons: HFONT,
    brush: HBRUSH,
    glass: bool,
    dpi: i32,
    input: SyncSender<Command>,
    receive: Receiver<(Vec<Row>, String)>,
    rows: Vec<Row>,
    status: String,
    selected: usize,
    context: Option<(i32, i32)>,
    scroll: usize,
    search: bool,
    host: HWND,
    plugin_id: String,
}
pub fn run(
    title: &str,
    search: bool,
    backend: impl FnOnce(Panel, Receiver<Command>) + Send + 'static,
) -> anyhow::Result<()> {
    let _logs = ultra_bar_native_ui::diagnostics::init(
        if search {
            "search-service"
        } else {
            "service-ui"
        },
        env!("CARGO_PKG_VERSION"),
    )?;
    theme::init_from_env();
    let (input, commands) = mpsc::sync_channel(32);
    let (output, receive) = mpsc::sync_channel(4);
    let (settings, settings_rx) = mpsc::sync_channel(2);
    let (statistics, statistics_rx) = mpsc::sync_channel(2);
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(null());
        let class = wide(&format!("UltraBar.Plugin.{}", std::process::id()));
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let owner = std::env::args()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|a| a[0] == "--owner")
            .and_then(|a| a[1].parse::<usize>().ok())
            .unwrap_or(0) as HWND;
        let dpi = if owner.is_null() {
            96
        } else {
            GetDpiForWindow(owner).max(96) as i32
        };
        let font = |name: &str, height: i32| {
            CreateFontW(
                -height * dpi / 96,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                0,
                0,
                CLEARTYPE_NATURAL_QUALITY as _,
                0,
                wide(name).as_ptr(),
            )
        };
        let mut state = Box::new(State {
            popover: Default::default(),
            dashboard: None,
            statistics_rx,
            settings: vec![],
            settings_rx,
            tips: None,
            edit: null_mut(),
            font: font("Segoe UI", 14),
            icons: font("Segoe MDL2 Assets", 17),
            brush: CreateSolidBrush(theme::palette().field),
            glass: false,
            dpi,
            input: input.clone(),
            receive,
            rows: vec![],
            status: String::new(),
            selected: 0,
            context: None,
            scroll: 0,
            search,
            host: null_mut(),
            plugin_id: String::new(),
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            520 * dpi / 96,
            450 * dpi / 96,
            owner,
            null_mut(),
            module,
            (&mut *state as *mut State).cast(),
        );
        anyhow::ensure!(!hwnd.is_null(), "Cannot create plugin window");
        state.glass = enable_glass(hwnd);
        state.tips = Some(Tooltips::new(hwnd));
        sync_tips(&mut state);
        SetLayeredWindowAttributes(hwnd, 0, if state.glass { 238 } else { 255 }, LWA_ALPHA);
        SetWindowRgn(
            hwnd,
            CreateRoundRectRgn(
                0,
                0,
                520 * dpi / 96,
                450 * dpi / 96,
                18 * dpi / 96,
                18 * dpi / 96,
            ),
            1,
        );
        let panel = Panel {
            pending_settings: Default::default(),
            data: Default::default(),
            owner: owner as isize,
            statistics,
            hwnd: hwnd as isize,
            output,
            settings,
        };
        let settings_data = panel.data.clone();
        std::thread::spawn(move || backend(panel, commands));
        let target = hwnd as isize;
        std::thread::spawn(move || {
            let mut reader = BufReader::new(std::io::stdin());
            let mut initialized = false;
            while let Ok(Some(line)) = ultra_bar_plugin_sdk::read_line(&mut reader) {
                let Ok(event) = serde_json::from_str::<HostEvent>(&line) else {
                    ultra_bar_native_ui::diagnostics::event(
                        "plugins",
                        "protocol.invalid",
                        serde_json::json!({"line_bytes":line.len()}),
                    );
                    break;
                };
                if !initialized {
                    if !matches!(event, HostEvent::Initialize { protocol: 1, .. }) {
                        break;
                    }
                    initialized = true;
                    if let HostEvent::Initialize { host, id, .. } = &event {
                        let data = Box::new((*host, id.clone()));
                        PostMessageW(target as _, WM_APP + 18, 0, Box::into_raw(data) as isize);
                    }
                    if let HostEvent::Initialize { id, protocol, .. } = &event {
                        ultra_bar_native_ui::diagnostics::event(
                            "plugins",
                            "initialized",
                            serde_json::json!({"id":id,"protocol":protocol}),
                        );
                    }
                    if let HostEvent::Initialize { data_dir, .. } = &event {
                        let _ = settings_data.set(data_dir.into());
                    }
                } else if matches!(event, HostEvent::Initialize { .. }) {
                    break;
                }
                match &event {
                    HostEvent::Settings => {
                        PostMessageW(target as _, WM_APP + 7, 0, 0);
                        continue;
                    }
                    HostEvent::Show => {
                        PostMessageW(target as _, WM_APP + 2, 0, 0);
                        continue;
                    }
                    HostEvent::Shutdown => break,
                    HostEvent::Theme { accent } => {
                        if let Some(rgb) = theme::parse_hex(accent) {
                            theme::set_accent(rgb);
                            PostMessageW(target as _, WM_APP + 3, 0, 0);
                        }
                    }
                    _ => {}
                }
                if input.send(Command::Host(event)).is_err() {
                    break;
                }
            }
            let _ = input.try_send(Command::Host(HostEvent::Shutdown));
            PostMessageW(target as _, WM_APP + 4, 0, 0);
        });
        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DeleteObject(state.font);
        DeleteObject(state.icons);
        DeleteObject(state.brush);
    }
    Ok(())
}
unsafe fn open(state: &State, parent: bool) {
    if let Some(path) = state.rows.get(state.selected).and_then(|r| r.path.as_ref()) {
        let _ = send(&PluginEvent::Open {
            path: path.clone(),
            select_parent: parent,
        });
    }
}
unsafe fn copy_path(state: &mut State, hwnd: HWND) {
    unsafe {
        use windows_sys::Win32::System::{DataExchange::*, Memory::*};
        let Some(path) = state.rows.get(state.selected).and_then(|r| r.path.as_ref()) else {
            return;
        };
        let value = wide(path);
        let memory = GlobalAlloc(GMEM_MOVEABLE, value.len() * 2);
        if memory.is_null() {
            state.status = "无法分配剪贴板内存".into();
            return;
        }
        let target = GlobalLock(memory) as *mut u16;
        if target.is_null() {
            GlobalFree(memory);
            state.status = "无法写入剪贴板".into();
            return;
        }
        std::ptr::copy_nonoverlapping(value.as_ptr(), target, value.len());
        GlobalUnlock(memory);
        if OpenClipboard(hwnd) == 0 {
            GlobalFree(memory);
            state.status = "剪贴板正忙，请重试".into();
            return;
        }
        if EmptyClipboard() != 0 && !SetClipboardData(13, memory).is_null() {
            state.status = "已复制完整路径".into();
        } else {
            GlobalFree(memory);
            state.status = "无法写入剪贴板".into();
        }
        CloseClipboard();
    }
}
unsafe fn result_action(s: &mut State, hwnd: HWND, index: usize) {
    unsafe {
        match index {
            0 => open(s, false),
            1 => open(s, true),
            _ => copy_path(s, hwnd),
        };
        InvalidateRect(hwnd, null(), 0);
    }
}
unsafe fn sync_tips(s: &mut State) {
    unsafe {
        let unit = |n| n * s.dpi / 96;
        let mut tips = vec![
            Tip {
                bounds: (
                    unit(if s.dashboard.is_some() { 595 } else { 435 }),
                    0,
                    unit(if s.dashboard.is_some() { 635 } else { 475 }),
                    unit(40),
                ),
                text: "刷新".into(),
            },
            Tip {
                bounds: (
                    unit(if s.dashboard.is_some() { 635 } else { 475 }),
                    0,
                    unit(if s.dashboard.is_some() { 680 } else { 520 }),
                    unit(40),
                ),
                text: "收起".into(),
            },
        ];
        if !s.settings.is_empty() {
            tips.push(Tip {
                bounds: (unit(395), 0, unit(435), unit(40)),
                text: "设置".into(),
            });
        }
        let top = if s.search { 94 } else { 54 };
        for (i, row) in s.rows.iter().skip(s.scroll).take(5).enumerate() {
            tips.push(Tip {
                bounds: (
                    unit(12),
                    unit(top + i as i32 * 62),
                    unit(390),
                    unit(top + i as i32 * 62 + 58),
                ),
                text: row
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("{}\n{}", row.title, row.detail)),
            });
            if row.path.is_some() {
                for (n, title) in [
                    "打开 · Enter",
                    "打开所在位置 · Ctrl+Enter",
                    "复制完整路径 · Ctrl+Shift+C",
                ]
                .iter()
                .enumerate()
                {
                    tips.push(Tip {
                        bounds: (
                            unit(394 + n as i32 * 36),
                            unit(top + i as i32 * 62),
                            unit(430 + n as i32 * 36),
                            unit(top + i as i32 * 62 + 58),
                        ),
                        text: (*title).into(),
                    });
                }
            }
        }
        if let Some(tooltips) = s.tips.as_mut() {
            tooltips.sync(tips);
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if p.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let s = &mut *p;
        let unit = |n| n * s.dpi / 96;
        match msg {
            m if m == WM_APP + 19 => {
                let operation = Box::from_raw(l as *mut ultra_bar_plugin_sdk::editors::Operation);
                let _ = ultra_bar_native_ui::editor_bridge::request(
                    s.host,
                    hwnd,
                    &s.plugin_id,
                    *operation,
                );
                0
            }
            m if m == WM_APP + 18 => {
                let data = Box::from_raw(l as *mut (usize, String));
                s.host = data.0 as HWND;
                s.plugin_id = data.1.clone();
                if s.search {
                    let field = ultra_bar_plugin_sdk::editors::Field::new(
                        "query",
                        "",
                        ultra_bar_plugin_sdk::editors::Kind::Search,
                        "",
                    );
                    let _ = ultra_bar_native_ui::editor_bridge::request(
                        s.host,
                        hwnd,
                        &s.plugin_id,
                        ultra_bar_plugin_sdk::editors::Operation::Mount {
                            session: 1,
                            field,
                            x: unit(22),
                            y: unit(48),
                            width: unit(476),
                            height: unit(30),
                        },
                    );
                }
                0
            }
            WM_COPYDATA if l != 0 && w as HWND == s.host => {
                let packet =
                    &*(l as *const windows_sys::Win32::System::DataExchange::COPYDATASTRUCT);
                if packet.dwData == ultra_bar_plugin_sdk::editors::EVENT
                    && packet.cbData <= 65536
                    && !packet.lpData.is_null()
                {
                    let bytes = std::slice::from_raw_parts(
                        packet.lpData as *const u8,
                        packet.cbData as usize,
                    );
                    if let Ok(event) =
                        serde_json::from_slice::<ultra_bar_plugin_sdk::editors::EditorEvent>(bytes)
                    {
                        if s.search && event.session == 1 {
                            if event.action == "opened" {
                                s.edit = event.input as HWND;
                                if IsWindowVisible(hwnd) != 0 {
                                    SetFocus(s.edit);
                                }
                            }
                            if event.action == "closed" {
                                s.edit = null_mut();
                            }
                            if event.action == "change"
                                && let Some(query) = event.values.get("query")
                            {
                                let _ = s.input.try_send(Command::Query(query.clone()));
                            }
                            return 1;
                        }
                        return i32::from(s.input.try_send(Command::Editor(event)).is_ok())
                            as isize;
                    }
                }
                0
            }
            m if m == WM_APP + 7 => {
                let _ = ultra_bar_native_ui::editor_bridge::request(
                    s.host,
                    hwnd,
                    &s.plugin_id,
                    ultra_bar_plugin_sdk::editors::Operation::Settings,
                );
                0
            }
            WM_NCHITTEST => {
                let y = ((l as u32 >> 16) as i16) as i32;
                let mut pt = POINT {
                    x: l as i16 as i32,
                    y,
                };
                ScreenToClient(hwnd, &mut pt);
                if s.dashboard
                    .as_ref()
                    .map(|view| view.draggable(pt.x * 96 / s.dpi, pt.y * 96 / s.dpi))
                    .unwrap_or(pt.y < unit(40) && pt.x < unit(395))
                {
                    HTCAPTION as _
                } else {
                    HTCLIENT as _
                }
            }
            WM_APP => 0,
            m if m == WM_APP + 6 => {
                if s.dashboard.is_none() {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
                }
                while let Ok(data) = s.statistics_rx.try_recv() {
                    s.dashboard
                        .get_or_insert_with(Default::default)
                        .replace(data);
                }
                let mut work: RECT = zeroed();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
                let height = unit(620).min(work.bottom - work.top - unit(20));
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    unit(680),
                    height,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                SetWindowRgn(
                    hwnd,
                    CreateRoundRectRgn(0, 0, unit(680), height, unit(18), unit(18)),
                    1,
                );
                InvalidateRect(hwnd, null(), 0);
                0
            }
            m if m == WM_APP + 5 => {
                while let Ok(fields) = s.settings_rx.try_recv() {
                    s.settings = fields;
                }
                sync_tips(s);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            m if m == WM_APP + 1 => {
                while let Ok((rows, status)) = s.receive.try_recv() {
                    s.context = None;
                    s.rows = rows;
                    s.status = status;
                    s.selected = s.selected.min(s.rows.len().saturating_sub(1));
                    s.scroll = s.scroll.min(s.selected);
                }
                if !s.search && s.dashboard.is_none() {
                    let height = unit(110 + s.rows.len().clamp(1, 5) as i32 * 62);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        0,
                        0,
                        unit(520),
                        height,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    SetWindowRgn(
                        hwnd,
                        CreateRoundRectRgn(0, 0, unit(520), height, unit(18), unit(18)),
                        1,
                    );
                }
                sync_tips(s);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            m if m == WM_APP + 2 => {
                if !s.popover.toggle(hwnd) {
                    return 0;
                }
                if std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR").is_some() {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        -32000,
                        -32000,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    return 0;
                }
                let mut area: RECT = zeroed();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut area as *mut RECT).cast(), 0);
                let mut bounds: RECT = zeroed();
                GetWindowRect(hwnd, &mut bounds);
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    if s.search {
                        area.left + unit(10)
                    } else {
                        area.right - (bounds.right - bounds.left) - unit(10)
                    },
                    area.bottom - (bounds.bottom - bounds.top) - unit(10),
                    0,
                    0,
                    SWP_NOSIZE | SWP_SHOWWINDOW,
                );
                SetForegroundWindow(hwnd);
                if s.search {
                    SetFocus(s.edit);
                }
                let _ = s.input.try_send(Command::Host(HostEvent::Show));
                0
            }
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => {
                s.context = None;
                s.popover.outside(hwnd);
                0
            }
            m if m == WM_APP + 3 => {
                DeleteObject(s.brush);
                s.brush = CreateSolidBrush(theme::palette().field);
                InvalidateRect(hwnd, null(), 1);
                0
            }
            m if m == WM_APP + 4 => {
                DestroyWindow(hwnd);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == EN_CHANGE => {
                SetTimer(hwnd, 1, 180, None);
                0
            }
            WM_TIMER => {
                KillTimer(hwnd, 1);
                let mut text = [0u16; 2048];
                let n = GetWindowTextW(s.edit, text.as_mut_ptr(), 2048);
                s.selected = 0;
                s.scroll = 0;
                let _ = s.input.try_send(Command::Query(String::from_utf16_lossy(
                    &text[..n.max(0) as usize],
                )));
                0
            }
            WM_KEYDOWN => {
                match w as u16 {
                    VK_ESCAPE => {
                        if s.context.take().is_none() {
                            ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    VK_RETURN => open(s, GetKeyState(VK_CONTROL as i32) < 0),
                    VK_DOWN => {
                        s.selected = (s.selected + 1).min(s.rows.len().saturating_sub(1));
                        if s.selected >= s.scroll + 5 {
                            s.scroll = s.selected - 4;
                        }
                    }
                    VK_UP => {
                        s.selected = s.selected.saturating_sub(1);
                        s.scroll = s.scroll.min(s.selected);
                    }
                    key if key == b'C' as u16
                        && GetKeyState(VK_CONTROL as i32) < 0
                        && GetKeyState(VK_SHIFT as i32) < 0 =>
                    {
                        copy_path(s, hwnd)
                    }
                    _ => {}
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_MOUSEWHEEL => {
                let delta = (w >> 16) as i16;
                if let Some(view) = s.dashboard.as_mut() {
                    view.wheel(delta < 0);
                    InvalidateRect(hwnd, null(), 0);
                    return 0;
                }
                if delta < 0 {
                    s.scroll = (s.scroll + 1).min(s.rows.len().saturating_sub(5));
                } else {
                    s.scroll = s.scroll.saturating_sub(1);
                }
                sync_tips(s);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                let x = l as i16 as i32 * 96 / s.dpi;
                let y = (l >> 16) as i16 as i32 * 96 / s.dpi;
                if let Some((left, top)) = s.context.take() {
                    if x >= left && x < left + 190 && y >= top && y < top + 120 {
                        result_action(s, hwnd, ((y - top) / 40) as usize);
                    }
                    InvalidateRect(hwnd, null(), 0);
                    return 0;
                }
                let width = if s.dashboard.is_some() { 680 } else { 520 };
                let header = if s.dashboard.is_some() { 54 } else { 40 };
                if y < header && x > width - 45 {
                    ShowWindow(hwnd, SW_HIDE);
                } else if y < header && x > width - 85 {
                    let _ = s.input.try_send(Command::Refresh);
                } else if let Some(view) = s.dashboard.as_mut() {
                    if view.click(x, y) {
                        InvalidateRect(hwnd, null(), 0);
                    }
                } else if y < 40 && x > 395 && !s.settings.is_empty() {
                    let _ = ultra_bar_native_ui::editor_bridge::request(
                        s.host,
                        hwnd,
                        &s.plugin_id,
                        ultra_bar_plugin_sdk::editors::Operation::Settings,
                    );
                } else {
                    let top = if s.search { 94 } else { 54 };
                    if y >= top && y < top + 310 {
                        s.selected = (s.scroll + ((y - top) / 62) as usize)
                            .min(s.rows.len().saturating_sub(1));
                        if (394..502).contains(&x) {
                            result_action(s, hwnd, ((x - 394) / 36) as usize);
                        } else if msg == WM_LBUTTONDBLCLK {
                            open(s, false);
                        }
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            WM_RBUTTONUP if s.search => {
                let x = l as i16 as i32 * 96 / s.dpi;
                let y = (l >> 16) as i16 as i32 * 96 / s.dpi;
                if (94..404).contains(&y) {
                    let selected = s.scroll + ((y - 94) / 62) as usize;
                    if s.rows.get(selected).is_some_and(|r| r.path.is_some()) {
                        s.selected = selected;
                        s.context = Some((x.clamp(12, 318), y.clamp(94, 292)));
                    }
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_CTLCOLOREDIT => {
                SetBkColor(w as _, theme::palette().field);
                SetTextColor(w as _, theme::palette().text);
                s.brush as _
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps = zeroed();
                BeginPaint(hwnd, &mut ps);
                let mut area: RECT = zeroed();
                GetClientRect(hwnd, &mut area);
                if let Some(view) = s.dashboard.as_mut() {
                    if let Some(surface) = Surface::new(area.right, area.bottom) {
                        view.paint(&surface, s.font, s.icons, s.dpi, &area);
                        surface.present(hwnd, true);
                        if let Some(dir) = std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR") {
                            let _ = surface.save_bmp(
                                &std::path::PathBuf::from(dir).join("agent-statistics.bmp"),
                            );
                        }
                    }
                    EndPaint(hwnd, &ps);
                    return 0;
                }
                if let Some(surface) = Surface::new(area.right, area.bottom) {
                    let dc = surface.dc;
                    let palette = theme::palette();
                    let brush = CreateSolidBrush(palette.background);
                    FillRect(dc, &area, brush);
                    DeleteObject(brush);
                    SelectObject(dc, s.font);
                    SetBkMode(dc, TRANSPARENT as _);
                    let text = |value: &str, x, y, width, height, color| {
                        SetTextColor(dc, color);
                        let mut rect = RECT {
                            left: unit(x),
                            top: unit(y),
                            right: unit(x + width),
                            bottom: unit(y + height),
                        };
                        DrawTextW(
                            dc,
                            wide(value).as_ptr(),
                            -1,
                            &mut rect,
                            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );
                    };
                    let mut title = [0u16; 128];
                    let n = GetWindowTextW(hwnd, title.as_mut_ptr(), 128);
                    text(
                        &String::from_utf16_lossy(&title[..n as usize]),
                        20,
                        8,
                        380,
                        30,
                        palette.text,
                    );
                    SelectObject(dc, s.icons);
                    if !s.settings.is_empty() {
                        text("\u{e713}", 405, 8, 24, 30, palette.muted);
                    }
                    text("\u{e72c}", 445, 8, 24, 30, palette.muted);
                    text("\u{e711}", 484, 8, 24, 30, palette.muted);
                    SelectObject(dc, s.font);
                    let top = if s.search { 94 } else { 54 };
                    for (i, row) in s.rows.iter().enumerate().skip(s.scroll).take(5) {
                        let y = top + (i - s.scroll) as i32 * 62;
                        if i == s.selected && row.path.is_some() {
                            let brush = CreateSolidBrush(palette.selection);
                            let old = SelectObject(dc, brush);
                            let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                            RoundRect(
                                dc,
                                unit(12),
                                unit(y),
                                unit(508),
                                unit(y + 58),
                                unit(12),
                                unit(12),
                            );
                            SelectObject(dc, old);
                            SelectObject(dc, pen);
                            DeleteObject(brush);
                        }
                        text(&row.title, 22, y + 3, 360, 26, palette.text);
                        text(&row.detail, 22, y + 29, 360, 23, palette.muted);
                        if row.path.is_some() {
                            SelectObject(dc, s.icons);
                            for (n, glyph) in
                                ["\u{e8a7}", "\u{e8b7}", "\u{e8c8}"].iter().enumerate()
                            {
                                text(glyph, 400 + n as i32 * 36, y + 12, 24, 28, palette.muted);
                            }
                            SelectObject(dc, s.font);
                        }
                    }
                    text(
                        &s.status,
                        20,
                        area.bottom * 96 / s.dpi - 39,
                        480,
                        28,
                        palette.muted,
                    );
                    if let Some((x, y)) = s.context {
                        let b = SelectObject(dc, theme::field_brush());
                        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                        RoundRect(
                            dc,
                            unit(x),
                            unit(y),
                            unit(x + 190),
                            unit(y + 120),
                            unit(12),
                            unit(12),
                        );
                        SelectObject(dc, b);
                        SelectObject(dc, pen);
                        for (i, label) in
                            ["打开", "打开所在位置", "复制完整路径"].iter().enumerate()
                        {
                            text(label, x + 16, y + i as i32 * 40, 164, 40, palette.text);
                        }
                    }
                    // Constant alpha keeps native EDIT/caret rendering; per-pixel layered updates
                    // would omit child controls from the composed panel.
                    surface.blit_opaque(ps.hdc);
                    if let Some(directory) = std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR") {
                        let name = if s.search {
                            "everything-panel.bmp"
                        } else {
                            "agent-usage-panel.bmp"
                        };
                        let _ = surface.save_bmp(&std::path::PathBuf::from(directory).join(name));
                    }
                }
                EndPaint(hwnd, &ps);
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
