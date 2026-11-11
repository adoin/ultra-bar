//! Event-driven glass panel shared by service plugins. Backend work never runs on the UI thread.
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
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
pub enum Command {
    Host(HostEvent),
    Query(String),
    Refresh,
    SettingsSaved(Vec<String>),
}
#[derive(Clone, Default)]
pub struct Row {
    pub title: String,
    pub detail: String,
    pub path: Option<String>,
}
#[derive(Clone)]
pub struct Panel {
    hwnd: isize,
    output: SyncSender<(Vec<Row>, String)>,
    settings: SyncSender<Vec<ultra_bar_native_ui::form::Field>>,
}
impl Panel {
    pub fn settings(&self, fields: Vec<ultra_bar_native_ui::form::Field>) {
        let _ = self.settings.try_send(fields);
        unsafe {
            PostMessageW(self.hwnd as _, WM_APP + 5, 0, 0);
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
    scroll: usize,
    search: bool,
}
pub fn run(
    title: &str,
    search: bool,
    backend: impl FnOnce(Panel, Receiver<Command>) + Send + 'static,
) -> anyhow::Result<()> {
    theme::init_from_env();
    let (input, commands) = mpsc::sync_channel(32);
    let (output, receive) = mpsc::sync_channel(4);
    let (settings, settings_rx) = mpsc::sync_channel(2);
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
            scroll: 0,
            search,
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
        if search {
            state.edit = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | ES_AUTOHSCROLL as u32,
                22 * dpi / 96,
                48 * dpi / 96,
                476 * dpi / 96,
                30 * dpi / 96,
                hwnd,
                1usize as _,
                module,
                null(),
            );
            SendMessageW(state.edit, WM_SETFONT, state.font as usize, 1);
            SendMessageW(
                state.edit,
                0x1501,
                0,
                wide("搜索文件或文件夹…").as_ptr() as isize,
            );
            SetWindowSubclass(state.edit, Some(edit_proc), 1, 0);
        }
        let panel = Panel {
            hwnd: hwnd as isize,
            output,
            settings,
        };
        std::thread::spawn(move || backend(panel, commands));
        let target = hwnd as isize;
        std::thread::spawn(move || {
            let mut reader = BufReader::new(std::io::stdin());
            let mut initialized = false;
            while let Ok(Some(line)) = ultra_bar_plugin_sdk::read_line(&mut reader) {
                let Ok(event) = serde_json::from_str::<HostEvent>(&line) else {
                    break;
                };
                if !initialized {
                    if !matches!(event, HostEvent::Initialize { protocol: 1, .. }) {
                        break;
                    }
                    initialized = true;
                } else if matches!(event, HostEvent::Initialize { .. }) {
                    break;
                }
                match &event {
                    HostEvent::Show => {
                        PostMessageW(target as _, WM_APP + 2, 0, 0);
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
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    w: usize,
    l: isize,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN && matches!(w as u16, VK_RETURN | VK_ESCAPE | VK_UP | VK_DOWN) {
            return SendMessageW(GetParent(hwnd), msg, w, l);
        }
        DefSubclassProc(hwnd, msg, w, l)
    }
}
unsafe fn open(state: &State, parent: bool) {
    if let Some(path) = state.rows.get(state.selected).and_then(|r| r.path.as_ref()) {
        let _ = send(&PluginEvent::Open {
            path: path.clone(),
            select_parent: parent,
        });
    }
}
unsafe fn sync_tips(s: &mut State) {
    unsafe {
        let unit = |n| n * s.dpi / 96;
        let mut tips = vec![
            Tip {
                bounds: (unit(435), 0, unit(475), unit(40)),
                text: "刷新".into(),
            },
            Tip {
                bounds: (unit(475), 0, unit(520), unit(40)),
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
                    unit(508),
                    unit(top + i as i32 * 62 + 58),
                ),
                text: row
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("{}\n{}", row.title, row.detail)),
            });
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
            WM_NCHITTEST => {
                let y = ((l as u32 >> 16) as i16) as i32;
                let mut pt = POINT {
                    x: l as i16 as i32,
                    y,
                };
                ScreenToClient(hwnd, &mut pt);
                if pt.y < unit(40) && pt.x < unit(395) {
                    HTCAPTION as _
                } else {
                    HTCLIENT as _
                }
            }
            WM_APP => 0,
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
                    s.rows = rows;
                    s.status = status;
                    s.selected = s.selected.min(s.rows.len().saturating_sub(1));
                    s.scroll = s.scroll.min(s.selected);
                }
                if !s.search {
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
                        area.right - unit(530)
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
                        ShowWindow(hwnd, SW_HIDE);
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
                    _ => {}
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_MOUSEWHEEL => {
                let delta = (w >> 16) as i16;
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
                if y < 40 && x > 475 {
                    ShowWindow(hwnd, SW_HIDE);
                } else if y < 40 && x > 435 {
                    let _ = s.input.try_send(Command::Refresh);
                } else if y < 40 && x > 395 && !s.settings.is_empty() {
                    if let Some(values) =
                        ultra_bar_native_ui::form::edit(hwnd, "搜索设置", s.settings.clone())
                    {
                        let _ = s.input.try_send(Command::SettingsSaved(values));
                    }
                } else {
                    let top = if s.search { 94 } else { 54 };
                    if y >= top && y < top + 310 {
                        s.selected = (s.scroll + ((y - top) / 62) as usize)
                            .min(s.rows.len().saturating_sub(1));
                        if msg == WM_LBUTTONDBLCLK || x > 465 {
                            open(s, x > 465);
                        }
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
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
                        text(&row.title, 22, y + 3, 440, 26, palette.text);
                        text(&row.detail, 22, y + 29, 440, 23, palette.muted);
                        if row.path.is_some() {
                            SelectObject(dc, s.icons);
                            text("\u{e838}", 478, y + 12, 24, 28, palette.muted);
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
                    // Constant alpha keeps native EDIT/caret rendering; per-pixel layered updates
                    // would omit child controls from the composed panel.
                    BitBlt(ps.hdc, 0, 0, area.right, area.bottom, dc, 0, 0, SRCCOPY);
                    if let Some(directory) = std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR") {
                        if !s.edit.is_null() {
                            let saved = SaveDC(dc);
                            SetViewportOrgEx(dc, unit(22), unit(48), null_mut());
                            SendMessageW(
                                s.edit,
                                WM_PRINTCLIENT,
                                dc as usize,
                                (PRF_CLIENT | PRF_ERASEBKGND) as isize,
                            );
                            RestoreDC(dc, saved);
                        }
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
