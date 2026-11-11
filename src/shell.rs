use crate::visual::text::draw_text as DrawTextW;
use crate::{
    Options,
    plugins::{Host, Slot, Widget},
    settings::{Settings, TaskbarSize},
    task_click::{Operation, TaskClick, foreground_candidate},
    task_layout::{self, Mode},
    tray,
    visual::{self, AppIcon, Surface, Tip, Tooltips},
};
use anyhow::{Context, Result, bail};
use std::{
    mem::{size_of, zeroed},
    os::windows::process::CommandExt,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{LibraryLoader::*, SystemInformation::*, Threading::*},
    UI::{
        Accessibility::*, Controls::WM_MOUSELEAVE, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::BOOL;

const PLUGIN_UPDATE: u32 = WM_APP + 1;
const APPBAR_UPDATE: u32 = WM_APP + 2;
const TRAY_UPDATE: u32 = WM_APP + 3;
const TRAY_IMPORT_PROMPT: u32 = WM_APP + 4;
const CLOCK_UPDATE: u32 = WM_APP + 7;
const HSHELL_FLASH: u32 = HSHELL_REDRAW | 0x8000;
const HSHELL_RUDEAPPACTIVATED: u32 = HSHELL_WINDOWACTIVATED | 0x8000;
pub const TRAY_DROP: u32 = WM_APP + 5;
static TASKS_DIRTY: AtomicBool = AtomicBool::new(true);
static LAST_ACTIVE_WINDOW: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn mark_tasks_dirty() {
    TASKS_DIRTY.store(true, Ordering::Release);
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[derive(Debug, PartialEq)]
struct Task {
    id: usize,
    key: String,
    executable: String,
    windows: Vec<TaskWindow>,
    pin: Option<crate::task_apps::Shortcut>,
    attention: bool,
    hwnd: HWND,
    title: String,
    icon: AppIcon,
}

#[derive(Clone, Debug, PartialEq)]
struct TaskWindow {
    hwnd: HWND,
    title: String,
    pid: u32,
    executable: String,
    app_id: Option<String>,
}

struct Drag {
    id: usize,
    start: POINT,
    active: bool,
    last_edge: Instant,
    click: Option<(HWND, TaskClick)>,
    pointer: POINT,
    grab_offset: i32,
    width: i32,
    original: Vec<usize>,
    original_anchor: usize,
    slots: Vec<(usize, i32)>,
}

#[derive(Clone)]
enum Action {
    Start,
    Notifications,
    Clock,
    Task(usize),
    Plugin(String, String),
    Overflow(Vec<usize>),
    TrayPanel,
    TrayIcon(String),
    Exit,
}

struct Hit {
    rect: RECT,
    action: Action,
    tooltip: String,
}

struct State {
    clock_feed: crate::clock_feed::Feed,
    preview_active: Option<usize>,
    jump_hub: Option<crate::jump_list::Hub>,
    jump_snapshot: Option<crate::jump_list::Snapshot>,
    jump_generation: u64,
    context_task: Option<usize>,
    task_popup: Option<crate::task_popup::Popup>,
    export: Option<std::path::PathBuf>,
    cancelled_click: bool,
    pins: Vec<crate::task_apps::Shortcut>,
    next_task: usize,
    shell_message: u32,
    attention: std::collections::HashMap<usize, String>,
    tray_drag: Option<(String, POINT, bool)>,
    tray_rect: RECT,
    isolated: bool,
    hwnd: HWND,
    host: Option<Host>,
    hub: Option<crate::plugin_system::Hub>,
    development_plugins: std::path::PathBuf,
    inventory_message: u32,
    tray_host: Option<tray::Host>,
    tray_snapshot: Arc<tray::Snapshot>,
    tray_popup: Option<tray::Popup>,
    tray_pinned: Vec<String>,
    tray_prompt_scheduled: bool,
    widgets: Vec<Widget>,
    services: crate::plugin_services::Manager,
    gpu_meter: Option<crate::usage_meter::compositor::Compositor>,
    gpu_meter_failed: bool,
    panel_started: Vec<std::path::PathBuf>,
    tasks: Vec<Task>,
    hits: Vec<Hit>,
    font: HFONT,
    icon_font: HFONT,
    tooltips: Option<Tooltips>,
    hovered: Option<usize>,
    glass: bool,
    settings: Settings,
    anchor: usize,
    foreground: HWND,
    mouse_foreground: Option<HWND>,
    pending_minimize: Option<(HWND, bool)>,
    drag: Option<Drag>,
    drag_layer: Option<crate::task_drag::Layer>,
    drag_layer_failed: bool,
    drag_settling: bool,
    drag_seed: Vec<(usize, i32)>,
    hidden: Vec<HWND>,
    original_taskbar_state: Option<u32>,
    appbar: bool,
    win11: bool,
    time: String,
    started: Instant,
    smoke_seconds: u64,
    dpi: i32,
}

impl Drop for State {
    fn drop(&mut self) {
        self.jump_hub.take();
        self.task_popup.take();
        self.tray_popup.take();
        self.hub.take();
        unsafe {
            if self.appbar {
                let mut data: APPBARDATA = zeroed();
                data.cbSize = size_of::<APPBARDATA>() as _;
                data.hWnd = self.hwnd;
                SHAppBarMessage(ABM_REMOVE, &mut data);
            }
            if let Some(original) = self.original_taskbar_state {
                restore_native(&self.hidden, original);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            if !self.icon_font.is_null() {
                DeleteObject(self.icon_font);
            }
        }
    }
}

pub fn run(options: Options) -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let initial_settings = Settings::load();
        visual::theme::set_accent(
            visual::theme::parse_hex(&initial_settings.accent_color)
                .unwrap_or(visual::theme::DEFAULT_ACCENT),
        );
        SetCurrentProcessExplicitAppUserModelID(wide("com.emssion.ultra-bar").as_ptr());
        let mutex = CreateMutexW(
            null(),
            0,
            wide(if options.isolated {
                "Local\\UltraBar.InstallerTest"
            } else {
                "Local\\UltraBar.Main"
            })
            .as_ptr(),
        );
        if mutex.is_null() {
            bail!("Cannot create instance lock");
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            bail!("Ultra Bar is already running");
        }
        let instance = GetModuleHandleW(null());
        let class = wide("UltraBar.Window");
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
            ..zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            CloseHandle(mutex);
            bail!("Cannot register window class");
        }
        let mut state = Box::new(State {
            preview_active: None,
            jump_hub: None,
            jump_snapshot: None,
            jump_generation: 0,
            context_task: None,
            task_popup: None,
            export: None,
            cancelled_click: false,
            pins: if options.isolated {
                vec![]
            } else {
                crate::task_apps::initial_pins()
            },
            next_task: 1,
            shell_message: RegisterWindowMessageW(wide("SHELLHOOK").as_ptr()),
            attention: std::collections::HashMap::new(),
            tray_drag: None,
            tray_rect: zeroed(),
            isolated: options.isolated,
            hwnd: null_mut(),
            host: None,
            hub: None,
            development_plugins: options.plugins.clone(),
            inventory_message: RegisterWindowMessageW(
                wide(crate::plugin_system::model::INVENTORY_MESSAGE).as_ptr(),
            ),
            tray_host: None,
            tray_snapshot: Arc::new(tray::Snapshot::empty()),
            tray_popup: None,
            tray_pinned: vec![],
            tray_prompt_scheduled: false,
            widgets: vec![],
            services: crate::plugin_services::Manager::default(),
            clock_feed: crate::clock_feed::Feed::new(0, CLOCK_UPDATE),
            gpu_meter: None,
            gpu_meter_failed: false,
            panel_started: vec![],
            tasks: vec![],
            hits: vec![],
            font: null_mut(),
            icon_font: null_mut(),
            tooltips: None,
            hovered: None,
            glass: false,
            settings: initial_settings,
            anchor: 0,
            foreground: null_mut(),
            mouse_foreground: None,
            pending_minimize: None,
            drag: None,
            drag_layer: None,
            drag_layer_failed: false,
            drag_settling: false,
            drag_seed: vec![],
            hidden: vec![],
            original_taskbar_state: None,
            appbar: false,
            win11: is_win11(),
            time: String::new(),
            started: Instant::now(),
            smoke_seconds: options.smoke_seconds,
            dpi: 96,
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST
                | WS_EX_LAYERED
                | if options.takeover {
                    WS_EX_TOOLWINDOW
                } else {
                    WS_EX_APPWINDOW
                },
            class.as_ptr(),
            wide("Ultra Bar").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            instance,
            (&mut *state as *mut State).cast(),
        );
        if hwnd.is_null() {
            CloseHandle(mutex);
            bail!("Cannot create taskbar window");
        }
        state.hwnd = hwnd;
        state.clock_feed = crate::clock_feed::Feed::new(hwnd as isize, CLOCK_UPDATE);
        if !options.isolated {
            state.jump_hub = Some(crate::jump_list::Hub::start(hwnd as usize));
        }
        RegisterShellHookWindow(hwnd);
        DragAcceptFiles(hwnd, 1);
        state.dpi = GetDpiForWindow(hwnd) as i32;
        state.glass = visual::enable_glass(hwnd);
        if !state.glass {
            eprintln!("[shell] compositor blur unavailable; using opaque fallback");
        }
        state.tooltips = Some(Tooltips::new(hwnd));
        set_font(&mut state);
        state.host = Some(Host::start(
            options.plugins,
            hwnd as isize,
            PLUGIN_UPDATE,
            options.prefer_development,
        ));
        if !options.isolated {
            let tray_host = tray::Host::start(hwnd as isize, TRAY_UPDATE);
            state.tray_popup = Some(tray::Popup::new(hwnd, tray_host.sender.clone()));
            state.tray_host = Some(tray_host);
        }
        if options.takeover {
            EnumWindows(
                Some(collect_taskbars),
                (&mut state.hidden as *mut Vec<HWND>) as _,
            );
            if state.hidden.is_empty() {
                DestroyWindow(hwnd);
                bail!("No native taskbar found; takeover cancelled");
            }
            let handles = state
                .hidden
                .iter()
                .map(|h| (*h as usize).to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut native_data: APPBARDATA = zeroed();
            native_data.cbSize = size_of::<APPBARDATA>() as _;
            native_data.hWnd = state.hidden[0];
            let original_state = SHAppBarMessage(ABM_GETSTATE, &mut native_data) as u32;
            // Spawn the recovery process before changing any system windows.
            if let Err(error) = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--restore-watch",
                    &std::process::id().to_string(),
                    &handles,
                    &original_state.to_string(),
                ])
                .creation_flags(0x01000000 | 0x08000000)
                .spawn()
            {
                state.hidden.clear();
                DestroyWindow(hwnd);
                bail!("Cannot start recovery process: {error}");
            }
            state.original_taskbar_state = Some(original_state);
            // Release Explorer's reserved strip before negotiating our own AppBar.
            native_data.lParam = (original_state | ABS_AUTOHIDE) as _;
            SHAppBarMessage(ABM_SETSTATE, &mut native_data);
            for native in &state.hidden {
                ShowWindow(*native, SW_HIDE);
            }
            let mut data: APPBARDATA = zeroed();
            data.cbSize = size_of::<APPBARDATA>() as _;
            data.hWnd = hwnd;
            data.uCallbackMessage = APPBAR_UPDATE;
            if SHAppBarMessage(ABM_NEW, &mut data) == 0 {
                DestroyWindow(hwnd);
                bail!("AppBar registration failed");
            }
            state.appbar = true;
        }
        position(&mut state);
        if let Some(directory) = options.visual_smoke {
            let result = smoke_taskbar(&mut state, &directory);
            DestroyWindow(hwnd);
            drop(state);
            CloseHandle(mutex);
            return result;
        }
        let hook = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_NAMECHANGE,
            null_mut(),
            Some(win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let foreground_hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        SetTimer(hwnd, 1, 1000, None);
        if !options.isolated {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        if options.isolated && std::env::args().any(|arg| arg == "--test-modal") {
            EnableWindow(hwnd, 0);
        }
        eprintln!(
            "[shell] running mode={} pid={}",
            if options.takeover {
                "takeover"
            } else {
                "preview"
            },
            std::process::id()
        );
        let mut msg: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_null() {
            UnhookWinEvent(hook);
        }
        if !foreground_hook.is_null() {
            UnhookWinEvent(foreground_hook);
        }
        drop(state);
        CloseHandle(mutex);
    }
    Ok(())
}

unsafe extern "system" fn win_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    TASKS_DIRTY.store(true, Ordering::Relaxed);
    if event == EVENT_SYSTEM_FOREGROUND && !hwnd.is_null() {
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid != std::process::id() && !unsafe { owned_plugin_window(hwnd) } {
            LAST_ACTIVE_WINDOW.store(hwnd as usize, Ordering::Relaxed);
        }
    }
    tray::event(hwnd, event);
}

unsafe fn set_font(state: &mut State) {
    unsafe {
        if !state.font.is_null() {
            DeleteObject(state.font);
        }
        state.font = CreateFontW(
            -(if state.settings.size == TaskbarSize::Small {
                12
            } else {
                14
            }) * state.dpi
                / 96,
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
        if !state.icon_font.is_null() {
            DeleteObject(state.icon_font);
        }
        state.icon_font = CreateFontW(
            -(if state.settings.size == TaskbarSize::Small {
                16
            } else {
                19
            }) * state.dpi
                / 96,
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
            wide(if state.win11 {
                "Segoe Fluent Icons"
            } else {
                "Segoe MDL2 Assets"
            })
            .as_ptr(),
        );
    }
}

unsafe fn position(state: &mut State) {
    unsafe {
        let monitor = MonitorFromWindow(state.hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let height = state.settings.size.height() * state.dpi / 96;
        let mut rect = info.rcMonitor;
        rect.top = rect.bottom - height;
        if state.appbar {
            let mut data: APPBARDATA = zeroed();
            data.cbSize = size_of::<APPBARDATA>() as _;
            data.hWnd = state.hwnd;
            data.uEdge = ABE_BOTTOM;
            data.rc = rect;
            SHAppBarMessage(ABM_QUERYPOS, &mut data);
            data.rc.top = data.rc.bottom - height;
            SHAppBarMessage(ABM_SETPOS, &mut data);
            rect = data.rc;
        } else {
            // Place the preview immediately above the native taskbar.
            rect.bottom = info.rcWork.bottom;
            rect.top = rect.bottom - height;
        }
        SetWindowPos(
            state.hwnd,
            HWND_TOPMOST,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE | if state.isolated { 0 } else { SWP_SHOWWINDOW },
        );
        InvalidateRect(state.hwnd, null(), 0);
    }
}

unsafe extern "system" fn collect_taskbars(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let mut class = [0u16; 128];
        let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as _);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        if (class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd")
            && IsWindowVisible(hwnd) != 0
        {
            (*(param as *mut Vec<HWND>)).push(hwnd);
        }
    }
    1
}

unsafe extern "system" fn collect_tasks(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let mut process = 0;
        GetWindowThreadProcessId(hwnd, &mut process);
        if process == GetCurrentProcessId() {
            return 1;
        }
        if hwnd == GetShellWindow()
            || hwnd == GetDesktopWindow()
            || IsWindowVisible(hwnd) == 0
            || !GetWindow(hwnd, GW_OWNER).is_null()
            || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW as isize != 0
        {
            return 1;
        }
        let mut cloaked = 0u32;
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            4,
        );
        if cloaked != 0 {
            return 1;
        }
        let length = GetWindowTextLengthW(hwnd).clamp(0, 32767) as usize;
        if length == 0 {
            return 1;
        }
        let mut title = vec![0u16; length + 1];
        let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as _);
        if len > 0 {
            let tasks = &mut *(param as *mut Vec<TaskWindow>);
            tasks.push(TaskWindow {
                hwnd,
                title: String::from_utf16_lossy(&title[..len as usize]),
                pid: process,
                executable: String::new(),
                app_id: None,
            });
        }
    }
    1
}

