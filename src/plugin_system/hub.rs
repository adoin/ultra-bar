use super::{github, model::*, package};
use crate::visual::text::draw_text as DrawTextW;
use crate::visual::{Surface, Tip, Tooltips, wide};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const UPDATE: u32 = WM_APP + 91;
#[derive(Clone, Deserialize)]
struct Development {
    id: String,
    name: String,
    #[serde(default)]
    version: String,
}
#[derive(Clone)]
struct Snapshot {
    sources: Sources,
    active: String,
    entries: Vec<Candidate>,
    registry: Registry,
    development: Vec<Development>,
    status: String,
    busy: bool,
}
enum Request {
    Select(String),
    Refresh,
    Add(String),
    Remove,
    Install(Box<Candidate>, bool),
    Local(PathBuf),
}
#[derive(Clone)]
enum Action {
    Tab(String),
    Add,
    Remove,
    Refresh,
    Local,
    Install(Box<Candidate>, bool),
    Register,
    Previous,
    Next,
    Close,
}
struct Hit {
    rect: RECT,
    action: Action,
    tip: String,
}
struct State {
    hwnd: HWND,
    owner: HWND,
    font: HFONT,
    icons: HFONT,
    dpi: i32,
    glass: bool,
    sender: SyncSender<Request>,
    updates: Receiver<Arc<Snapshot>>,
    snapshot: Arc<Snapshot>,
    hits: Vec<Hit>,
    tips: Option<Tooltips>,
    scroll: usize,
    tabs: usize,
    export: Option<PathBuf>,
}
pub struct Hub {
    state: Box<State>,
}
impl Drop for Hub {
    fn drop(&mut self) {
        unsafe {
            if !self.state.hwnd.is_null() {
                DestroyWindow(self.state.hwnd);
            }
            if !self.state.font.is_null() {
                ultra_bar_native_ui::fonts::delete_object(self.state.font);
            }
            if !self.state.icons.is_null() {
                ultra_bar_native_ui::fonts::delete_object(self.state.icons);
            }
        }
    }
}
fn development(root: &std::path::Path) -> Vec<Development> {
    std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path().join("plugin.json");
            let bytes = std::fs::read(path).ok()?;
            if bytes.len() > 65536 {
                return None;
            }
            serde_json::from_slice(&bytes).ok()
        })
        .collect()
}
fn worker(
    window: usize,
    root: PathBuf,
    requests: Receiver<Request>,
    updates: SyncSender<Arc<Snapshot>>,
) {
    let Ok(paths) = Paths::user() else {
        return;
    };
    let loaded_sources = paths.sources();
    let sources_writable = loaded_sources.is_ok();
    let mut snapshot = Snapshot {
        sources: loaded_sources.unwrap_or_default(),
        active: OFFICIAL_ID.into(),
        entries: vec![],
        registry: paths.registry().unwrap_or_default(),
        development: development(&root),
        status: String::new(),
        busy: false,
    };
    let publish = |snapshot: &Snapshot| -> bool {
        if updates.send(Arc::new(snapshot.clone())).is_err() {
            return false;
        }
        unsafe {
            PostMessageW(window as _, UPDATE, 0, 0);
        }
        true
    };
    if !publish(&snapshot) {
        return;
    }
    while let Ok(request) = requests.recv() {
        let force_refresh = matches!(request, Request::Refresh);
        snapshot.busy = true;
        snapshot.status = "正在处理…".into();
        if let Request::Select(id) = &request {
            snapshot.active = id.clone();
            snapshot.entries.clear();
        }
        if !publish(&snapshot) {
            return;
        }
        let result = (|| -> Result<()> {
            match request {
                Request::Select(_) | Request::Refresh => {}
                Request::Add(input) => {
                    ensure!(sources_writable, "仓库配置无法读取，未覆盖原文件");
                    let source = github::resolve(&input)?;
                    ensure!(
                        !snapshot
                            .sources
                            .repositories
                            .iter()
                            .any(|existing| existing.id == source.id),
                        "此仓库已添加"
                    );
                    let mut sources = snapshot.sources.clone();
                    sources.repositories.push(source.clone());
                    paths.save("repositories.json", &sources)?;
                    snapshot.sources = sources;
                    snapshot.active = source.id;
                }
                Request::Remove => {
                    ultra_bar_native_ui::diagnostics::event(
                        "plugins",
                        "repository.remove_requested",
                        serde_json::json!({"source_id":snapshot.active}),
                    );
                    ensure!(sources_writable, "仓库配置无法读取，未覆盖原文件");
                    ensure!(
                        snapshot.active != OFFICIAL_ID && snapshot.active != "installed",
                        "不能移除官方仓库"
                    );
                    let mut sources = snapshot.sources.clone();
                    sources
                        .repositories
                        .retain(|source| source.id != snapshot.active);
                    paths.save("repositories.json", &sources)?;
                    snapshot.sources = sources;
                    snapshot.active = OFFICIAL_ID.into();
                }
                Request::Install(candidate, force) => {
                    let path = github::download(&candidate, &paths)?;
                    let result = package::install_at(
                        &paths,
                        &path,
                        force,
                        Some(candidate.source.clone()),
                        Some(&candidate.entry),
                    )?;
                    notify_inventory();
                    snapshot.status = result.summary();
                }
                Request::Local(path) => {
                    let result = package::install_file(&path, true, None)?;
                    snapshot.status = result.summary();
                }
            }
            snapshot.registry = paths.registry()?;
            snapshot.development = development(&root);
            if snapshot.active == "installed" {
                snapshot.entries.clear();
                for source in &snapshot.sources.repositories {
                    if !snapshot.registry.plugins.iter().any(|plugin| {
                        plugin
                            .source
                            .as_ref()
                            .is_none_or(|installed_source| installed_source.id == source.id)
                    }) {
                        continue;
                    }
                    match github::catalog(&paths, source, force_refresh) {
                        Ok(entries) => {
                            for candidate in entries {
                                let eligible = snapshot
                                    .registry
                                    .plugins
                                    .iter()
                                    .any(|plugin| eligible_update(plugin, &candidate));
                                if !eligible {
                                    continue;
                                }
                                if let Some(index) = snapshot
                                    .entries
                                    .iter()
                                    .position(|entry| entry.entry.id == candidate.entry.id)
                                {
                                    if precedence(
                                        &candidate.entry.version,
                                        &snapshot.entries[index].entry.version,
                                    )?
                                    .is_gt()
                                    {
                                        snapshot.entries[index] = candidate;
                                    }
                                } else {
                                    snapshot.entries.push(candidate);
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("[plugin-hub] {}: {error:#}", source.id);
                            snapshot.status = "部分仓库暂时无法检查更新".into();
                        }
                    }
                }
            } else if let Some(source) = snapshot
                .sources
                .repositories
                .iter()
                .find(|source| source.id == snapshot.active)
            {
                snapshot.entries = github::catalog(&paths, source, force_refresh)?;
            }
            if snapshot.status == "正在处理…" {
                snapshot.status = if snapshot.active == "installed" {
                    format!(
                        "{} 个发布包，{} 个可用更新",
                        snapshot.registry.plugins.len(),
                        snapshot.entries.len()
                    )
                } else if snapshot.entries.is_empty() {
                    "此仓库尚无兼容的已发布插件".into()
                } else {
                    format!("{} 个插件", snapshot.entries.len())
                };
            }
            Ok(())
        })();
        if let Err(error) = result {
            snapshot.status = format!("{error:#}");
        }
        snapshot.busy = false;
        if !publish(&snapshot) {
            return;
        }
    }
}
impl Hub {
    pub fn busy(&self) -> bool {
        self.state.snapshot.busy
    }
    pub fn inventory_changed(&self) {
        unsafe {
            if IsWindowVisible(self.state.hwnd) != 0 {
                let _ = self
                    .state
                    .sender
                    .try_send(Request::Select(self.state.snapshot.active.clone()));
            }
        }
    }
    pub unsafe fn new(owner: HWND, development_root: PathBuf) -> Self {
        unsafe {
            let (sender, requests) = mpsc::sync_channel(16);
            let (outgoing, updates) = mpsc::sync_channel(1);
            let snapshot = Arc::new(Snapshot {
                sources: Sources::default(),
                active: OFFICIAL_ID.into(),
                entries: vec![],
                registry: Registry::default(),
                development: vec![],
                status: String::new(),
                busy: false,
            });
            let mut state = Box::new(State {
                hwnd: null_mut(),
                owner,
                font: null_mut(),
                icons: null_mut(),
                dpi: 96,
                glass: false,
                sender,
                updates,
                snapshot,
                hits: vec![],
                tips: None,
                scroll: 0,
                tabs: 0,
                export: None,
            });
            let module = GetModuleHandleW(null());
            let class = wide("UltraBar.PluginHub");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..zeroed()
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.as_ptr(),
                wide("插件中心").as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner,
                null_mut(),
                module,
                (&mut *state as *mut State).cast(),
            );
            state.hwnd = hwnd;
            state.glass = crate::visual::enable_glass(hwnd);
            state.tips = Some(Tooltips::new(hwnd));
            let window = hwnd as usize;
            std::thread::spawn(move || worker(window, development_root, requests, outgoing));
            Self { state }
        }
    }
    pub unsafe fn show(&mut self) {
        unsafe {
            let dpi = GetDpiForWindow(self.state.owner).max(96) as i32;
            self.state.dpi = dpi;
            for font in [&mut self.state.font, &mut self.state.icons] {
                if !font.is_null() {
                    ultra_bar_native_ui::fonts::delete_object(*font);
                }
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
            self.state.icons = ultra_bar_native_ui::fonts::create_font(
                -18 * dpi / 96,
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
                wide("Segoe MDL2 Assets").as_ptr(),
            );
            let mut info: MONITORINFO = zeroed();
            info.cbSize = size_of::<MONITORINFO>() as _;
            GetMonitorInfoW(
                MonitorFromWindow(self.state.owner, MONITOR_DEFAULTTOPRIMARY),
                &mut info,
            );
            let width = (840 * dpi / 96).min(info.rcWork.right - info.rcWork.left - 16);
            let height = (560 * dpi / 96).min(info.rcWork.bottom - info.rcWork.top - 16);
            SetWindowPos(
                self.state.hwnd,
                HWND_TOPMOST,
                (info.rcWork.left + info.rcWork.right - width) / 2,
                (info.rcWork.top + info.rcWork.bottom - height) / 2,
                width,
                height,
                SWP_NOACTIVATE,
            );
            SetWindowRgn(
                self.state.hwnd,
                CreateRoundRectRgn(0, 0, width, height, 20 * dpi / 96, 20 * dpi / 96),
                1,
            );
            ShowWindow(self.state.hwnd, SW_SHOW);
            SetForegroundWindow(self.state.hwnd);
            InvalidateRect(self.state.hwnd, null(), 0);
            let _ = self
                .state
                .sender
                .try_send(Request::Select(self.state.snapshot.active.clone()));
        }
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
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
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
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => {
                if IsWindowEnabled(hwnd) != 0 {
                    ShowWindow(hwnd, SW_HIDE);
                }
                0
            }
            UPDATE => {
                while let Ok(snapshot) = state.updates.try_recv() {
                    if snapshot.active != state.snapshot.active {
                        state.scroll = 0;
                        state.tabs = snapshot
                            .sources
                            .repositories
                            .iter()
                            .position(|source| source.id == snapshot.active)
                            .unwrap_or(0);
                    }
                    state.snapshot = snapshot;
                }
                if IsWindowVisible(hwnd) != 0 {
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_KEYDOWN if w == 0x1b => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_MOUSEWHEEL => {
                if (w >> 16) as u16 as i16 > 0 {
                    state.scroll = state.scroll.saturating_sub(3);
                } else {
                    state.scroll += 3;
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
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
                        Action::Close => {
                            ShowWindow(hwnd, SW_HIDE);
                        }
                        Action::Previous => {
                            state.tabs = state.tabs.saturating_sub(1);
                        }
                        Action::Next => {
                            state.tabs = (state.tabs + 1)
                                .min(state.snapshot.sources.repositories.len().saturating_sub(1));
                        }
                        Action::Tab(id) => {
                            let _ = state.sender.try_send(Request::Select(id));
                        }
                        Action::Refresh => {
                            let _ = state.sender.try_send(Request::Refresh);
                        }
                        Action::Add => {
                            if let Some(input) = crate::input_dialog::edit(
                                hwnd,
                                "添加插件仓库",
                                "GitHub 仓库 URL 或 owner/repository",
                                "",
                            ) {
                                let _ = state.sender.try_send(Request::Add(input));
                            }
                        }
                        Action::Remove => {
                            let _ = state.sender.try_send(Request::Remove);
                        }
                        Action::Local => {
                            if let Some(path) = file(hwnd) {
                                let _ = state.sender.try_send(Request::Local(path));
                            }
                        }
                        Action::Install(candidate, normal) => {
                            let force = if message == WM_RBUTTONUP {
                                let menu = CreatePopupMenu();
                                AppendMenuW(menu, MF_STRING, 1, wide("强制安装此版本").as_ptr());
                                let mut point: POINT = zeroed();
                                GetCursorPos(&mut point);
                                let selected = TrackPopupMenu(
                                    menu,
                                    TPM_RETURNCMD | TPM_NONOTIFY,
                                    point.x,
                                    point.y,
                                    0,
                                    hwnd,
                                    null(),
                                );
                                DestroyMenu(menu);
                                if selected != 1 {
                                    return 0;
                                }
                                true
                            } else {
                                if !normal {
                                    return 0;
                                }
                                false
                            };
                            let _ = state.sender.try_send(Request::Install(candidate, force));
                        }
                        Action::Register => {
                            let menu = CreatePopupMenu();
                            AppendMenuW(menu, MF_STRING, 1, wide("关联 .ubplugin 文件").as_ptr());
                            AppendMenuW(menu, MF_STRING, 2, wide("取消本程序文件关联").as_ptr());
                            let mut point: POINT = zeroed();
                            GetCursorPos(&mut point);
                            let selected = TrackPopupMenu(
                                menu,
                                TPM_RETURNCMD | TPM_NONOTIFY,
                                point.x,
                                point.y,
                                0,
                                hwnd,
                                null(),
                            );
                            DestroyMenu(menu);
                            let result = if selected == 1 {
                                super::association::register()
                            } else if selected == 2 {
                                super::association::unregister()
                            } else {
                                Ok(())
                            };
                            if let Err(error) = result {
                                MessageBoxW(
                                    hwnd,
                                    wide(&format!("{error:#}")).as_ptr(),
                                    wide("插件文件关联").as_ptr(),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                        }
                    }
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}

unsafe fn fill(dc: HDC, rect: &RECT, color: u32, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old = SelectObject(dc, brush);
        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, pen);
        SelectObject(dc, old);
        ultra_bar_native_ui::fonts::delete_object(brush);
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
        let brush = CreateSolidBrush(crate::visual::theme::palette().background);
        FillRect(dc, &area, brush);
        ultra_bar_native_ui::fonts::delete_object(brush);
        SetBkMode(dc, TRANSPARENT as _);
        let unit = |n: i32| n * state.dpi / 96;
        let text = |value: &str, mut rect: RECT, color: u32| {
            SetTextColor(dc, color);
            DrawTextW(
                dc,
                wide(value).as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        };
        state.hits.clear();
        text(
            "插件中心",
            RECT {
                left: unit(16),
                top: unit(8),
                right: unit(400),
                bottom: unit(40),
            },
            0x00ffffff,
        );
        let mut icon = |glyph: &str, rect: RECT, tip: &str, action: Action| {
            fill(dc, &rect, crate::visual::theme::palette().hover, unit(10));
            let font = SelectObject(dc, state.icons);
            SetTextColor(dc, crate::visual::theme::palette().text);
            let mut target = rect;
            DrawTextW(
                dc,
                wide(glyph).as_ptr(),
                -1,
                &mut target,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(dc, font);
            state.hits.push(Hit {
                rect,
                action,
                tip: tip.into(),
            });
        };
        icon(
            "\u{e8bb}",
            RECT {
                left: area.right - unit(48),
                top: unit(8),
                right: area.right - unit(16),
                bottom: unit(40),
            },
            "关闭",
            Action::Close,
        );
        icon(
            "\u{e713}",
            RECT {
                left: area.right - unit(88),
                top: unit(8),
                right: area.right - unit(56),
                bottom: unit(40),
            },
            "文件关联",
            Action::Register,
        );
        if !state.snapshot.busy {
            for (glyph, offset, tip, action) in [
                ("\u{e710}", 144, "添加仓库", Action::Add),
                (
                    "\u{e72c}",
                    100,
                    if state.snapshot.active == "installed" {
                        "检查已安装插件更新"
                    } else {
                        "刷新当前仓库"
                    },
                    Action::Refresh,
                ),
                ("\u{e8b5}", 56, "从 .ubplugin 文件强制安装", Action::Local),
            ] {
                icon(
                    glyph,
                    RECT {
                        left: area.right - unit(offset),
                        top: unit(52),
                        right: area.right - unit(offset - 36),
                        bottom: unit(86),
                    },
                    tip,
                    action,
                );
            }
        }
        let end = area.right - unit(220);
        let capacity = ((end - unit(112)) / unit(130)).max(1) as usize;
        if state.tabs > 0 {
            icon(
                "\u{e76b}",
                RECT {
                    left: end + unit(4),
                    top: unit(52),
                    right: end + unit(32),
                    bottom: unit(86),
                },
                "前面的仓库",
                Action::Previous,
            );
        }
        if state.tabs + capacity < state.snapshot.sources.repositories.len() {
            icon(
                "\u{e76c}",
                RECT {
                    left: end + unit(36),
                    top: unit(52),
                    right: end + unit(64),
                    bottom: unit(86),
                },
                "后面的仓库",
                Action::Next,
            );
        }
        let mut tab = |label: &str, id: &str, left: i32, width: i32| {
            let rect = RECT {
                left,
                top: unit(52),
                right: left + width,
                bottom: unit(86),
            };
            fill(
                dc,
                &rect,
                if state.snapshot.active == id {
                    crate::visual::theme::palette().selection
                } else {
                    crate::visual::theme::palette().hover
                },
                unit(10),
            );
            text(
                label,
                RECT {
                    left: rect.left + unit(8),
                    right: rect.right - unit(8),
                    ..rect
                },
                crate::visual::theme::palette().text,
            );
            if !state.snapshot.busy {
                state.hits.push(Hit {
                    rect,
                    action: Action::Tab(id.into()),
                    tip: label.into(),
                });
            }
        };
        tab("已安装", "installed", unit(12), unit(90));
        for (index, source) in state
            .snapshot
            .sources
            .repositories
            .iter()
            .skip(state.tabs)
            .take(capacity)
            .enumerate()
        {
            tab(
                &source.name,
                &source.id,
                unit(112 + index as i32 * 130),
                unit(122),
            );
        }
        if let Some(source) = state
            .snapshot
            .sources
            .repositories
            .iter()
            .find(|source| source.id == state.snapshot.active)
        {
            text(
                &format!("{} · {}", source.full_name, source.id),
                RECT {
                    left: unit(16),
                    top: unit(92),
                    right: area.right - unit(60),
                    bottom: unit(118),
                },
                crate::visual::theme::palette().muted,
            );
            if !source.official_source() && !state.snapshot.busy {
                let rect = RECT {
                    left: area.right - unit(48),
                    top: unit(92),
                    right: area.right - unit(16),
                    bottom: unit(118),
                };
                text("×", rect, crate::visual::theme::palette().muted);
                state.hits.push(Hit {
                    rect,
                    action: Action::Remove,
                    tip: "移除此仓库（保留已安装插件）".into(),
                });
            }
        }
        let rows = ((area.bottom - unit(166)) / unit(76)).max(1) as usize;
        if state.snapshot.active == "installed" {
            let mut installed: Vec<_> = state
                .snapshot
                .registry
                .plugins
                .iter()
                .map(|plugin| {
                    (
                        plugin.id.clone(),
                        plugin.name.clone(),
                        plugin.version.clone(),
                        plugin
                            .source
                            .as_ref()
                            .map(|source| source.name.clone())
                            .unwrap_or_else(|| "本地安装".into()),
                    )
                })
                .collect();
            for plugin in &state.snapshot.development {
                if !installed.iter().any(|(id, _, _, _)| id == &plugin.id) {
                    installed.push((
                        plugin.id.clone(),
                        plugin.name.clone(),
                        plugin.version.clone(),
                        "开发目录".into(),
                    ));
                }
            }
            state.scroll = state.scroll.min(installed.len().saturating_sub(rows));
            for (index, (id, name, version, source)) in
                installed.iter().skip(state.scroll).take(rows).enumerate()
            {
                let update = state
                    .snapshot
                    .entries
                    .iter()
                    .find(|candidate| candidate.entry.id == *id);
                let rect = RECT {
                    left: unit(12),
                    top: unit(126 + index as i32 * 76),
                    right: area.right - unit(12),
                    bottom: unit(194 + index as i32 * 76),
                };
                fill(dc, &rect, crate::visual::theme::palette().hover, unit(12));
                text(
                    &if let Some(update) = update {
                        format!("{name}  v{version} → v{}", update.entry.version)
                    } else {
                        format!("{name}  v{version}")
                    },
                    RECT {
                        left: rect.left + unit(12),
                        top: rect.top + unit(4),
                        bottom: rect.top + unit(32),
                        ..rect
                    },
                    crate::visual::theme::palette().text,
                );
                text(
                    &format!("{id} · {source}"),
                    RECT {
                        left: rect.left + unit(12),
                        top: rect.top + unit(32),
                        bottom: rect.bottom - unit(4),
                        ..rect
                    },
                    crate::visual::theme::palette().muted,
                );
                if !state.snapshot.busy
                    && let Some(candidate) = update
                {
                    let target = RECT {
                        left: rect.right - unit(56),
                        top: rect.top + unit(16),
                        right: rect.right - unit(16),
                        bottom: rect.bottom - unit(16),
                    };
                    let font = SelectObject(dc, state.icons);
                    let mut glyph = target;
                    SetTextColor(dc, crate::visual::theme::palette().text);
                    DrawTextW(
                        dc,
                        wide("\u{e72c}").as_ptr(),
                        -1,
                        &mut glyph,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                    SelectObject(dc, font);
                    state.hits.push(Hit {
                        rect: target,
                        action: Action::Install(Box::new(candidate.clone()), true),
                        tip: format!(
                            "从 {} 更新到 v{}",
                            candidate.source.name, candidate.entry.version
                        ),
                    });
                }
            }
        } else {
            state.scroll = state
                .scroll
                .min(state.snapshot.entries.len().saturating_sub(rows));
            for (index, candidate) in state
                .snapshot
                .entries
                .iter()
                .skip(state.scroll)
                .take(rows)
                .enumerate()
            {
                let rect = RECT {
                    left: unit(12),
                    top: unit(126 + index as i32 * 76),
                    right: area.right - unit(12),
                    bottom: unit(194 + index as i32 * 76),
                };
                fill(dc, &rect, crate::visual::theme::palette().hover, unit(12));
                let existing = state
                    .snapshot
                    .registry
                    .plugins
                    .iter()
                    .find(|plugin| plugin.id == candidate.entry.id);
                let status = if let Some(existing) = existing {
                    let source = if existing
                        .source
                        .as_ref()
                        .is_some_and(|source| source.id == candidate.source.id)
                    {
                        "已安装"
                    } else {
                        "其他来源"
                    };
                    format!("{} · {source} v{}", candidate.entry.id, existing.version)
                } else {
                    candidate.entry.id.clone()
                };
                text(
                    &format!("{}  v{}", candidate.entry.name, candidate.entry.version),
                    RECT {
                        left: rect.left + unit(12),
                        top: rect.top + unit(4),
                        right: rect.right - unit(72),
                        bottom: rect.top + unit(32),
                    },
                    crate::visual::theme::palette().text,
                );
                text(
                    &status,
                    RECT {
                        left: rect.left + unit(12),
                        top: rect.top + unit(32),
                        right: rect.right - unit(72),
                        bottom: rect.bottom - unit(4),
                    },
                    crate::visual::theme::palette().muted,
                );
                if !state.snapshot.busy {
                    let install = existing.is_none_or(|existing| {
                        precedence(&candidate.entry.version, &existing.version)
                            .is_ok_and(|order| order.is_gt())
                    });
                    let target = RECT {
                        left: rect.right - unit(56),
                        top: rect.top + unit(16),
                        right: rect.right - unit(16),
                        bottom: rect.bottom - unit(16),
                    };
                    let font = SelectObject(dc, state.icons);
                    SetTextColor(
                        dc,
                        if install {
                            crate::visual::theme::palette().text
                        } else {
                            crate::visual::theme::palette().disabled
                        },
                    );
                    let mut target_text = target;
                    DrawTextW(
                        dc,
                        wide(if install { "\u{e8b5}" } else { "\u{e73e}" }).as_ptr(),
                        -1,
                        &mut target_text,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                    SelectObject(dc, font);
                    state.hits.push(Hit {
                        rect: target,
                        action: Action::Install(Box::new(candidate.clone()), install),
                        tip: if install {
                            if existing.is_some_and(|existing| {
                                existing
                                    .source
                                    .as_ref()
                                    .is_some_and(|source| source.id != candidate.source.id)
                            }) {
                                "切换来源并安装此版本"
                            } else {
                                "安装／更新"
                            }
                        } else {
                            "已是相同或更新版本；右键可强制安装"
                        }
                        .into(),
                    });
                }
            }
        }
        if state.snapshot.entries.is_empty()
            && state.snapshot.active != "installed"
            && !state.snapshot.busy
        {
            text(
                "暂无已发布的兼容插件",
                RECT {
                    left: unit(24),
                    top: unit(150),
                    right: area.right - unit(24),
                    bottom: unit(186),
                },
                crate::visual::theme::palette().muted,
            );
        }
        text(
            &state.snapshot.status,
            RECT {
                left: unit(16),
                top: area.bottom - unit(36),
                right: area.right - unit(16),
                bottom: area.bottom - unit(8),
            },
            crate::visual::theme::palette().muted,
        );
        SelectObject(dc, old);
        if let Some(path) = &state.export {
            let _ = surface.save_bmp(path);
        }
        surface.present(state.hwnd, state.glass);
        if let Some(tips) = &mut state.tips {
            tips.sync(
                state
                    .hits
                    .iter()
                    .map(|hit| Tip {
                        bounds: (hit.rect.left, hit.rect.top, hit.rect.right, hit.rect.bottom),
                        text: hit.tip.clone(),
                    })
                    .collect(),
            );
        }
        EndPaint(state.hwnd, &ps);
    }
}
pub fn smoke(directory: PathBuf) -> Result<()> {
    unsafe {
        std::fs::create_dir_all(&directory)?;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins");
        let mut hub = Hub::new(null_mut(), root.clone());
        let state = &mut hub.state;
        state.font = ultra_bar_native_ui::fonts::create_font(
            -13,
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
        state.icons = ultra_bar_native_ui::fonts::create_font(
            -18,
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
            wide("Segoe MDL2 Assets").as_ptr(),
        );
        SetWindowPos(
            state.hwnd,
            null_mut(),
            0,
            0,
            840,
            560,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let sources = Sources {
            repositories: vec![
                Source::official(),
                Source {
                    id: "github:1452115414".into(),
                    full_name: "adoin/ultra-bar".into(),
                    name: "自定义仓库".into(),
                },
            ],
        };
        state.snapshot = Arc::new(Snapshot {
            sources,
            active: OFFICIAL_ID.into(),
            entries: vec![],
            registry: Registry::default(),
            development: development(&root),
            status: "离屏布局检查，不请求网络".into(),
            busy: false,
        });
        state.export = Some(directory.join("plugin-hub-official.bmp"));
        paint(state);
        let mut snapshot = (*state.snapshot).clone();
        snapshot.active = "installed".into();
        state.snapshot = Arc::new(snapshot);
        state.export = Some(directory.join("plugin-hub-installed.bmp"));
        paint(state);
        println!(
            "plugin hub smoke: repository tabs and development inventory rendered; no keyboard or mouse input"
        );
        Ok(())
    }
}

unsafe fn file(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let mut buffer = [0u16; 32768];
        let filter: Vec<u16> = "Ultra Bar 插件包\0*.ubplugin\0\0".encode_utf16().collect();
        let mut dialog: OPENFILENAMEW = zeroed();
        dialog.lStructSize = size_of::<OPENFILENAMEW>() as _;
        dialog.hwndOwner = owner;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as _;
        dialog.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST;
        if GetOpenFileNameW(&mut dialog) == 0 {
            return None;
        }
        Some(
            String::from_utf16_lossy(
                &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
            )
            .into(),
        )
    }
}