unsafe fn refresh_tasks(state: &mut State) -> bool {
    unsafe {
        let mut windows: Vec<TaskWindow> = Vec::new();
        EnumWindows(
            Some(collect_tasks),
            (&mut windows as *mut Vec<TaskWindow>) as _,
        );
        let mut old = std::mem::take(&mut state.tasks);
        let before: Vec<_> = old
            .iter()
            .map(|task| {
                (
                    task.id,
                    task.title.clone(),
                    task.windows.clone(),
                    task.pin.clone(),
                    task.attention,
                )
            })
            .collect();
        for window in &mut windows {
            window.executable = old
                .iter()
                .flat_map(|task| &task.windows)
                .find(|previous| previous.hwnd == window.hwnd && previous.pid == window.pid)
                .map(|previous| previous.executable.clone())
                .unwrap_or_else(|| crate::task_apps::process_path(window.hwnd));
            window.app_id = old
                .iter()
                .flat_map(|t| &t.windows)
                .find(|w| w.hwnd == window.hwnd && w.pid == window.pid)
                .and_then(|w| w.app_id.clone())
                .or_else(|| crate::task_apps::hosted_app_id(window.hwnd, &window.executable));
        }
        let mut groups = crate::task_apps::group(&state.pins, windows, |window| {
            crate::task_apps::task_identity(
                &window.executable,
                window.hwnd as usize,
                window.app_id.as_deref(),
            )
        });
        groups.sort_by_key(|(key, _, _)| {
            old.iter()
                .position(|task| &task.key == key)
                .unwrap_or(usize::MAX)
        });
        let foreground = effective_foreground();
        let anchored_foreground = old
            .iter()
            .any(|t| t.id == state.anchor && t.windows.iter().any(|w| w.hwnd == foreground));
        state
            .attention
            .retain(|hwnd, _| IsWindow(*hwnd as HWND) != 0);
        state.attention.remove(&(foreground as usize));
        for (key, mut members, pin) in groups {
            // Keep window picker order stable across z-order/focus changes too.
            members.sort_by_key(|window| {
                old.iter()
                    .flat_map(|task| &task.windows)
                    .position(|previous| previous.hwnd == window.hwnd)
                    .unwrap_or(usize::MAX)
            });
            let executable = pin
                .as_ref()
                .map(|pin| pin.executable.clone())
                .or_else(|| members.first().map(|window| window.executable.clone()))
                .unwrap_or_default();
            let hwnd = members
                .iter()
                .find(|window| window.hwnd == foreground)
                .or_else(|| members.first())
                .map(|window| window.hwnd)
                .unwrap_or(null_mut());
            let name = pin.as_ref().map(|pin| pin.name.clone()).unwrap_or_else(|| {
                if members.iter().any(|w| w.app_id.is_some()) {
                    members.first().map(|w| w.title.clone()).unwrap_or_default()
                } else {
                    std::path::Path::new(&executable)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into()
                }
            });
            let title = if members.len() == 1 {
                members[0].title.clone()
            } else if members.len() > 1 {
                name.clone()
            } else {
                name
            };
            let attention = state.attention.values().any(|app| app == &key);
            let mut task = if let Some(index) = old.iter().position(|task| task.key == key) {
                old.remove(index)
            } else {
                let id = state.next_task;
                state.next_task += 1;
                Task {
                    id,
                    key: key.clone(),
                    executable: executable.clone(),
                    hwnd,
                    title: String::new(),
                    windows: vec![],
                    pin: None,
                    attention: false,
                    icon: if let Some(pin) = &pin {
                        crate::task_apps::icon(pin, state.dpi, state.settings.size.icon())
                    } else {
                        AppIcon::read(hwnd, state.dpi, state.settings.size.icon())
                    },
                }
            };
            if task.hwnd.is_null() != hwnd.is_null() || task.pin != pin {
                task.icon = if let Some(pin) = &pin {
                    crate::task_apps::icon(pin, state.dpi, state.settings.size.icon())
                } else {
                    AppIcon::read(hwnd, state.dpi, state.settings.size.icon())
                };
            }
            task.hwnd = hwnd;
            task.windows = members;
            task.pin = pin;
            task.title = title;
            task.attention = attention;
            state.tasks.push(task);
        }
        if anchored_foreground
            && state.drag.is_none()
            && let Some(task) = state
                .tasks
                .iter()
                .find(|t| t.windows.iter().any(|w| w.hwnd == foreground))
        {
            state.anchor = task.id;
        }
        if let Some(popup) = &mut state.task_popup {
            popup.refresh();
        }
        if let Some(id) = state.context_task
            && !state.tasks.iter().any(|task| task.id == id)
        {
            if let Some(popup) = &mut state.task_popup {
                popup.hide();
            }
            state.context_task = None;
        }
        before
            != state
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.id,
                        task.title.clone(),
                        task.windows.clone(),
                        task.pin.clone(),
                        task.attention,
                    )
                })
                .collect::<Vec<_>>()
    }
}
fn task_snapshot_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(wide("UltraBar.TaskSnapshot.v1").as_ptr())
    })
}
pub fn diagnose_task_windows(pid: u32, path: &std::path::Path) -> Result<()> {
    struct Output {
        pid: u32,
        rows: Vec<serde_json::Value>,
    }
    unsafe extern "system" fn inspect(hwnd: HWND, data: isize) -> i32 {
        unsafe {
            let out = &mut *(data as *mut Output);
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, &mut process);
            if process != out.pid {
                return 1;
            }
            let mut title = [0u16; 512];
            let length = GetWindowTextW(hwnd, title.as_mut_ptr(), 512);
            let mut class = [0u16; 128];
            let n = GetClassNameW(hwnd, class.as_mut_ptr(), 128);
            let owner = GetWindow(hwnd, GW_OWNER);
            let mut owner_pid = 0;
            GetWindowThreadProcessId(owner, &mut owner_pid);
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as _,
                (&mut cloaked as *mut u32).cast(),
                4,
            );
            let mut accepted = vec![];
            collect_tasks(hwnd, (&mut accepted as *mut Vec<TaskWindow>) as isize);
            out.rows.push(serde_json::json!({"hwnd":format!("{:x}",hwnd as usize),"title":String::from_utf16_lossy(&title[..length.max(0) as usize]),"class":String::from_utf16_lossy(&class[..n.max(0) as usize]),"visible":IsWindowVisible(hwnd)!=0,"style":format!("{:x}",GetWindowLongPtrW(hwnd,GWL_STYLE) as u32),"ex_style":format!("{:x}",GetWindowLongPtrW(hwnd,GWL_EXSTYLE) as u32),"owner":format!("{:x}",owner as usize),"owner_pid":owner_pid,"owner_visible":IsWindowVisible(owner)!=0,"cloaked":cloaked,"accepted":!accepted.is_empty(),"executable":crate::task_apps::process_path(hwnd)}));
            1
        }
    }
    unsafe {
        let mut out = Output { pid, rows: vec![] };
        EnumWindows(Some(inspect), (&mut out as *mut Output) as isize);
        let root = FindWindowW(wide("UltraBar.Window").as_ptr(), null());
        let mut runtime = 0;
        let mut delivered = false;
        if !root.is_null() {
            delivered = SendMessageTimeoutW(
                root,
                task_snapshot_message(),
                pid as usize,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                500,
                &mut runtime,
            ) != 0;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &serde_json::json!({"windows":out.rows,"runtime_taskbar":{"delivered":delivered,"included":runtime&1!=0,"visible_slot":runtime&2!=0,"has_icon":runtime&4!=0,"pinned_group":runtime&8!=0,"group_windows":runtime>>16}}),
            )?,
        )?;
    }
    Ok(())
}

fn hit_action(state: &State, x: i32, y: i32) -> Option<Action> {
    state
        .hits
        .iter()
        .find(|hit| {
            x >= hit.rect.left && x < hit.rect.right && y >= hit.rect.top && y < hit.rect.bottom
        })
        .map(|hit| hit.action.clone())
}

fn app_error(owner: HWND, error: &anyhow::Error) {
    unsafe {
        MessageBoxW(
            owner,
            wide(&format!("{error:#}")).as_ptr(),
            wide("应用操作").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}
fn add_shortcuts(state: &mut State, paths: Vec<std::path::PathBuf>) {
    let mut next = state.pins.clone();
    let mut errors = vec![];
    for path in paths {
        match crate::task_apps::from_file(&path) {
            Ok(pin) => {
                let key = crate::task_apps::normalize(&pin.executable);
                if !next
                    .iter()
                    .any(|previous| crate::task_apps::normalize(&previous.executable) == key)
                {
                    match crate::task_apps::retain_link(pin) {
                        Ok(pin) => next.push(pin),
                        Err(error) => errors.push(format!("{}：{error:#}", path.display())),
                    }
                }
            }
            Err(error) => errors.push(format!("{}：{error:#}", path.display())),
        }
    }
    if next != state.pins {
        match crate::task_apps::save(&next) {
            Ok(()) => {
                state.pins = next;
                unsafe {
                    refresh_tasks(state);
                    InvalidateRect(state.hwnd, null(), 0);
                }
            }
            Err(error) => app_error(state.hwnd, &error),
        }
    }
    if !errors.is_empty() {
        app_error(state.hwnd, &anyhow::anyhow!(errors.join("\n")));
    }
}
fn persist_pin_order(state: &mut State) {
    let next: Vec<_> = state
        .tasks
        .iter()
        .filter_map(|task| task.pin.clone())
        .collect();
    if next != state.pins {
        match crate::task_apps::save(&next) {
            Ok(()) => state.pins = next,
            Err(error) => app_error(state.hwnd, &error),
        }
    }
}
unsafe fn popup_icon(state: &State, task: &Task) -> Option<AppIcon> {
    unsafe {
        if state.settings.size.icon() >= 24 {
            return None;
        }
        Some(if let Some(pin) = &task.pin {
            crate::task_apps::icon(pin, state.dpi, 24)
        } else {
            AppIcon::read(task.hwnd, state.dpi, 24)
        })
    }
}

unsafe fn popup_anchor(state: &State, action: &Action) -> RECT {
    unsafe {
        let mut rect = state
            .hits
            .iter()
            .find(|hit| match (&hit.action, action) {
                (Action::Task(a), Action::Task(b)) => a == b,
                (Action::Overflow(a), Action::Overflow(b)) => a == b,
                _ => false,
            })
            .map(|hit| hit.rect)
            .unwrap_or(RECT {
                left: 0,
                top: 0,
                right: 48,
                bottom: 40,
            });
        let mut origin = POINT { x: 0, y: 0 };
        ClientToScreen(state.hwnd, &mut origin);
        rect.left += origin.x;
        rect.right += origin.x;
        rect.top += origin.y;
        rect.bottom += origin.y;
        rect
    }
}
unsafe fn show_task_popup(
    state: &mut State,
    key: String,
    title: String,
    items: Vec<crate::task_popup::Item>,
    anchor: RECT,
) {
    unsafe {
        if let Some(popup) = &mut state.task_popup
            && popup.is_open_for(&key)
        {
            popup.hide();
            return;
        }
        state.task_popup =
            crate::task_popup::Popup::new(state.hwnd, key, title, items, anchor, state.dpi);
        if let Some(popup) = &mut state.task_popup {
            popup.show();
        }
    }
}
unsafe fn context_items(state: &State, id: usize) -> Vec<crate::task_popup::Item> {
    unsafe {
        let Some(task) = state.tasks.iter().find(|task| task.id == id) else {
            return vec![];
        };
        let mut items = vec![
            crate::task_popup::Item::command(
                crate::task_popup::Target::Launch(id),
                "打开应用",
                "\u{e8a7}",
                !task.executable.is_empty(),
            ),
            crate::task_popup::Item::command(
                crate::task_popup::Target::Pin(id),
                if task.pin.is_some() {
                    "取消固定"
                } else {
                    "固定到任务栏"
                },
                "\u{e718}",
                !task.executable.is_empty(),
            ),
        ];
        let foreground = GetAncestor(effective_foreground(), GA_ROOTOWNER);
        if crate::task_apps::is_explorer(&task.executable) {
            items.insert(
                0,
                crate::task_popup::Item::command(
                    crate::task_popup::Target::ThisPc,
                    "此电脑",
                    "\u{e7f4}",
                    true,
                ),
            );
        }
        if let Some(window) = task
            .windows
            .iter()
            .find(|window| window.hwnd == foreground)
            .or_else(|| {
                if task.windows.len() == 1 {
                    task.windows.first()
                } else {
                    None
                }
            })
            && let Some(enabled) = crate::window_commands::close_available(window.hwnd, window.pid)
        {
            items.push(crate::task_popup::Item::command(
                crate::task_popup::Target::CloseWindow {
                    hwnd: window.hwnd as usize,
                    pid: window.pid,
                },
                if task.windows.len() > 1 {
                    "关闭当前窗口"
                } else {
                    "关闭窗口"
                },
                "\u{e711}",
                enabled,
            ));
        }
        if task.windows.len() > 1
            && task.windows.iter().any(|window| {
                crate::window_commands::close_available(window.hwnd, window.pid).is_some()
            })
        {
            items.push(crate::task_popup::Item::command(
                crate::task_popup::Target::CloseGroup(id),
                "关闭所有窗口",
                "\u{e8bb}",
                task.windows.iter().any(|window| {
                    crate::window_commands::close_available(window.hwnd, window.pid) == Some(true)
                }),
            ));
        }
        if let Some(snapshot) = &state.jump_snapshot {
            let mut native = vec![];
            let mut category = String::new();
            for (index, entry) in snapshot.wire.entries.iter().enumerate() {
                let path = std::path::Path::new(&entry.icon);
                let local = entry.icon.as_bytes().get(1) == Some(&b':')
                    && path.extension().is_some_and(|ext| {
                        ["exe", "dll", "ico"]
                            .iter()
                            .any(|kind| ext.eq_ignore_ascii_case(kind))
                    });
                let icon = local
                    .then(|| AppIcon::from_resource(&entry.icon, entry.icon_index, state.dpi, 24));
                let mut item = crate::task_popup::Item::new(
                    crate::task_popup::Target::Jump {
                        generation: state.jump_generation,
                        index,
                    },
                    entry.title.clone(),
                    icon.as_ref()
                        .map(|icon| icon.handle)
                        .unwrap_or(task.icon.handle),
                    false,
                );
                item.tooltip = Some(format!("{} · {}", entry.category, entry.title));
                item.separator_before = !category.is_empty() && category != entry.category;
                category = entry.category.clone();
                native.push(item);
            }
            if !native.is_empty() {
                if items
                    .first()
                    .is_some_and(|item| item.target == crate::task_popup::Target::ThisPc)
                {
                    let navigation = items.remove(0);
                    native.insert(0, navigation);
                    if let Some(first) = native.get_mut(1) {
                        first.separator_before = true;
                    }
                }
                if let Some(first) = items.first_mut() {
                    first.separator_before = true;
                }
                native.extend(items);
                items = native;
            }
        }
        items
    }
}
unsafe fn task_menu(state: &mut State, id: usize) {
    unsafe {
        let Some(task) = state.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        let foreground = GetAncestor(effective_foreground(), GA_ROOTOWNER);
        let window = task
            .windows
            .iter()
            .find(|window| window.hwnd == foreground)
            .or_else(|| task.windows.iter().find(|window| window.hwnd == task.hwnd))
            .or_else(|| task.windows.first());
        let query = crate::jump_list::Query::new(
            window.map(|window| window.hwnd as usize).unwrap_or(0),
            window.map(|window| window.pid).unwrap_or(0),
            task.executable.clone(),
            task.pin.as_ref().map(|pin| pin.launch.clone()),
        );
        let title = task.title.clone();
        let anchor = popup_anchor(state, &Action::Task(id));
        state.context_task = Some(id);
        state.jump_snapshot = None;
        state.jump_generation = state
            .jump_hub
            .as_ref()
            .map(|hub| hub.request(query))
            .unwrap_or(state.jump_generation + 1);
        let items = context_items(state, id);
        show_task_popup(state, format!("context:{id}"), title, items, anchor);
    }
}
unsafe fn task_command(state: &mut State, target: crate::task_popup::Target) {
    unsafe {
        use crate::task_popup::Target;
        if target == Target::ThisPc {
            if let Err(error) = crate::task_apps::open_this_pc(state.hwnd) {
                app_error(state.hwnd, &error);
            }
            return;
        }
        if let Target::Jump { generation, index } = target {
            if generation == state.jump_generation
                && let Some(snapshot) = &state.jump_snapshot
                && let Err(error) = snapshot.invoke(index, state.hwnd)
            {
                app_error(state.hwnd, &error);
            }
            return;
        }
        if let Target::CloseWindow { hwnd, pid } = target {
            crate::window_commands::request_close(hwnd as HWND, pid);
            return;
        }
        let id = match target {
            Target::Pin(id) | Target::Launch(id) | Target::CloseGroup(id) => id,
            _ => return,
        };
        let Some(task) = state.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        let pin = task.pin.clone();
        let exe = task.executable.clone();
        let name = std::path::Path::new(&exe)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match target {
            Target::Pin(_) => {
                let mut next = state.pins.clone();
                if pin.is_some() {
                    next.retain(|pin| {
                        crate::task_apps::normalize(&pin.executable)
                            != crate::task_apps::normalize(&exe)
                    });
                } else {
                    next.push(crate::task_apps::Shortcut {
                        launch: exe.clone(),
                        executable: exe,
                        name,
                    });
                }
                match crate::task_apps::save(&next) {
                    Ok(()) => {
                        state.pins = next;
                        refresh_tasks(state);
                        InvalidateRect(state.hwnd, null(), 0);
                    }
                    Err(error) => app_error(state.hwnd, &error),
                }
            }
            Target::Launch(_) => {
                let pin = pin.unwrap_or(crate::task_apps::Shortcut {
                    launch: exe.clone(),
                    executable: exe,
                    name,
                });
                if let Err(error) = crate::task_apps::launch(&pin) {
                    app_error(state.hwnd, &error);
                }
            }
            Target::CloseGroup(_) => {
                for window in &task.windows {
                    crate::window_commands::request_close(window.hwnd, window.pid);
                }
            }
            _ => {}
        }
    }
}
fn set_tray_collapsed(state: &mut State, key: &str, collapsed: bool) {
    if let Some(item) = state
        .tray_snapshot
        .items
        .iter()
        .find(|item| item.key == key)
        && let Some(host) = &state.tray_host
    {
        host.send(tray::Command::SetCollapsed(
            item.preference_key.clone(),
            collapsed,
        ));
    }
}

unsafe fn owned_plugin_window(window: HWND) -> bool {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(GetAncestor(window, GA_ROOTOWNER), &mut pid);
        pid == GetCurrentProcessId()
    }
}
unsafe fn effective_foreground() -> HWND {
    unsafe {
        let raw = GetForegroundWindow();
        let mut gui: GUITHREADINFO = zeroed();
        gui.cbSize = size_of::<GUITHREADINFO>() as _;
        let active = if GetGUIThreadInfo(0, &mut gui) != 0 {
            gui.hwndActive
        } else {
            null_mut()
        };
        let valid = |window: HWND| {
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            !window.is_null()
                && IsWindow(window) != 0
                && IsIconic(window) == 0
                && pid != GetCurrentProcessId()
                && !owned_plugin_window(window)
        };
        let remembered = LAST_ACTIVE_WINDOW.load(Ordering::Relaxed) as HWND;
        let window = foreground_candidate(
            if valid(raw) { raw as usize } else { 0 },
            if valid(active) { active as usize } else { 0 },
            if valid(remembered) {
                remembered as usize
            } else {
                0
            },
        );
        if window != 0 {
            LAST_ACTIVE_WINDOW.store(window, Ordering::Relaxed);
        }
        window as HWND
    }
}

unsafe fn drag_move(state: &mut State, x: i32, y: i32) {
    unsafe {
        let Some(drag) = state.drag.as_mut() else {
            return;
        };
        if !drag.active {
            let threshold = GetSystemMetricsForDpi(SM_CXDRAG, state.dpi as u32).max(4);
            if (x - drag.start.x).abs() < threshold && (y - drag.start.y).abs() < threshold {
                return;
            }
            drag.active = true;
            state.drag_settling = false;
            KillTimer(state.hwnd, 5);
            state.drag_seed = drag.slots.clone();
            InvalidateRect(state.hwnd, null(), 0);
            if let Some(popup) = &mut state.task_popup {
                popup.hide();
            }
            SetTimer(state.hwnd, 2, 150, None);
        }
        drag.pointer = POINT { x, y };
        let dragged = drag.id;
        let reference = x - drag.grab_offset + drag.width / 2;
        let action = match hit_action(state, x, y) {
            Some(action @ Action::Overflow(_)) => Some(action),
            _ => hit_action(state, reference, y),
        };
        match action {
            Some(Action::Task(target)) if target != dragged => {
                let from = state.tasks.iter().position(|task| task.id == dragged);
                let to = state.tasks.iter().position(|task| task.id == target);
                let hit = state
                    .hits
                    .iter()
                    .find(|hit| matches!(hit.action, Action::Task(hwnd) if hwnd == target));
                if let (Some(from), Some(to), Some(hit)) = (from, to, hit) {
                    let midpoint = (hit.rect.left + hit.rect.right) / 2;
                    if (from < to && reference >= midpoint) || (from > to && reference <= midpoint)
                    {
                        task_layout::move_item(&mut state.tasks, from, to);
                        state.anchor = dragged;
                        InvalidateRect(state.hwnd, null(), 0);
                    }
                }
            }
            Some(Action::Overflow(hidden)) if !hidden.is_empty() => {
                let drag = state.drag.as_mut().unwrap();
                if drag.last_edge.elapsed().as_millis() >= 400 {
                    let dragged_index = state
                        .tasks
                        .iter()
                        .position(|task| task.id == dragged)
                        .unwrap_or(0);
                    let leading = state
                        .tasks
                        .first()
                        .is_some_and(|task| Some(&task.id) == hidden.first());
                    let target = if leading {
                        *hidden.last().unwrap()
                    } else {
                        hidden[0]
                    };
                    if let Some(target_index) =
                        state.tasks.iter().position(|task| task.id == target)
                    {
                        task_layout::move_item(&mut state.tasks, dragged_index, target_index);
                        state.anchor = dragged;
                        drag.last_edge = Instant::now();
                        InvalidateRect(state.hwnd, null(), 0);
                    }
                }
            }
            _ => {}
        }
        if let Some(drag) = &state.drag
            && drag.active
            && let Some(layer) = state.drag_layer.as_mut()
            && let Err(error) = layer.follow(drag.id, x - drag.grab_offset)
        {
            eprintln!("[drag] follow: {error}");
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as _);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *ptr;
        if message == task_snapshot_message() {
            if let Some(task) = state
                .tasks
                .iter()
                .find(|t| t.windows.iter().any(|window| window.pid == w as u32))
            {
                let visible = state
                    .hits
                    .iter()
                    .any(|h| matches!(h.action,Action::Task(id)if id==task.id));
                return (1
                    | if visible { 2 } else { 0 }
                    | if !task.icon.handle.is_null() { 4 } else { 0 }
                    | if task.pin.is_some() { 8 } else { 0 }
                    | (task.windows.len().min(65535) << 16)) as isize;
            }
            return 0;
        }
        if message == crate::task_apps::pins_message() {
            state.pins = crate::task_apps::load();
            refresh_tasks(state);
            state.tasks.sort_by_key(|task| {
                state
                    .pins
                    .iter()
                    .position(|pin| crate::task_apps::identity(&pin.executable, 0) == task.key)
                    .unwrap_or(usize::MAX)
            });
            InvalidateRect(hwnd, null(), 0);
            return 0;
        }
        if message == state.shell_message {
            let raw = l as HWND;
            let root = GetAncestor(raw, GA_ROOTOWNER);
            let target = if root.is_null() { raw } else { root };
            if w == HSHELL_FLASH as usize {
                if target != effective_foreground() {
                    let key = state
                        .tasks
                        .iter()
                        .find(|t| t.windows.iter().any(|window| window.hwnd == target))
                        .map(|t| t.key.clone())
                        .unwrap_or_else(|| {
                            let executable = crate::task_apps::process_path(target);
                            crate::task_apps::task_identity(
                                &executable,
                                target as usize,
                                crate::task_apps::hosted_app_id(target, &executable).as_deref(),
                            )
                        });
                    state.attention.entry(target as usize).or_insert(key);
                }
            } else if w == HSHELL_WINDOWDESTROYED as usize
                || w == HSHELL_WINDOWACTIVATED as usize
                || w == HSHELL_RUDEAPPACTIVATED as usize
            {
                state.attention.remove(&(target as usize));
            }
            TASKS_DIRTY.store(true, Ordering::Relaxed);
            if w == HSHELL_WINDOWDESTROYED as usize
                && let Some(popup) = &mut state.task_popup
            {
                popup.refresh();
            }
            let mut changed = false;
            for task in &mut state.tasks {
                let value = state.attention.values().any(|app| app == &task.key);
                changed |= task.attention != value;
                task.attention = value;
            }
            if changed {
                InvalidateRect(hwnd, null(), 0);
            }
            return 0;
        }
        if message == crate::lifecycle::exit_message() {
            if crate::lifecycle::modal_open(hwnd)
                || state.hub.as_ref().is_some_and(|hub| hub.busy())
            {
                return 2;
            }
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
            return 1;
        }
        match message {
            crate::jump_list::UPDATE => {
                let updates: Vec<_> = state
                    .jump_hub
                    .as_ref()
                    .map(|hub| hub.updates.try_iter().collect())
                    .unwrap_or_default();
                for update in updates {
                    if update.generation != state.jump_generation {
                        continue;
                    }
                    state.jump_snapshot = match update.result {
                        Ok(snapshot) => Some(snapshot),
                        Err(error) => {
                            eprintln!("[jump-list] {error:#}");
                            None
                        }
                    };
                    if let Some(id) = state.context_task
                        && state
                            .task_popup
                            .as_ref()
                            .is_some_and(|popup| popup.is_open_for(&format!("context:{id}")))
                    {
                        let items = context_items(state, id);
                        if let Some(popup) = &mut state.task_popup {
                            popup.replace_items(items);
                        }
                    }
                }
                0
            }
            crate::task_popup::SELECTED => {
                if let Some(target) = state
                    .task_popup
                    .as_mut()
                    .and_then(|popup| popup.take_selection())
                    && target.valid()
                {
                    match target {
                        crate::task_popup::Target::Window { hwnd, .. } => {
                            let hwnd = hwnd as HWND;
                            let click = TaskClick::begin(hwnd as usize, 0, IsIconic(hwnd) != 0);
                            apply_task_click(state, hwnd, click);
                        }
                        crate::task_popup::Target::Task(id) => execute(state, Action::Task(id)),
                        command => task_command(state, command),
                    }
                }
                0
            }
            WM_DROPFILES => {
                let drop = w as windows_sys::Win32::UI::Shell::HDROP;
                let count = DragQueryFileW(drop, u32::MAX, null_mut(), 0).min(128);
                let mut paths = vec![];
                for index in 0..count {
                    let mut path = [0u16; 32768];
                    let len = DragQueryFileW(drop, index, path.as_mut_ptr(), path.len() as u32);
                    if len > 0 {
                        paths.push(std::path::PathBuf::from(String::from_utf16_lossy(
                            &path[..len as usize],
                        )));
                    }
                }
                DragFinish(drop);
                add_shortcuts(state, paths);
                0
            }
            TRAY_DROP => {
                if let Some((key, point)) = state
                    .tray_popup
                    .as_mut()
                    .and_then(|popup| popup.take_drop())
                {
                    let mut point = point;
                    ScreenToClient(hwnd, &mut point);
                    if point.x >= state.tray_rect.left
                        && point.x < state.tray_rect.right
                        && point.y >= state.tray_rect.top
                        && point.y < state.tray_rect.bottom
                    {
                        set_tray_collapsed(state, &key, false);
                    }
                }
                0
            }
            WM_PAINT => {
                paint(state);
                0
            }
            m if m == state.inventory_message => {
                if let Some(host) = &state.host {
                    host.refresh();
                }
                if let Some(hub) = &state.hub {
                    hub.inventory_changed();
                }
                0
            }
            WM_ERASEBKGND => 1,
            WM_SHOWWINDOW => {
                if let Some(compositor) = &state.gpu_meter {
                    compositor.input_visibility(w != 0);
                }
                DefWindowProcW(hwnd, message, w, l)
            }
            WM_MOVE => {
                if let Some(compositor) = &state.gpu_meter {
                    compositor.input_position();
                }
                DefWindowProcW(hwnd, message, w, l)
            }
            WM_MOUSEACTIVATE => {
                state.mouse_foreground = if (l as u32 >> 16) == WM_LBUTTONDOWN {
                    Some(effective_foreground())
                } else {
                    None
                };
                MA_NOACTIVATE as _
            }
            CLOCK_UPDATE => {
                state.clock_feed.poll(&state.widgets);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_TIMER => {
                if w == 5 {
                    KillTimer(hwnd, 5);
                    if !state.drag.as_ref().is_some_and(|d| d.active) {
                        state.drag_settling = false;
                        state.drag_layer = None;
                        state.drag_seed.clear();
                        InvalidateRect(hwnd, null(), 0);
                    }
                    return 0;
                }
                if w == 4 {
                    InvalidateRect(hwnd, null(), 0);
                    if !state.services.animating() {
                        KillTimer(hwnd, 4);
                    }
                    return 0;
                }
                if w == 3 {
                    if let Some((target, retried)) = state.pending_minimize.take() {
                        let minimized = IsWindow(target) != 0 && IsIconic(target) != 0;
                        eprintln!(
                            "[task-click] verify-minimize target={:#x} minimized={} retried={}",
                            target as usize, minimized, retried
                        );
                        if !minimized && !retried && IsWindow(target) != 0 {
                            ShowWindowAsync(target, SW_MINIMIZE);
                            state.pending_minimize = Some((target, true));
                        } else {
                            KillTimer(hwnd, 3);
                        }
                    } else {
                        KillTimer(hwnd, 3);
                    }
                    return 0;
                }
                if w == 2 {
                    if state.drag.as_ref().is_some_and(|drag| drag.active) {
                        let mut point: POINT = zeroed();
                        GetCursorPos(&mut point);
                        ScreenToClient(hwnd, &mut point);
                        drag_move(state, point.x, point.y);
                    }
                    return 0;
                }
                if let Some(host) = &state.tray_host {
                    host.refresh_if_dirty();
                }
                if state.smoke_seconds > 0
                    && state.started.elapsed().as_secs() >= state.smoke_seconds
                {
                    DestroyWindow(hwnd);
                    return 0;
                }
                let mut now: SYSTEMTIME = zeroed();
                GetLocalTime(&mut now);
                let time =
                    crate::clock::format(&state.settings.clock_format, &now).unwrap_or_else(|_| {
                        crate::clock::format(crate::clock::DEFAULT_FORMAT, &now).unwrap()
                    });
                let mut changed = time != state.time;
                changed |= state.clock_feed.poll(&state.widgets);
                state.time = time;
                let foreground = effective_foreground();
                let foreground_changed = foreground != state.foreground;
                state.foreground = foreground;
                let executable = crate::task_apps::process_path(foreground);
                let mut title = [0u16; 512];
                let len = GetWindowTextW(foreground, title.as_mut_ptr(), title.len() as i32);
                changed |= state.services.poll(
                    &state.widgets,
                    hwnd as isize,
                    executable,
                    String::from_utf16_lossy(&title[..len.max(0) as usize]),
                );
                if state.services.animating() {
                    SetTimer(hwnd, 4, 20, None);
                }
                if TASKS_DIRTY.swap(false, Ordering::Relaxed) {
                    changed |= refresh_tasks(state);
                }
                let active_key = state
                    .tasks
                    .iter()
                    .find(|task| task.windows.iter().any(|window| window.hwnd == foreground))
                    .map(|task| task.key.clone());
                let previous_attention = state.attention.len();
                state.attention.remove(&(foreground as usize));
                if let Some(key) = active_key {
                    state.attention.retain(|_, app| app != &key);
                }
                if state.attention.len() != previous_attention {
                    for task in &mut state.tasks {
                        task.attention = state.attention.values().any(|app| app == &task.key);
                    }
                    changed = true;
                }
                if state.drag.is_none()
                    && (foreground_changed
                        || !state.tasks.iter().any(|task| task.id == state.anchor))
                    && let Some(task) = state
                        .tasks
                        .iter()
                        .find(|task| task.windows.iter().any(|window| window.hwnd == foreground))
                {
                    state.anchor = task.id;
                    changed = true;
                }
                if changed {
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            PLUGIN_UPDATE => {
                if let Some(host) = &state.host {
                    while let Ok(widgets) = host.updates.try_recv() {
                        state.widgets = widgets;
                    }
                }
                state.services.sync(&state.widgets, hwnd as isize);
                for widget in &state.widgets {
                    if !widget.service
                        && widget.background
                        && let Some(binary) = &widget.panel
                        && !state.panel_started.contains(binary)
                    {
                        match crate::plugin_panels::launch(widget, hwnd, false) {
                            Ok(()) => state.panel_started.push(binary.clone()),
                            Err(error) => eprintln!("[plugins] {}: {error:#}", widget.id),
                        }
                    }
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            TRAY_UPDATE => {
                let mut offer = false;
                if let Some(host) = &state.tray_host {
                    while let Ok(snapshot) = host.updates.try_recv() {
                        offer |= snapshot.offer_import;
                        if snapshot.status != state.tray_snapshot.status
                            && !snapshot.status.is_empty()
                        {
                            eprintln!("[tray] {}", snapshot.status);
                        }
                        state.tray_snapshot = snapshot;
                    }
                }
                if offer && !state.tray_prompt_scheduled {
                    state.tray_prompt_scheduled = true;
                    PostMessageW(hwnd, TRAY_IMPORT_PROMPT, 0, 0);
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            TRAY_IMPORT_PROMPT => {
                if MessageBoxW(hwnd, wide("是否导入 Windows 当前的托盘显示与收纳设置？\n选择“否”跳过，之后可在“设置”中手动导入。").as_ptr(), wide("系统托盘").as_ptr(), MB_YESNO | MB_DEFBUTTON2 | MB_ICONQUESTION) == IDYES
                    && let Some(host) = &state.tray_host { host.send(tray::Command::Import); }
                0
            }
            APPBAR_UPDATE => {
                if w == ABN_POSCHANGED as usize {
                    position(state);
                }
                0
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DWMCOMPOSITIONCHANGED => {
                state.glass = visual::enable_glass(hwnd);
                position(state);
                TASKS_DIRTY.store(true, Ordering::Relaxed);
                0
            }
            WM_DISPLAYCHANGE => {
                position(state);
                TASKS_DIRTY.store(true, Ordering::Relaxed);
                0
            }
            WM_DPICHANGED => {
                state.dpi = (w & 0xffff) as i32;
                for task in &mut state.tasks {
                    task.icon = if let Some(pin) = &task.pin {
                        crate::task_apps::icon(pin, state.dpi, state.settings.size.icon())
                    } else {
                        AppIcon::read(task.hwnd, state.dpi, state.settings.size.icon())
                    };
                }
                set_font(state);
                position(state);
                0
            }
            WM_LBUTTONDOWN => {
                state.cancelled_click = false;
                let x = (l as u16) as i16 as i32;
                let y = ((l >> 16) as u16) as i16 as i32;
                let foreground = state
                    .mouse_foreground
                    .take()
                    .unwrap_or_else(|| effective_foreground());
                if let Some(Action::TrayIcon(key)) = hit_action(state, x, y) {
                    state.tray_drag = Some((key, POINT { x, y }, false));
                    SetCapture(hwnd);
                    return 0;
                }
                if let Some(Action::Task(id)) = hit_action(state, x, y) {
                    let Some(group) = state.tasks.iter().find(|task| task.id == id) else {
                        return 0;
                    };
                    let task = group.hwnd;
                    let click = TaskClick::begin(
                        GetAncestor(task, GA_ROOTOWNER) as usize,
                        GetAncestor(foreground, GA_ROOTOWNER) as usize,
                        IsIconic(task) != 0,
                    );
                    eprintln!(
                        "[task-click] press target={:#x} foreground={:#x} target_owner={:#x} foreground_owner={:#x} minimized={} intended={:?}",
                        task as usize,
                        foreground as usize,
                        GetAncestor(task, GA_ROOTOWNER) as usize,
                        GetAncestor(foreground, GA_ROOTOWNER) as usize,
                        IsIconic(task),
                        click.release(IsIconic(task) != 0)
                    );
                    state.drag = Some(Drag {
                        id,
                        start: POINT { x, y },
                        active: false,
                        last_edge: Instant::now(),
                        click: if group.windows.len() == 1 {
                            Some((task, click))
                        } else {
                            None
                        },
                        pointer: POINT { x, y },
                        grab_offset: state
                            .hits
                            .iter()
                            .find(|h| matches!(h.action,Action::Task(task)if task==id))
                            .map(|h| x - h.rect.left)
                            .unwrap_or(0),
                        width: state
                            .hits
                            .iter()
                            .find(|h| matches!(h.action,Action::Task(task)if task==id))
                            .map(|h| h.rect.right - h.rect.left)
                            .unwrap_or(1),
                        original: state.tasks.iter().map(|t| t.id).collect(),
                        original_anchor: state.anchor,
                        slots: state
                            .hits
                            .iter()
                            .filter_map(|h| {
                                if let Action::Task(id) = h.action {
                                    Some((id, h.rect.left))
                                } else {
                                    None
                                }
                            })
                            .collect(),
                    });
                    SetCapture(hwnd);
                }
                0
            }
            WM_LBUTTONDBLCLK => {
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                if let Some(Action::TrayIcon(key)) = hit_action(state, x, y)
                    && let Some(host) = &state.tray_host
                {
                    host.send(tray::Command::Click(key, tray::Click::Double));
                }
                0
            }
            WM_LBUTTONUP => {
                if state.cancelled_click {
                    state.cancelled_click = false;
                    return 0;
                }
                let x = (l as u16) as i16 as i32;
                let y = ((l >> 16) as u16) as i16 as i32;
                let action = hit_action(state, x, y);
                if let Some((key, _, active)) = state.tray_drag.take() {
                    ReleaseCapture();
                    if active {
                        let mut point = POINT { x, y };
                        ClientToScreen(hwnd, &mut point);
                        if state
                            .tray_popup
                            .as_ref()
                            .is_some_and(|popup| popup.contains(point))
                            || matches!(action, Some(Action::TrayPanel))
                        {
                            set_tray_collapsed(state, &key, true);
                        }
                    } else if matches!(&action,Some(Action::TrayIcon(current)) if current==&key) {
                        execute(state, Action::TrayIcon(key));
                    }
                    return 0;
                }
                if let Some(drag) = state.drag.take() {
                    KillTimer(hwnd, 2);
                    ReleaseCapture();
                    if !drag.active && matches!(action, Some(Action::Task(task)) if task == drag.id)
                    {
                        if let Some((target, click)) = drag.click {
                            apply_task_click(state, target, click);
                        } else {
                            execute(state, Action::Task(drag.id));
                        }
                    }
                    if drag.active {
                        state.hovered = None;
                        persist_pin_order(state);
                        state.drag_settling = true;
                        SetTimer(hwnd, 5, crate::task_drag::SETTLE_MS, None);
                    }
                    InvalidateRect(hwnd, null(), 0);
                    return 0;
                }
                if let Some(action) = action {
                    execute(state, action);
                }
                0
            }
            WM_MOUSEMOVE => {
                let x = (l as u16) as i16 as i32;
                let y = ((l >> 16) as u16) as i16 as i32;
                if let Some((_, start, active)) = &mut state.tray_drag
                    && !*active
                    && ((x - start.x).abs()
                        >= GetSystemMetricsForDpi(SM_CXDRAG, state.dpi as u32).max(4)
                        || (y - start.y).abs()
                            >= GetSystemMetricsForDpi(SM_CYDRAG, state.dpi as u32).max(4))
                {
                    *active = true;
                    if let Some(popup) = &mut state.tray_popup {
                        popup.open_for_drag(state.dpi);
                    }
                }
                if state.drag.is_some() {
                    drag_move(state, x, y);
                }
                let hovered = state.hits.iter().position(|hit| {
                    x >= hit.rect.left
                        && x < hit.rect.right
                        && y >= hit.rect.top
                        && y < hit.rect.bottom
                });
                if !state.drag.as_ref().is_some_and(|d| d.active) && state.hovered != hovered {
                    state.hovered = hovered;
                    InvalidateRect(hwnd, null(), 0);
                }
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as _,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
                0
            }
            WM_MOUSELEAVE => {
                if state.hovered.take().is_some() {
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                if state.drag.is_some() || state.tray_drag.is_some() {
                    state.cancelled_click = true;
                }
                if let Some(drag) = state.drag.take()
                    && drag.active
                {
                    state.tasks.sort_by_key(|t| {
                        drag.original
                            .iter()
                            .position(|id| *id == t.id)
                            .unwrap_or(usize::MAX)
                    });
                    state.anchor = drag.original_anchor;
                    state.drag_settling = true;
                    SetTimer(hwnd, 5, crate::task_drag::SETTLE_MS, None);
                }
                state.tray_drag = None;
                if message == WM_CANCELMODE && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                KillTimer(hwnd, 2);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_RBUTTONUP => {
                let x = (l as u16) as i16 as i32;
                let y = ((l >> 16) as u16) as i16 as i32;
                if let Some(Action::Task(id)) = hit_action(state, x, y) {
                    task_menu(state, id);
                    return 0;
                }
                if let Some(Action::TrayIcon(key)) = hit_action(state, x, y) {
                    if let Some(host) = &state.tray_host {
                        host.send(tray::Command::Click(key, tray::Click::Right));
                    }
                    return 0;
                }
                if x < state.settings.size.start_pixels(state.dpi) {
                    shortcut(&[VK_LWIN, b'X' as u16]);
                } else {
                    let menu = CreatePopupMenu();
                    let settings = CreatePopupMenu();
                    AppendMenuW(settings, MF_STRING, 10, wide("插件中心…").as_ptr());
                    AppendMenuW(settings, MF_STRING, 12, wide("固定快捷方式…").as_ptr());
                    AppendMenuW(settings, MF_STRING, 13, wide("同步系统固定应用").as_ptr());
                    AppendMenuW(settings, MF_STRING, 14, wide("主色…").as_ptr());
                    AppendMenuW(settings, MF_STRING, 15, wide("恢复默认主色").as_ptr());
                    AppendMenuW(settings, MF_SEPARATOR, 0, null());
                    AppendMenuW(
                        settings,
                        MF_STRING
                            | if !state.settings.icons_only {
                                MF_CHECKED
                            } else {
                                0
                            },
                        2,
                        wide("自动显示标题").as_ptr(),
                    );
                    AppendMenuW(
                        settings,
                        MF_STRING
                            | if state.settings.icons_only {
                                MF_CHECKED
                            } else {
                                0
                            },
                        3,
                        wide("始终仅图标").as_ptr(),
                    );
                    AppendMenuW(settings, MF_SEPARATOR, 0, null());
                    let formats = CreatePopupMenu();
                    for (id, format) in
                        [(20, "MM-DD HH:mm"), (21, "HH:mm"), (22, "YYYY-MM-DD HH:mm")]
                    {
                        AppendMenuW(
                            formats,
                            MF_STRING
                                | if state.settings.clock_format == format {
                                    MF_CHECKED
                                } else {
                                    0
                                },
                            id,
                            wide(format).as_ptr(),
                        );
                    }
                    AppendMenuW(formats, MF_STRING, 23, wide("自定义…").as_ptr());
                    AppendMenuW(
                        settings,
                        MF_POPUP,
                        formats as usize,
                        wide("时间格式").as_ptr(),
                    );
                    let extensions = CreatePopupMenu();
                    AppendMenuW(
                        extensions,
                        MF_STRING
                            | if state.settings.clock_extension.is_empty() {
                                MF_CHECKED
                            } else {
                                0
                            },
                        30,
                        wide("自动").as_ptr(),
                    );
                    AppendMenuW(
                        extensions,
                        MF_STRING
                            | if state.settings.clock_extension == "system" {
                                MF_CHECKED
                            } else {
                                0
                            },
                        31,
                        wide("系统入口").as_ptr(),
                    );
                    for (index, widget) in state
                        .widgets
                        .iter()
                        .filter(|widget| widget.slot == Slot::Clock)
                        .enumerate()
                    {
                        AppendMenuW(
                            extensions,
                            MF_STRING
                                | if crate::plugin_system::model::canonical_id(
                                    &state.settings.clock_extension,
                                ) == widget.id
                                {
                                    MF_CHECKED
                                } else {
                                    0
                                },
                            100 + index,
                            wide(&widget.name).as_ptr(),
                        );
                    }
                    AppendMenuW(
                        settings,
                        MF_POPUP,
                        extensions as usize,
                        wide("时钟扩展").as_ptr(),
                    );
                    let sizes = CreatePopupMenu();
                    AppendMenuW(
                        sizes,
                        MF_STRING
                            | if state.settings.size == TaskbarSize::Normal {
                                MF_CHECKED
                            } else {
                                0
                            },
                        7,
                        wide("正常").as_ptr(),
                    );
                    AppendMenuW(
                        sizes,
                        MF_STRING
                            | if state.settings.size == TaskbarSize::Small {
                                MF_CHECKED
                            } else {
                                0
                            },
                        8,
                        wide("小").as_ptr(),
                    );
                    AppendMenuW(
                        settings,
                        MF_POPUP,
                        sizes as usize,
                        wide("任务栏大小").as_ptr(),
                    );
                    AppendMenuW(settings, MF_STRING, 4, wide("托盘图标").as_ptr());
                    AppendMenuW(settings, MF_STRING, 5, wide("导入系统托盘设置").as_ptr());
                    AppendMenuW(settings, MF_STRING, 6, wide("刷新托盘图标").as_ptr());
                    AppendMenuW(menu, MF_POPUP, settings as usize, wide("设置").as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    AppendMenuW(menu, MF_STRING, 1, wide("退出 Ultra Bar").as_ptr());
                    let mut point: POINT = zeroed();
                    GetCursorPos(&mut point);
                    SetForegroundWindow(hwnd);
                    let result = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_NONOTIFY,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        null(),
                    );
                    DestroyMenu(menu);
                    if result == 1 {
                        execute(state, Action::Exit);
                    } else if result == 12 {
                        if let Some(path) = crate::task_apps::choose(hwnd) {
                            add_shortcuts(state, vec![path]);
                        }
                    } else if result == 13 {
                        match crate::task_apps::sync_system_pins() {
                            Ok(_) => {
                                PostMessageW(hwnd, crate::task_apps::pins_message(), 0, 0);
                            }
                            Err(error) => app_error(hwnd, &error),
                        }
                    } else if result == 14 || result == 15 {
                        let selected = if result == 15 {
                            Some(visual::theme::DEFAULT_ACCENT)
                        } else {
                            crate::color_picker::choose(hwnd, visual::theme::accent_rgb())
                        };
                        if let Some(rgb) = selected {
                            let mut next = state.settings.clone();
                            next.accent_color = visual::theme::hex(rgb);
                            match next.save() {
                                Ok(()) => {
                                    state.settings = next;
                                    apply_theme(hwnd, rgb);
                                }
                                Err(error) => app_error(hwnd, &error),
                            }
                        }
                    } else if result == 10 {
                        if state.hub.is_none() {
                            state.hub = Some(crate::plugin_system::Hub::new(
                                hwnd,
                                state.development_plugins.clone(),
                            ));
                        }
                        if let Some(hub) = &mut state.hub {
                            hub.show();
                        }
                    } else if result == 30 || result == 31 || (100..132).contains(&result) {
                        let mut next = state.settings.clone();
                        next.clock_extension = match result {
                            30 => String::new(),
                            31 => "system".into(),
                            _ => state
                                .widgets
                                .iter()
                                .filter(|widget| widget.slot == Slot::Clock)
                                .nth(result as usize - 100)
                                .map(|widget| widget.id.clone())
                                .unwrap_or_default(),
                        };
                        match next.save() {
                            Ok(()) => {
                                state.settings = next;
                                InvalidateRect(hwnd, null(), 0);
                            }
                            Err(error) => {
                                MessageBoxW(
                                    hwnd,
                                    wide(&format!("{error:#}")).as_ptr(),
                                    wide("时钟扩展").as_ptr(),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                        }
                    } else if (20..=23).contains(&result) {
                        let format = match result {
                            20 => Some("MM-DD HH:mm".to_owned()),
                            21 => Some("HH:mm".to_owned()),
                            22 => Some("YYYY-MM-DD HH:mm".to_owned()),
                            _ => crate::input_dialog::edit(
                                hwnd,
                                "时间格式",
                                "例如 MM-DD HH:mm；ss 为秒，ddd 为星期，[] 内为固定文字",
                                &state.settings.clock_format,
                            ),
                        };
                        if let Some(format) = format {
                            let mut now: SYSTEMTIME = zeroed();
                            GetLocalTime(&mut now);
                            let result = crate::clock::format(&format, &now).and_then(|display| {
                                let mut next = state.settings.clone();
                                next.clock_format = format;
                                next.save()?;
                                state.settings = next;
                                state.time = display;
                                Ok(())
                            });
                            if let Err(error) = result {
                                MessageBoxW(
                                    hwnd,
                                    wide(&format!("{error:#}")).as_ptr(),
                                    wide("时间格式").as_ptr(),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                            InvalidateRect(hwnd, null(), 0);
                        }
                    } else if result == 4 {
                        if let Some(popup) = &mut state.tray_popup {
                            popup.update(state.tray_snapshot.clone(), state.tray_pinned.clone());
                            popup.settings();
                        }
                    } else if result == 5 {
                        if let Some(host) = &state.tray_host {
                            host.send(tray::Command::Import);
                        }
                    } else if result == 6 {
                        if let Some(host) = &state.tray_host {
                            host.send(tray::Command::Refresh);
                        }
                    } else if matches!(result, 2 | 3 | 7 | 8) {
                        let icons_only = result == 3;
                        let mut next = state.settings.clone();
                        if result == 2 || result == 3 {
                            next.icons_only = icons_only;
                        } else {
                            next.size = if result == 8 {
                                TaskbarSize::Small
                            } else {
                                TaskbarSize::Normal
                            };
                        }
                        match next.save() {
                            Ok(()) => {
                                state.settings = next;
                                state.hovered = None;
                                set_font(state);
                                for task in &mut state.tasks {
                                    task.icon = if let Some(pin) = &task.pin {
                                        crate::task_apps::icon(
                                            pin,
                                            state.dpi,
                                            state.settings.size.icon(),
                                        )
                                    } else {
                                        AppIcon::read(
                                            task.hwnd,
                                            state.dpi,
                                            state.settings.size.icon(),
                                        )
                                    };
                                }
                                position(state);
                                InvalidateRect(hwnd, null(), 0);
                            }
                            Err(error) => {
                                MessageBoxW(
                                    hwnd,
                                    wide(&format!("无法保存设置：{error:#}")).as_ptr(),
                                    wide("Ultra Bar").as_ptr(),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                        }
                    }
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                state.drag_layer = None;
                KillTimer(hwnd, 5);
                state.gpu_meter = None;
                DeregisterShellHookWindow(hwnd);
                KillTimer(hwnd, 1);
                KillTimer(hwnd, 2);
                KillTimer(hwnd, 3);
                PostQuitMessage(0);
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
        // Per-pixel alpha preserves an actual translucent background while text
        // and application icons stay opaque. No frame loop or wallpaper capture.
        let Some(surface) = Surface::new(area.right, area.bottom) else {
            EndPaint(state.hwnd, &ps);
            return;
        };
        let buffer = surface.dc;
        let old_font = SelectObject(buffer, state.font);
        let background = CreateSolidBrush(visual::theme::palette().background);
        FillRect(buffer, &area, background);
        DeleteObject(background);
        SetBkMode(buffer, TRANSPARENT as _);
        SetTextColor(buffer, crate::visual::theme::palette().text);
        state.hits.clear();
        let unit = |value: i32| value * state.dpi / 96;
        let height = area.bottom;
        let small = state.settings.size == TaskbarSize::Small;
        let start_width = state.settings.size.start_pixels(state.dpi);
        let item_width = |content| state.settings.size.item_pixels(content, state.dpi);
        let padding = unit(state.settings.size.padding());
        let tray_width = |content| crate::settings::tray_pixels(content, state.dpi);
        let tray_padding = unit(crate::settings::TRAY_PADDING);
        let tray_pitch = tray_width(unit(16));
        let glyph_width = unit(if small { 16 } else { 19 });
        let glyph_pitch = item_width(glyph_width);
        let system_pitch = tray_width(glyph_width);
        let pinned = state.tray_snapshot.pinned();
        let pinned_limit = ((area.right / 4) / tray_pitch).max(0) as usize;
        let pinned: Vec<_> = pinned.into_iter().take(pinned_limit).collect();
        state.tray_pinned = pinned
            .iter()
            .map(|index| state.tray_snapshot.items[*index].key.clone())
            .collect();
        let foreground = GetAncestor(effective_foreground(), GA_ROOTOWNER);
        let clock_provider = state.widgets.iter().find(|widget| {
            widget.slot == Slot::Clock
                && state.settings.clock_extension != "system"
                && (state.settings.clock_extension.is_empty()
                    || crate::plugin_system::model::canonical_id(&state.settings.clock_extension)
                        == widget.id)
        });
        let (clock_text, clock_tip) = clock_provider
            .map(|widget| {
                state.clock_feed.render(
                    &widget.id,
                    &widget.text,
                    &state.time,
                    &state.settings.clock_format,
                )
            })
            .unwrap_or_else(|| (state.time.clone(), String::new()));
        let mut clock_measure: SIZE = zeroed();
        let clock_wide = wide(&clock_text);
        GetTextExtentPoint32W(
            buffer,
            clock_wide.as_ptr(),
            (clock_wide.len() - 1) as _,
            &mut clock_measure,
        );
        let clock_width = tray_width(clock_measure.cx)
            .max(unit(40))
            .min(area.right / 2);
        if state.gpu_meter.is_none()
            && !state.gpu_meter_failed
            && state
                .widgets
                .iter()
                .any(|w| w.slot == Slot::Tray && state.services.page(&w.id).is_some())
        {
            match crate::usage_meter::compositor::Compositor::new(state.hwnd) {
                Ok(compositor) => {
                    eprintln!("[usage-meter] DirectComposition hardware timeline enabled");
                    state.gpu_meter = Some(compositor);
                }
                Err(e) => {
                    eprintln!("[usage-meter] compositor unavailable: {e}");
                    state.gpu_meter_failed = true;
                }
            }
        }
        let gpu_enabled = state.gpu_meter.is_some();
        let drag_active = state.drag.as_ref().is_some_and(|d| d.active);
        if (drag_active || state.drag_settling)
            && state.drag_layer.is_none()
            && !state.drag_layer_failed
        {
            match crate::task_drag::Layer::new(state.hwnd, state.drag_seed.clone()) {
                Ok(layer) => state.drag_layer = Some(layer),
                Err(error) => {
                    state.drag_layer_failed = true;
                    eprintln!("[drag] compositor unavailable: {error}");
                }
            }
        }
        let mut gpu_meters = Vec::new();
        let mut button = |left: i32,
                          width: i32,
                          text: &str,
                          icon: ButtonIcon<'_>,
                          tooltip: &str,
                          action: Action| {
            let (item_left, item_right) = crate::settings::item_edges(left, width);
            let padding = match &action {
                Action::TrayIcon(_) | Action::TrayPanel | Action::Clock | Action::Notifications => {
                    tray_padding
                }
                Action::Plugin(id, _)
                    if state
                        .widgets
                        .iter()
                        .any(|w| &w.id == id && matches!(w.slot, Slot::Tray | Slot::Clock)) =>
                {
                    tray_padding
                }
                _ => padding,
            };
            let rect = RECT {
                left: item_left,
                top: unit(4),
                right: item_right,
                bottom: height - unit(4),
            };
            if rect.right <= rect.left {
                return;
            }
            let group = if let Action::Task(id) = &action {
                state.tasks.iter().find(|task| task.id == *id)
            } else {
                None
            };
            let highlighted = group.is_some_and(|task| !task.windows.is_empty() || task.attention);
            if highlighted {
                let active = group.is_some_and(|task| {
                    state.preview_active == Some(task.id)
                        || (!foreground.is_null()
                            && task.windows.iter().any(|window| window.hwnd == foreground))
                });
                task_background(
                    &surface,
                    &rect,
                    state.dpi,
                    active,
                    group.is_some_and(|task| task.attention),
                    (!drag_active && state.hovered == Some(state.hits.len()))
                        || state.drag.as_ref().is_some_and(|drag| {
                            drag.active && matches!(action,Action::Task(id)if id==drag.id)
                        }),
                );
            } else if !drag_active
                && state.hovered == Some(state.hits.len())
                && !matches!(icon, ButtonIcon::Meter(..))
            {
                if matches!(action, Action::Task(_)) {
                    task_background(&surface, &rect, state.dpi, false, false, true);
                } else {
                    let brush = CreateSolidBrush(visual::theme::palette().hover);
                    FillRect(buffer, &rect, brush);
                    DeleteObject(brush);
                }
            }
            let mut text_rect = RECT {
                left: rect.left + padding,
                right: rect.right - padding,
                ..rect
            };
            match icon {
                ButtonIcon::Windows => {
                    surface.windows_logo(&rect, state.dpi, state.win11, state.settings.size.logo())
                }
                ButtonIcon::Application(handle) => {
                    let size = unit(state.settings.size.icon());
                    let icon_left = if text.is_empty() {
                        (rect.left + rect.right - size) / 2
                    } else {
                        rect.left + padding
                    };
                    DrawIconEx(
                        buffer,
                        icon_left,
                        (height - size) / 2,
                        handle,
                        size,
                        size,
                        0,
                        null_mut(),
                        DI_NORMAL,
                    );
                    if let Some(group) = group
                        && group.windows.len() > 1
                    {
                        crate::task_badge::draw(
                            &surface,
                            &rect,
                            &RECT {
                                left: icon_left,
                                top: (height - size) / 2,
                                right: icon_left + size,
                                bottom: (height + size) / 2,
                            },
                            group.windows.len(),
                            state.dpi,
                            small,
                            state.glass,
                        );
                    }
                    if !text.is_empty() {
                        text_rect.left = icon_left + size + unit(6);
                    }
                }
                ButtonIcon::Tray(handle) => {
                    let size = unit(16);
                    DrawIconEx(
                        buffer,
                        (rect.left + rect.right - size) / 2,
                        (height - size) / 2,
                        handle,
                        size,
                        size,
                        0,
                        null_mut(),
                        DI_NORMAL,
                    );
                }
                ButtonIcon::Glyph(glyph) => {
                    let icon_font = SelectObject(buffer, state.icon_font);
                    let mut glyph_rect = if text.is_empty() {
                        rect
                    } else {
                        RECT {
                            left: rect.left + padding,
                            right: rect.left + padding + glyph_width,
                            ..rect
                        }
                    };
                    DrawTextW(
                        buffer,
                        wide(glyph).as_ptr(),
                        -1,
                        &mut glyph_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX,
                    );
                    SelectObject(buffer, icon_font);
                    if !text.is_empty() {
                        text_rect.left = rect.left + padding + glyph_width + unit(6);
                        text_rect.right = rect.right - padding;
                    }
                }
                ButtonIcon::None => {
                    text_rect.left = rect.left + padding;
                    text_rect.right = rect.right - padding;
                }
                ButtonIcon::Meter(page, previous, phase) => {
                    surface.clear_rounded(&rect, (rect.bottom - rect.top) as f64 / 2.);
                    if gpu_enabled {
                        if let Action::Plugin(id, _) = &action {
                            gpu_meters.push(crate::usage_meter::compositor::spec(
                                id, &rect, page, previous, phase, state.font, padding,
                            ));
                        }
                    } else {
                        crate::usage_meter::draw(&surface, &rect, page.progress);
                        let saved = SaveDC(buffer);
                        let clip = CreateRoundRectRgn(
                            rect.left,
                            rect.top,
                            rect.right,
                            rect.bottom,
                            unit(8),
                            unit(8),
                        );
                        SelectClipRgn(buffer, clip);
                        DeleteObject(clip);
                        IntersectClipRect(buffer, rect.left, rect.top, rect.right, rect.bottom);
                        let displacement = ((rect.bottom - rect.top) as f64 * (1.0 - phase)) as i32;
                        let mut current = RECT {
                            top: rect.top + displacement,
                            bottom: rect.bottom + displacement,
                            left: rect.left + padding,
                            right: rect.right - padding,
                        };
                        crate::usage_meter::label(buffer, &current, &page.text);
                        if let Some(old) = previous {
                            current.top -= rect.bottom - rect.top;
                            current.bottom -= rect.bottom - rect.top;
                            crate::usage_meter::label(buffer, &current, &old.text);
                        }
                        RestoreDC(buffer, saved);
                    }
                }
            }
            let text = wide(text);
            DrawTextW(
                buffer,
                text.as_ptr(),
                -1,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            state.hits.push(Hit {
                rect,
                action,
                tooltip: tooltip.to_owned(),
            });
        };
        button(
            0,
            start_width,
            "",
            ButtonIcon::Windows,
            "开始菜单",
            Action::Start,
        );
        let mut tasks_left = start_width;
        for widget in state.widgets.iter().filter(|w| w.slot == Slot::Leading) {
            let width = item_width(unit(widget.width));
            button(
                tasks_left,
                width,
                &widget.text,
                widget
                    .icon
                    .as_deref()
                    .map(ButtonIcon::Glyph)
                    .unwrap_or(ButtonIcon::None),
                &widget.name,
                Action::Plugin(widget.id.clone(), widget.action.clone()),
            );
            tasks_left += width;
        }
        let notify_width = system_pitch;
        let clock_left = area.right - clock_width - notify_width;
        let right = clock_left - system_pitch - pinned.len() as i32 * tray_pitch;
        state.tray_rect = RECT {
            left: right,
            top: 0,
            right: clock_left,
            bottom: height,
        };
        button(
            clock_left,
            clock_width,
            &clock_text,
            ButtonIcon::None,
            if !clock_tip.is_empty() {
                clock_tip.as_str()
            } else {
                clock_provider
                    .map(|widget| widget.name.as_str())
                    .unwrap_or("日期和时间")
            },
            clock_provider
                .map(|widget| Action::Plugin(widget.id.clone(), widget.action.clone()))
                .unwrap_or(Action::Clock),
        );
        button(
            clock_left + clock_width,
            notify_width,
            "",
            ButtonIcon::Glyph("\u{e7f4}"),
            "通知中心",
            Action::Notifications,
        );
        button(
            right,
            system_pitch,
            "",
            ButtonIcon::Glyph("\u{e70e}"),
            "系统托盘",
            Action::TrayPanel,
        );
        for (position, index) in pinned.iter().enumerate() {
            let item = &state.tray_snapshot.items[*index];
            button(
                right + system_pitch + position as i32 * tray_pitch,
                tray_pitch,
                "",
                ButtonIcon::Tray(item.icon.handle()),
                &item.title,
                Action::TrayIcon(item.key.clone()),
            );
        }
        let mut plugin_right = right;
        for widget in state.widgets.iter().filter(|w| w.slot == Slot::Tray) {
            if let Some((page, old, phase)) = state.services.page(&widget.id) {
                let width = tray_width(unit(widget.width));
                plugin_right -= width;
                button(
                    plugin_right,
                    width,
                    "",
                    ButtonIcon::Meter(page, old, phase),
                    &page.tooltip,
                    Action::Plugin(widget.id.clone(), widget.action.clone()),
                );
            }
        }
        for widget in state.widgets.iter().rev() {
            if widget.slot != Slot::Taskbar {
                continue;
            }
            let width = item_width(unit(widget.width));
            if plugin_right - width < unit(220) {
                break;
            }
            plugin_right -= width;
            button(
                plugin_right,
                width,
                &widget.text,
                ButtonIcon::None,
                &widget.text,
                Action::Plugin(widget.id.clone(), widget.action.clone()),
            );
        }
        let preferred: Vec<_> = state
            .tasks
            .iter()
            .map(|task| {
                let text = wide(&task.title);
                let mut size: SIZE = zeroed();
                GetTextExtentPoint32W(buffer, text.as_ptr(), (text.len() - 1) as i32, &mut size);
                item_width(size.cx + unit(state.settings.size.icon() + 6)).clamp(
                    unit(if small { 88 } else { 112 }),
                    unit(if small { 180 } else { 220 }),
                )
            })
            .collect();
        let anchor = state
            .tasks
            .iter()
            .position(|task| task.id == state.anchor)
            .unwrap_or(0);
        let layout = task_layout::arrange_physical(
            &preferred,
            plugin_right - tasks_left,
            state.settings.icons_only,
            anchor,
            state.settings.size.slot_pixels(state.dpi),
            glyph_pitch,
        );
        let mut left = tasks_left;
        if !layout.leading.is_empty() {
            let hidden = state.tasks[layout.leading.clone()]
                .iter()
                .map(|task| task.id)
                .collect();
            button(
                left,
                layout.overflow_width,
                "",
                ButtonIcon::Glyph("\u{e76b}"),
                &format!("前面的应用（{}）", layout.leading.len()),
                Action::Overflow(hidden),
            );
            left += layout.overflow_width;
        }
        for (position, index) in layout.visible.clone().enumerate() {
            let task = &state.tasks[index];
            let width = layout.widths[position];
            button(
                left,
                width,
                if layout.mode == Mode::Labels {
                    &task.title
                } else {
                    ""
                },
                ButtonIcon::Application(task.icon.handle),
                &if task.windows.len() > 1 {
                    task.windows
                        .iter()
                        .map(|window| window.title.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    task.title.clone()
                },
                Action::Task(task.id),
            );
            left += width;
        }
        if !layout.trailing.is_empty() {
            let hidden = state.tasks[layout.trailing.clone()]
                .iter()
                .map(|task| task.id)
                .collect();
            button(
                left,
                layout.overflow_width,
                "",
                ButtonIcon::Glyph("\u{e76c}"),
                &format!("后面的应用（{}）", layout.trailing.len()),
                Action::Overflow(hidden),
            );
        }
        SelectObject(buffer, old_font);
        if let Some(layer) = state.drag_layer.as_mut() {
            let rectangles: Vec<_> = state
                .hits
                .iter()
                .filter_map(|hit| {
                    if let Action::Task(id) = hit.action {
                        Some((id, hit.rect))
                    } else {
                        None
                    }
                })
                .collect();
            let shots = crate::task_drag::capture(&surface, area.right, &rectangles, state.glass);
            let dragged = state
                .drag
                .as_ref()
                .filter(|d| d.active)
                .map(|d| (d.id, d.pointer.x - d.grab_offset));
            match layer.sync(shots, dragged, height) {
                Ok(()) => {
                    let brush = CreateSolidBrush(visual::theme::palette().background);
                    if let (Some(left), Some(right)) = (
                        rectangles.iter().map(|(_, r)| r.left).min(),
                        rectangles.iter().map(|(_, r)| r.right).max(),
                    ) {
                        FillRect(
                            buffer,
                            &RECT {
                                left,
                                top: 0,
                                right,
                                bottom: height,
                            },
                            brush,
                        );
                    }
                    DeleteObject(brush);
                }
                Err(error) => {
                    eprintln!("[drag] compose: {error}");
                    state.drag_layer = None;
                    state.drag_layer_failed = true;
                }
            }
        }
        if let Some(path) = state.export.take()
            && let Err(error) = surface.save_bmp(&path)
        {
            eprintln!("[taskbar-smoke] {error}");
        }
        surface.present(state.hwnd, state.glass);
        if let Some(compositor) = state.gpu_meter.as_mut()
            && let Err(e) = compositor.sync(gpu_meters)
        {
            eprintln!("[usage-meter] composition failed: {e}");
            state.gpu_meter = None;
            state.gpu_meter_failed = true;
            InvalidateRect(state.hwnd, null(), 0);
        }
        if let Some(tooltips) = &mut state.tooltips {
            tooltips.sync(
                state
                    .hits
                    .iter()
                    .map(|hit| Tip {
                        bounds: (hit.rect.left, hit.rect.top, hit.rect.right, hit.rect.bottom),
                        text: hit.tooltip.clone(),
                    })
                    .collect(),
            );
        }
        EndPaint(state.hwnd, &ps);
        if let Some(popup) = &mut state.tray_popup {
            popup.update(state.tray_snapshot.clone(), state.tray_pinned.clone());
        }
    }
}

enum ButtonIcon<'a> {
    Meter(
        &'a ultra_bar_plugin_sdk::Page,
        Option<&'a ultra_bar_plugin_sdk::Page>,
        f64,
    ),
    None,
    Windows,
    Glyph(&'a str),
    Application(HICON),
    Tray(HICON),
}

unsafe fn task_background(
    surface: &Surface,
    rect: &RECT,
    dpi: i32,
    active: bool,
    attention: bool,
    hover: bool,
) {
    unsafe {
        crate::task_relief::draw(surface, rect, dpi, active, attention, hover);
    }
}
unsafe fn apply_theme(owner: HWND, rgb: u32) {
    unsafe {
        visual::theme::set_accent(rgb);
        InvalidateRect(owner, null(), 0);
        unsafe extern "system" fn notify(hwnd: HWND, param: LPARAM) -> BOOL {
            unsafe {
                let data = &*(param as *const (HWND, u32));
                if hwnd != data.0 && GetAncestor(hwnd, GA_ROOTOWNER) == data.0 {
                    SendNotifyMessageW(hwnd, visual::theme::changed_message(), data.1 as usize, 0);
                }
                1
            }
        }
        let data = (owner, rgb);
        EnumWindows(Some(notify), (&data as *const (HWND, u32)) as isize);
    }
}

unsafe fn smoke_taskbar(state: &mut State, directory: &std::path::Path) -> Result<()> {
    unsafe {
        std::fs::create_dir_all(directory)?;
        state.preview_active = Some(2);
        let exe = std::env::current_exe()?.to_string_lossy().into_owned();
        state.tasks.clear();
        for index in 0..8 {
            let pin = crate::task_apps::Shortcut {
                launch: exe.clone(),
                executable: exe.clone(),
                name: if index == 0 {
                    "聊天".into()
                } else {
                    format!("应用 {}", index + 1)
                },
            };
            let count = if index == 0 {
                1
            } else if index == 1 {
                3
            } else if index == 2 {
                0
            } else {
                1
            };
            let members = (0..count)
                .map(|number| TaskWindow {
                    hwnd: if index == 0 { state.hwnd } else { null_mut() },
                    title: format!(
                        "文档 {} — 一个用于检验标题省略和完整工具提示的窗口",
                        number + 1
                    ),
                    pid: std::process::id(),
                    executable: exe.clone(),
                    app_id: None,
                })
                .collect();
            state.tasks.push(Task {
                id: index + 1,
                key: if index == 0 {
                    crate::task_apps::identity(&exe, state.hwnd as usize)
                } else {
                    format!("fixture-{index}")
                },
                executable: exe.clone(),
                hwnd: if index == 0 { state.hwnd } else { null_mut() },
                windows: members,
                pin: if index == 2 { Some(pin.clone()) } else { None },
                title: if index == 1 {
                    "文档".into()
                } else {
                    pin.name.clone()
                },
                icon: crate::task_apps::icon(&pin, state.dpi, 32),
                attention: false,
            });
        }
        state.time = "11-10 13:18".into();
        state.anchor = 5;
        SendMessageW(
            state.hwnd,
            state.shell_message,
            HSHELL_FLASH as usize,
            state.hwnd as isize,
        );
        anyhow::ensure!(state.tasks[0].attention, "系统提醒事件未标记应用");
        let mut files = vec![];
        for (name, width, icons_only, size) in [
            ("labels", 1600, false, TaskbarSize::Normal),
            ("icons", 1000, true, TaskbarSize::Normal),
            ("overflow", 400, true, TaskbarSize::Normal),
            ("small", 1000, true, TaskbarSize::Small),
        ] {
            state.settings.icons_only = icons_only;
            state.settings.size = size;
            set_font(state);
            for task in &mut state.tasks {
                task.icon = AppIcon::from_file(&exe, state.dpi, size.icon());
            }
            SetWindowPos(
                state.hwnd,
                null_mut(),
                0,
                0,
                width * state.dpi / 96,
                size.height() * state.dpi / 96,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            let path = directory.join(format!("taskbar-{name}.bmp"));
            state.export = Some(path.clone());
            paint(state);
            anyhow::ensure!(path.exists(), "任务栏绘制失败");
            if name == "overflow" {
                anyhow::ensure!(
                    state
                        .hits
                        .iter()
                        .any(|hit| matches!(hit.action, Action::Overflow(_))),
                    "收纳按钮缺失"
                );
            }
            files.push(path.to_string_lossy().into_owned());
        }
        if std::env::args().any(|a| a == "--drag-smoke") {
            smoke_task_drag(state, directory)?;
        }
        SendMessageW(
            state.hwnd,
            state.shell_message,
            HSHELL_WINDOWACTIVATED as usize,
            state.hwnd as isize,
        );
        anyhow::ensure!(!state.tasks[0].attention, "激活后提醒未清除");
        state.preview_active = None;
        let native: Vec<_> = crate::task_apps::system_pins()
            .iter()
            .filter_map(|path| crate::task_apps::from_file(path).ok())
            .collect();
        let native_count = native.len();
        if !native.is_empty() {
            state.tasks = native
                .into_iter()
                .enumerate()
                .map(|(index, pin)| Task {
                    id: index + 1,
                    key: crate::task_apps::identity(&pin.executable, 0),
                    executable: pin.executable.clone(),
                    hwnd: null_mut(),
                    windows: vec![],
                    title: pin.name.clone(),
                    icon: crate::task_apps::icon(&pin, state.dpi, 32),
                    pin: Some(pin),
                    attention: false,
                })
                .collect();
            state.settings.size = TaskbarSize::Normal;
            state.settings.icons_only = true;
            set_font(state);
            SetWindowPos(
                state.hwnd,
                null_mut(),
                0,
                0,
                1600 * state.dpi / 96,
                40 * state.dpi / 96,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            let path = directory.join("taskbar-system-pins.bmp");
            state.export = Some(path.clone());
            paint(state);
            anyhow::ensure!(path.exists(), "系统固定图标绘制失败");
            files.push(path.to_string_lossy().into_owned());
        }
        std::fs::write(
            directory.join("result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"rendered":files,"native_pins":native_count,"shell_flash":"passed","activation_clears_attention":"passed","global_input":false}),
            )?,
        )?;
        Ok(())
    }
}

unsafe fn smoke_task_drag(state: &mut State, directory: &std::path::Path) -> Result<()> {
    unsafe {
        const LEFT_BUTTON_HELD: usize = 1;
        let original: Vec<_> = state.tasks.iter().map(|t| t.id).collect();
        let style = GetWindowLongPtrW(state.hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            state.hwnd,
            GWL_EXSTYLE,
            (style & !(WS_EX_APPWINDOW as isize))
                | WS_EX_TOOLWINDOW as isize
                | WS_EX_NOACTIVATE as isize,
        );
        // Exercise both composition layers together: usage widgets stay in the upper layer.
        state.gpu_meter = Some(crate::usage_meter::compositor::Compositor::new(state.hwnd)?);
        let message = |x: i32, y: i32| ((x as u16 as u32) | ((y as u16 as u32) << 16)) as isize;
        let mut reports = vec![];
        for (name, size, icons_only) in [
            ("normal", TaskbarSize::Normal, true),
            ("small", TaskbarSize::Small, true),
            ("labels", TaskbarSize::Normal, false),
        ] {
            state
                .tasks
                .sort_by_key(|t| original.iter().position(|id| *id == t.id).unwrap());
            state.settings.size = size;
            state.settings.icons_only = icons_only;
            set_font(state);
            SetWindowPos(
                state.hwnd,
                HWND_TOPMOST,
                100,
                100,
                1200,
                size.height(),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            paint(state);
            let hit = |i: usize| {
                state
                    .hits
                    .iter()
                    .filter(|h| matches!(h.action, Action::Task(_)))
                    .nth(i)
                    .map(|h| h.rect)
                    .unwrap()
            };
            let first = hit(0);
            let target = hit(2);
            let y = (first.top + first.bottom) / 2;
            let x = (first.left + first.right) / 2;
            let end = (target.left + target.right) / 2 + 2;
            let dragged = state.tasks[0].id;
            SendMessageW(state.hwnd, WM_LBUTTONDOWN, 0, message(x, y));
            SendMessageW(state.hwnd, WM_MOUSEMOVE, LEFT_BUTTON_HELD, message(end, y));
            paint(state);
            anyhow::ensure!(
                state.drag.as_ref().is_some_and(|d| d.active),
                "Drag threshold was not crossed"
            );
            anyhow::ensure!(
                state.tasks[2].id == dragged,
                "Live drag insertion did not move the source task"
            );
            let moving = state
                .drag_layer
                .as_ref()
                .context("GPU drag layer missing")?
                .moving_neighbours(dragged);
            anyhow::ensure!(
                moving >= 2,
                "Neighbor tasks did not animate out of the insertion slot"
            );
            let capture = |stage: &str| -> Result<Vec<u32>> {
                DwmFlush();
                let mut bounds: RECT = zeroed();
                GetWindowRect(state.hwnd, &mut bounds);
                let surface = Surface::new(bounds.right - bounds.left, bounds.bottom - bounds.top)
                    .context("Cannot capture drag fixture")?;
                let dc = GetDC(null_mut());
                let copied = BitBlt(
                    surface.dc,
                    0,
                    0,
                    bounds.right - bounds.left,
                    bounds.bottom - bounds.top,
                    dc,
                    bounds.left,
                    bounds.top,
                    SRCCOPY | CAPTUREBLT,
                );
                ReleaseDC(null_mut(), dc);
                anyhow::ensure!(copied != 0, "Drag capture failed");
                surface.save_bmp(&directory.join(format!("drag-{name}-{stage}.bmp")))?;
                Ok(surface.copy_pixels())
            };
            let before = match capture("initial") {
                Ok(image) => Some(image),
                Err(error) => {
                    eprintln!(
                        "[drag-smoke] desktop capture unavailable: {error}; checking the GPU scene and reorder logic"
                    );
                    None
                }
            };
            let commits = state.drag_layer.as_ref().unwrap().commits;
            // DWM moves neighbors while this UI thread issues no paints or commits.
            std::thread::sleep(std::time::Duration::from_millis(90));
            let after = if before.is_some() {
                capture("shifted").ok()
            } else {
                None
            };
            let changed = before
                .as_ref()
                .zip(after.as_ref())
                .map(|(before, after)| before.iter().zip(after).filter(|(a, b)| a != b).count());
            if let Some(changed) = changed {
                anyhow::ensure!(
                    changed > 20,
                    "Neighbor animation did not change the composed frame"
                );
            }
            anyhow::ensure!(
                commits == state.drag_layer.as_ref().unwrap().commits,
                "Motion required application frame commits"
            );
            SendMessageW(state.hwnd, WM_LBUTTONUP, 0, message(end, y));
            paint(state);
            anyhow::ensure!(
                state.drag_settling,
                "Drop did not settle into its target slot"
            );
            std::thread::sleep(std::time::Duration::from_millis(190));
            SendMessageW(state.hwnd, WM_TIMER, 5, 0);
            paint(state);
            anyhow::ensure!(
                state.drag_layer.is_none() && !state.drag_settling,
                "Drag animation resources did not stop"
            );
            // A subsequent cancelled reorder restores the exact prior order and saved pins.
            let saved_order: Vec<_> = state.tasks.iter().map(|t| t.id).collect();
            let pins_path = crate::data_paths::root()?.join("taskbar-apps.json");
            let saved_pins = std::fs::read(&pins_path).ok();
            let first = state
                .hits
                .iter()
                .find(|h| matches!(h.action, Action::Task(_)))
                .unwrap()
                .rect;
            SendMessageW(
                state.hwnd,
                WM_LBUTTONDOWN,
                0,
                message((first.left + first.right) / 2, y),
            );
            SendMessageW(state.hwnd, WM_MOUSEMOVE, LEFT_BUTTON_HELD, message(end, y));
            paint(state);
            SendMessageW(state.hwnd, WM_CANCELMODE, 0, 0);
            paint(state);
            anyhow::ensure!(
                state.tasks.iter().map(|t| t.id).collect::<Vec<_>>() == saved_order,
                "Cancelled reorder changed task order"
            );
            anyhow::ensure!(
                std::fs::read(&pins_path).ok() == saved_pins,
                "Cancelled reorder overwrote pinned order"
            );
            std::thread::sleep(std::time::Duration::from_millis(190));
            SendMessageW(state.hwnd, WM_TIMER, 5, 0);
            paint(state);
            reports.push(serde_json::json!({"mode":name,"moving_neighbors":moving,"changed_pixels":changed,"visual_capture_available":changed.is_some(),"gpu_timeline_without_frame_commits":true,"cancel_restores_order":true,"settled_resources_released":true}));
        }
        for leading in [true, false] {
            state
                .tasks
                .sort_by_key(|t| original.iter().position(|id| *id == t.id).unwrap());
            state.settings.size = TaskbarSize::Normal;
            state.settings.icons_only = true;
            state.anchor = original[3];
            set_font(state);
            SetWindowPos(
                state.hwnd,
                HWND_TOPMOST,
                100,
                100,
                400,
                40,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            paint(state);
            let first = state
                .hits
                .iter()
                .find(|h| matches!(h.action, Action::Task(_)))
                .unwrap()
                .rect;
            let overflows: Vec<_> = state
                .hits
                .iter()
                .filter(|h| matches!(h.action, Action::Overflow(_)))
                .map(|h| h.rect)
                .collect();
            anyhow::ensure!(overflows.len() == 2, "Two-ended overflow fixture missing");
            let edge = if leading { overflows[0] } else { overflows[1] };
            let (x, y) = ((first.left + first.right) / 2, 20);
            let target = (edge.left + edge.right) / 2;
            SendMessageW(state.hwnd, WM_LBUTTONDOWN, 0, message(x, y));
            SendMessageW(
                state.hwnd,
                WM_MOUSEMOVE,
                LEFT_BUTTON_HELD,
                message(target, y),
            );
            paint(state);
            let dragged = state.drag.as_ref().unwrap().id;
            state.drag.as_mut().unwrap().last_edge =
                Instant::now() - std::time::Duration::from_millis(450);
            SendMessageW(
                state.hwnd,
                WM_MOUSEMOVE,
                LEFT_BUTTON_HELD,
                message(target, y),
            );
            paint(state);
            anyhow::ensure!(
                state.tasks.iter().map(|t| t.id).collect::<Vec<_>>() != original,
                "Overflow edge did not advance the reorder"
            );
            anyhow::ensure!(
                state
                    .hits
                    .iter()
                    .any(|h| matches!(h.action,Action::Task(id)if id==dragged)),
                "Dragged task was lost in overflow"
            );
            SendMessageW(state.hwnd, WM_CANCELMODE, 0, 0);
            paint(state);
            anyhow::ensure!(
                state.tasks.iter().map(|t| t.id).collect::<Vec<_>>() == original,
                "Overflow cancel did not restore order"
            );
            std::thread::sleep(std::time::Duration::from_millis(190));
            SendMessageW(state.hwnd, WM_TIMER, 5, 0);
            paint(state);
            reports.push(serde_json::json!({"mode":if leading{"overflow-leading"}else{"overflow-trailing"},"edge_reorder_keeps_dragged_task_visible":true,"cancel_restores_order":true}));
        }
        ShowWindow(state.hwnd, SW_HIDE);
        state
            .tasks
            .sort_by_key(|t| original.iter().position(|id| *id == t.id).unwrap());
        std::fs::write(
            directory.join("drag-checks.json"),
            serde_json::to_vec_pretty(&reports)?,
        )?;
        Ok(())
    }
}

unsafe fn apply_task_click(state: &mut State, hwnd: HWND, click: TaskClick) {
    unsafe {
        if IsWindow(hwnd) == 0 {
            return;
        }
        if let Some(task) = state
            .tasks
            .iter_mut()
            .find(|task| task.windows.iter().any(|window| window.hwnd == hwnd))
        {
            state.anchor = task.id;
            state.attention.retain(|_, app| app != &task.key);
            task.attention = false;
        }
        state.attention.remove(&(hwnd as usize));
        InvalidateRect(state.hwnd, null(), 0);
        match click.release(IsIconic(hwnd) != 0) {
            Operation::Minimize => {
                let posted = PostMessageW(hwnd, WM_SYSCOMMAND, SC_MINIMIZE as _, 0);
                eprintln!(
                    "[task-click] minimize target={:#x} foreground={:#x} posted={} error={}",
                    hwnd as usize,
                    GetForegroundWindow() as usize,
                    posted,
                    if posted == 0 { GetLastError() } else { 0 }
                );
                if posted == 0 {
                    ShowWindowAsync(hwnd, SW_MINIMIZE);
                }
                if GetAncestor(
                    LAST_ACTIVE_WINDOW.load(Ordering::Relaxed) as HWND,
                    GA_ROOTOWNER,
                ) == GetAncestor(hwnd, GA_ROOTOWNER)
                {
                    LAST_ACTIVE_WINDOW.store(0, Ordering::Relaxed);
                }
                state.pending_minimize = Some((hwnd, false));
                SetTimer(state.hwnd, 3, 150, None);
            }
            Operation::Activate => {
                if state
                    .pending_minimize
                    .is_some_and(|(target, _)| target == hwnd)
                {
                    state.pending_minimize = None;
                    KillTimer(state.hwnd, 3);
                }
                if IsIconic(hwnd) != 0 {
                    ShowWindowAsync(hwnd, SW_RESTORE);
                }
                let popup = GetLastActivePopup(hwnd);
                let target = if !popup.is_null() && IsWindowVisible(popup) != 0 {
                    popup
                } else {
                    hwnd
                };
                let activated = SetForegroundWindow(target);
                if activated != 0 {
                    LAST_ACTIVE_WINDOW.store(target as usize, Ordering::Relaxed);
                }
                eprintln!(
                    "[task-click] activate target={:#x} popup={:#x} foreground={:#x} accepted={}",
                    hwnd as usize,
                    target as usize,
                    GetForegroundWindow() as usize,
                    activated
                );
            }
        }
    }
}

unsafe fn execute(state: &mut State, action: Action) {
    unsafe {
        let chooser = match &action {
            Action::Overflow(_) => true,
            Action::Task(id) => state
                .tasks
                .iter()
                .any(|task| task.id == *id && task.windows.len() > 1),
            _ => false,
        };
        if !chooser && let Some(popup) = &mut state.task_popup {
            popup.hide();
        }
        match action {
            Action::TrayPanel => {
                if let Some(popup) = &mut state.tray_popup {
                    popup.update(state.tray_snapshot.clone(), state.tray_pinned.clone());
                    popup.toggle(state.dpi);
                }
            }
            Action::TrayIcon(key) => {
                if let Some(host) = &state.tray_host {
                    host.send(tray::Command::Click(key, tray::Click::Left));
                }
            }
            Action::Start => shortcut(&[VK_CONTROL, VK_ESCAPE]),
            Action::Notifications => shortcut(&[
                VK_LWIN,
                if state.win11 {
                    b'N' as u16
                } else {
                    b'A' as u16
                },
            ]),
            Action::Clock => open_settings("ms-settings:dateandtime"),
            Action::Task(id) => {
                if let Some(task) = state.tasks.iter().find(|task| task.id == id) {
                    if task.windows.is_empty() {
                        if let Some(pin) = &task.pin
                            && let Err(error) = crate::task_apps::launch(pin)
                        {
                            app_error(state.hwnd, &error);
                        }
                    } else if task.windows.len() == 1 {
                        let hwnd = task.hwnd;
                        let click = TaskClick::begin(
                            GetAncestor(hwnd, GA_ROOTOWNER) as usize,
                            GetAncestor(effective_foreground(), GA_ROOTOWNER) as usize,
                            IsIconic(hwnd) != 0,
                        );
                        apply_task_click(state, hwnd, click);
                    } else {
                        let foreground = GetAncestor(effective_foreground(), GA_ROOTOWNER);
                        let larger = popup_icon(state, task);
                        let icon = larger
                            .as_ref()
                            .map(|icon| icon.handle)
                            .unwrap_or(task.icon.handle);
                        let items = task
                            .windows
                            .iter()
                            .map(|window| {
                                crate::task_popup::Item::new(
                                    crate::task_popup::Target::Window {
                                        hwnd: window.hwnd as usize,
                                        pid: window.pid,
                                    },
                                    window.title.clone(),
                                    icon,
                                    GetAncestor(window.hwnd, GA_ROOTOWNER) == foreground,
                                )
                            })
                            .collect();
                        let title = task.title.clone();
                        let anchor = popup_anchor(state, &Action::Task(id));
                        show_task_popup(state, format!("group:{id}"), title, items, anchor);
                    }
                }
            }
            Action::Plugin(id, action) => {
                if action == "panel"
                    && let Some(widget) = state.widgets.iter().find(|widget| widget.id == id)
                    && let Err(error) = if widget.service {
                        state.services.show(&widget.id)
                    } else {
                        crate::plugin_panels::launch(widget, state.hwnd, true)
                    }
                {
                    MessageBoxW(
                        state.hwnd,
                        wide(&format!("{error:#}")).as_ptr(),
                        wide(&widget.name).as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
                if let Some(host) = &state.host {
                    host.click(&id);
                }
                match action.as_str() {
                    "start" => shortcut(&[VK_CONTROL, VK_ESCAPE]),
                    "notifications" => shortcut(&[
                        VK_LWIN,
                        if state.win11 {
                            b'N' as u16
                        } else {
                            b'A' as u16
                        },
                    ]),
                    "settings" => open_settings("ms-settings:"),
                    _ => {}
                }
            }
            Action::Overflow(hidden) => {
                let foreground = GetAncestor(effective_foreground(), GA_ROOTOWNER);
                let items = hidden
                    .iter()
                    .filter_map(|id| state.tasks.iter().find(|task| task.id == *id))
                    .map(|task| {
                        let larger = popup_icon(state, task);
                        let icon = larger
                            .as_ref()
                            .map(|icon| icon.handle)
                            .unwrap_or(task.icon.handle);
                        crate::task_popup::Item::new(
                            crate::task_popup::Target::Task(task.id),
                            task.title.clone(),
                            icon,
                            task.windows
                                .iter()
                                .any(|window| GetAncestor(window.hwnd, GA_ROOTOWNER) == foreground),
                        )
                    })
                    .collect();
                let anchor = popup_anchor(state, &Action::Overflow(hidden.clone()));
                show_task_popup(
                    state,
                    format!("overflow:{hidden:?}"),
                    "收纳的应用".into(),
                    items,
                    anchor,
                );
            }
            Action::Exit => {
                DestroyWindow(state.hwnd);
            }
        }
    }
}

unsafe fn open_settings(uri: &str) {
    unsafe {
        ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            wide(uri).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

unsafe fn shortcut(keys: &[u16]) {
    unsafe {
        let mut inputs = Vec::new();
        for key in keys {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: *key,
                        ..zeroed()
                    },
                },
            });
        }
        for key in keys.iter().rev() {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: *key,
                        dwFlags: KEYEVENTF_KEYUP,
                        ..zeroed()
                    },
                },
            });
        }
        if SendInput(inputs.len() as _, inputs.as_ptr(), size_of::<INPUT>() as _) == 0 {
            eprintln!("[shell] system shortcut was rejected");
        }
    }
}

unsafe fn is_win11() -> bool {
    unsafe {
        let module = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        let Some(address) = GetProcAddress(module, c"RtlGetVersion".as_ptr().cast()) else {
            return false;
        };
        let get_version: unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32 =
            std::mem::transmute(address);
        let mut version: OSVERSIONINFOW = zeroed();
        version.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as _;
        get_version(&mut version) == 0 && version.dwBuildNumber >= 22000
    }
}

pub fn restore_watch(args: &[String]) -> Result<()> {
    let parent: u32 = args.first().context("missing parent pid")?.parse()?;
    let handles = args.get(1).context("missing taskbar handles")?;
    let original: u32 = args
        .get(2)
        .context("missing original taskbar state")?
        .parse()?;
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, parent);
        if !process.is_null() {
            WaitForSingleObject(process, INFINITE);
            CloseHandle(process);
        }
        let handles = handles
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<usize>().map(|h| h as HWND))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        restore_native(&handles, original);
    }
    Ok(())
}

unsafe fn restore_native(handles: &[HWND], original: u32) {
    unsafe {
        let native = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        if !native.is_null() {
            let mut data: APPBARDATA = zeroed();
            data.cbSize = size_of::<APPBARDATA>() as _;
            data.hWnd = native;
            data.lParam = original as _;
            SHAppBarMessage(ABM_SETSTATE, &mut data);
        }
        for hwnd in handles {
            let hwnd = *hwnd;
            let mut class = [0u16; 128];
            let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as _);
            let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
            if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
                ShowWindow(hwnd, SW_SHOWNA);
            }
        }
    }
}
