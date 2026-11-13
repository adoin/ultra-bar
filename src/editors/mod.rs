//! The host's sole provider for plugin editing. Display-only surfaces stay extensible.
mod controls;
use crate::visual::{Surface, text::draw_text as DrawTextW, theme, wide};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use std::{
    collections::VecDeque,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use ultra_bar_plugin_sdk::editors::*;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{
        Com::CoTaskMemFree, DataExchange::COPYDATASTRUCT, LibraryLoader::*,
        SystemInformation::GetLocalTime,
    },
    UI::{
        Controls::Dialogs::*, Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};
pub const UPDATE: u32 = WM_APP + 30;
pub const FAILED: u32 = WM_APP + 31;
type LocalSave = Box<dyn Fn(&Values) -> Result<()>>;
struct Binding {
    field: Field,
    inputs: Vec<HWND>,
    browse: HWND,
    bounds: RECT,
    visible: bool,
}
struct View {
    hwnd: HWND,
    host: HWND,
    caller: HWND,
    disabled_caller: bool,
    plugin: String,
    form: Form,
    bindings: Vec<Binding>,
    font: HFONT,
    icons: HFONT,
    dpi: i32,
    scroll: i32,
    total: i32,
    error: String,
    saving: bool,
    mount: bool,
    loading: bool,
    local_save: Option<LocalSave>,
}
pub struct Window(Box<View>);
struct ProviderState {
    manager: Manager,
    widgets: Vec<crate::plugins::Widget>,
}
pub struct TestProvider {
    state: Box<ProviderState>,
    pub hwnd: HWND,
}
impl Drop for TestProvider {
    fn drop(&mut self) {
        unsafe {
            self.state.manager.close_mounts();
            DestroyWindow(self.hwnd);
        }
    }
}
impl TestProvider {
    pub unsafe fn new(widgets: &[crate::plugins::Widget]) -> Result<Self> {
        unsafe {
            unsafe extern "system" fn provider(hwnd: HWND, m: u32, w: usize, l: isize) -> LRESULT {
                unsafe {
                    if m == WM_NCCREATE {
                        SetWindowLongPtrW(
                            hwnd,
                            GWLP_USERDATA,
                            (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
                        );
                    }
                    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ProviderState;
                    if p.is_null() {
                        return DefWindowProcW(hwnd, m, w, l);
                    }
                    let state = &mut *p;
                    if m == WM_COPYDATA && l != 0 {
                        if state.manager.receive(
                            w as HWND,
                            &*(l as *const COPYDATASTRUCT),
                            &state.widgets,
                        ) {
                            PostMessageW(hwnd, UPDATE, 0, 0);
                            return 1;
                        }
                        return 0;
                    }
                    if m == UPDATE {
                        state.manager.apply(hwnd);
                        return 0;
                    }
                    DefWindowProcW(hwnd, m, w, l)
                }
            }
            let mut state = Box::new(ProviderState {
                manager: Manager::default(),
                widgets: widgets.to_vec(),
            });
            let class = wide("UltraBar.Editor.TestProvider");
            let instance = GetModuleHandleW(null());
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(provider),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("").as_ptr(),
                WS_POPUP,
                -32000,
                -32000,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                (&mut *state as *mut ProviderState).cast(),
            );
            ensure!(!hwnd.is_null(), "无法创建宿主验证端点");
            Ok(Self { state, hwnd })
        }
    }
    pub unsafe fn pump(&self) {
        unsafe {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    pub unsafe fn verify_mount(&mut self, directory: &std::path::Path) -> Result<()> {
        unsafe {
            self.pump();
            let view = &self
                .state
                .manager
                .windows
                .iter()
                .find(|w| w.0.mount)
                .context("搜索插件没有请求宿主编辑器")?
                .0;
            let input = view.bindings[0].inputs[0];
            let mut input_pid = 0;
            let mut plugin_pid = 0;
            GetWindowThreadProcessId(input, &mut input_pid);
            GetWindowThreadProcessId(view.caller, &mut plugin_pid);
            ensure!(
                input_pid == windows_sys::Win32::System::Threading::GetCurrentProcessId()
                    && input_pid != plugin_pid,
                "搜索编辑器仍在插件进程中"
            );
            SetWindowPos(
                view.caller,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetWindowTextW(input, wide("chat").as_ptr());
            ensure!(text(input) == "chat", "宿主搜索框未接收输入值");
            let mut rect: RECT = zeroed();
            GetWindowRect(input, &mut rect);
            let surface = Surface::new(rect.right - rect.left, rect.bottom - rect.top)
                .context("无法捕获宿主搜索框")?;
            SendMessageW(
                input,
                WM_PRINT,
                surface.dc as usize,
                (PRF_CLIENT | PRF_NONCLIENT) as isize,
            );
            surface.save_bmp(&directory.join("host-search-input.bmp"))?;
            std::fs::write(
                directory.join("host-search-result.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"renderer_pid":input_pid,"plugin_pid":plugin_pid,"host_owned":true,"visible":IsWindowVisible(input)!=0,"chars":GetWindowTextLengthW(input),"global_input":false}),
                )?,
            )?;
            Ok(())
        }
    }
    pub fn close_mounts(&mut self) {
        self.state.manager.close_mounts();
    }
}
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
            ultra_bar_native_ui::fonts::delete_object(self.font);
            ultra_bar_native_ui::fonts::delete_object(self.icons);
        }
    }
}
#[derive(Default)]
pub struct Manager {
    pending: VecDeque<(HWND, Request)>,
    windows: Vec<Window>,
    settings: Vec<String>,
    local_pending: VecDeque<(Form, LocalSave)>,
}
impl Manager {
    /// Internal forms use the same controls and validation as plugin editors.
    pub fn open_local(
        &mut self,
        form: Form,
        save: impl Fn(&Values) -> Result<()> + 'static,
    ) -> Result<()> {
        form.validate_schema().map_err(anyhow::Error::msg)?;
        ensure!(self.local_pending.len() < 32, "待编辑项目过多");
        self.local_pending.push_back((form, Box::new(save)));
        Ok(())
    }
    pub unsafe fn delivery_failed(&mut self, caller: HWND, session: u64) {
        unsafe {
            if let Some(window) = self
                .windows
                .iter_mut()
                .find(|w| w.0.caller == caller && w.0.form.session == session)
            {
                window.0.saving = false;
                window.0.error = "插件暂未响应，请稍后重试".into();
                layout(&mut window.0);
                InvalidateRect(window.0.hwnd, null(), 0);
            }
        }
    }

    pub unsafe fn receive(
        &mut self,
        caller: HWND,
        packet: &COPYDATASTRUCT,
        widgets: &[crate::plugins::Widget],
    ) -> bool {
        unsafe {
            if packet.dwData != REQUEST
                || packet.cbData > 65536
                || packet.lpData.is_null()
                || IsWindow(caller) == 0
            {
                return false;
            }
            let bytes =
                std::slice::from_raw_parts(packet.lpData as *const u8, packet.cbData as usize);
            let Ok(request) = serde_json::from_slice::<Request>(bytes) else {
                return false;
            };
            let ongoing = matches!(&request.operation,Operation::Complete{session,..} if self.windows.iter().any(|w|w.0.caller==caller&&w.0.plugin==request.plugin_id&&w.0.form.session==*session));
            let path = crate::task_apps::process_path(caller);
            let managed = widgets.iter().any(|w| {
                w.id == request.plugin_id
                    && w.panel.as_ref().is_some_and(|p| {
                        crate::task_apps::normalize(&p.to_string_lossy())
                            == crate::task_apps::normalize(&path)
                    })
            });
            (ongoing || managed) && self.accept(caller, request)
        }
    }
    pub unsafe fn prune_mounts(&mut self, widgets: &[crate::plugins::Widget]) {
        self.windows.retain(|w| {
            !w.0.mount
                || widgets.iter().any(|widget| {
                    widget.id == w.0.plugin
                        && widget.panel.as_ref().is_some_and(|p| {
                            crate::task_apps::normalize(&p.to_string_lossy())
                                == crate::task_apps::normalize(&crate::task_apps::process_path(
                                    w.0.caller,
                                ))
                        })
                })
        });
    }
    pub fn close_mounts(&mut self) {
        self.windows.retain(|w| !w.0.mount);
    }
    pub fn accept(&mut self, caller: HWND, request: Request) -> bool {
        if request.protocol != PROTOCOL
            || self.pending.len() >= 32
            || (matches!(
                request.operation,
                Operation::Open { .. } | Operation::Mount { .. }
            ) && self.windows.len() >= 128)
        {
            return false;
        }
        if let Operation::Mount { session, field, .. } = &request.operation
            && self.windows.iter().any(|w| {
                w.0.mount
                    && w.0.caller == caller
                    && w.0.plugin == request.plugin_id
                    && w.0.form.session == *session
                    && w.0
                        .form
                        .fields
                        .first()
                        .is_some_and(|old| old.id != field.id || old.kind != field.kind)
            })
        {
            return false;
        }
        match &request.operation {
            Operation::Open { form } if form.validate_schema().is_err() => return false,
            Operation::Mount {
                field,
                x,
                y,
                width,
                height,
                ..
            } if *width < 8
                || *width > 4096
                || *height < 8
                || *height > 2048
                || x.checked_add(*width).is_none()
                || y.checked_add(*height).is_none()
                || (Form {
                    session: 0,
                    title: String::new(),
                    fields: vec![field.clone()],
                    can_delete: false,
                })
                .validate_schema()
                .is_err() =>
            {
                return false;
            }
            _ => {}
        }
        self.pending.push_back((caller, request));
        true
    }
    pub fn busy(&self) -> bool {
        !self.local_pending.is_empty()
            || self
                .windows
                .iter()
                .any(|w| !w.0.mount && unsafe { IsWindow(w.0.hwnd) != 0 })
    }
    pub fn take_settings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.settings)
    }
    pub unsafe fn dialog(&mut self, message: &MSG) -> bool {
        unsafe {
            for window in &mut self.windows {
                let view = &mut window.0;
                if !view.mount
                    && IsWindow(view.hwnd) != 0
                    && GetAncestor(message.hwnd, GA_ROOT) == view.hwnd
                {
                    if message.message == WM_KEYDOWN
                        && message.wParam == VK_TAB as usize
                        && focus_next(view, GetKeyState(VK_SHIFT as i32) < 0)
                    {
                        return true;
                    }
                    return IsDialogMessageW(view.hwnd, message) != 0;
                }
            }
            false
        }
    }
    pub unsafe fn apply(&mut self, host: HWND) {
        unsafe {
            self.windows.retain(|w| IsWindow(w.0.hwnd) != 0);
            if !self.windows.iter().any(|w| w.0.local_save.is_some())
                && let Some((form, save)) = self.local_pending.pop_front()
            {
                match create(host, host, "ultra-bar".into(), form, None, Some(save)) {
                    Ok(window) => {
                        self.windows.push(window);
                    }
                    Err(error) => eprintln!("[editors] {error:#}"),
                }
            }
            while let Some((caller, request)) = self.pending.pop_front() {
                match request.operation {
                    Operation::Open { form } => {
                        if let Some(old) = self.windows.iter().find(|w| {
                            w.0.plugin == request.plugin_id
                                && w.0.form.session == form.session
                                && !w.0.mount
                        }) {
                            SetForegroundWindow(old.0.hwnd);
                            continue;
                        }
                        if let Ok(window) =
                            create(host, caller, request.plugin_id, form, None, None)
                        {
                            self.windows.push(window);
                        }
                    }
                    Operation::Mount {
                        session,
                        field,
                        x,
                        y,
                        width,
                        height,
                    } => {
                        if let Some(hwnd) = self
                            .windows
                            .iter()
                            .find(|w| {
                                w.0.plugin == request.plugin_id
                                    && w.0.form.session == session
                                    && w.0.caller == caller
                                    && w.0.mount
                            })
                            .map(|w| w.0.hwnd)
                        {
                            // A repeated mount is a layout update, never a replacement of the draft.
                            SetWindowPos(
                                hwnd,
                                null_mut(),
                                x,
                                y,
                                width,
                                height,
                                SWP_NOACTIVATE | SWP_NOZORDER,
                            );
                            InvalidateRect(hwnd, null(), 0);
                            continue;
                        }
                        let form = Form {
                            session,
                            title: String::new(),
                            fields: vec![field],
                            can_delete: false,
                        };
                        if let Ok(window) = create(
                            host,
                            caller,
                            request.plugin_id,
                            form,
                            Some(RECT {
                                left: x,
                                top: y,
                                right: x + width,
                                bottom: y + height,
                            }),
                            None,
                        ) {
                            self.windows.push(window);
                        }
                    }
                    Operation::Complete {
                        session,
                        error,
                        close,
                    } => {
                        if let Some(window) = self.windows.iter_mut().find(|w| {
                            w.0.plugin == request.plugin_id
                                && w.0.caller == caller
                                && w.0.form.session == session
                        }) {
                            window.0.saving = false;
                            window.0.error = error;
                            if close {
                                DestroyWindow(window.0.hwnd);
                            } else {
                                layout(&mut window.0);
                                InvalidateRect(window.0.hwnd, null(), 0);
                            }
                        }
                    }
                    Operation::ChoosePath { session, save } => {
                        let mut buffer = vec![0u16; 32768];
                        let mut dialog: OPENFILENAMEW = zeroed();
                        dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
                        dialog.hwndOwner = caller;
                        dialog.lpstrFile = buffer.as_mut_ptr();
                        dialog.nMaxFile = buffer.len() as u32;
                        dialog.lpstrDefExt = windows_sys::core::w!("json");
                        dialog.Flags = OFN_EXPLORER
                            | OFN_NOCHANGEDIR
                            | OFN_PATHMUSTEXIST
                            | if save {
                                OFN_OVERWRITEPROMPT
                            } else {
                                OFN_FILEMUSTEXIST
                            };
                        let accepted = if save {
                            GetSaveFileNameW(&mut dialog)
                        } else {
                            GetOpenFileNameW(&mut dialog)
                        };
                        let path = if accepted != 0 {
                            String::from_utf16_lossy(
                                &buffer
                                    [..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
                            )
                        } else {
                            String::new()
                        };
                        emit(
                            host,
                            caller,
                            EditorEvent {
                                session,
                                action: "path".into(),
                                values: std::collections::BTreeMap::from([("path".into(), path)]),
                                ..Default::default()
                            },
                        );
                    }
                    Operation::Settings => self.settings.push(request.plugin_id),
                }
            }
        }
    }
}
struct Delivery {
    host: usize,
    caller: usize,
    pid: u32,
    session: u64,
    change: bool,
    bytes: Vec<u8>,
}
enum DeliveryNote {
    Critical(Delivery),
    Changes,
}
struct Transport {
    tx: std::sync::mpsc::Sender<DeliveryNote>,
    changes: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<(usize, u64), Delivery>>>,
}
unsafe fn emit(host: HWND, caller: HWND, event: EditorEvent) {
    static OUT: std::sync::OnceLock<Transport> = std::sync::OnceLock::new();
    let out = OUT.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<DeliveryNote>();
        let changes = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let shared = changes.clone();
        std::thread::spawn(move || {
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
                );
            }
            while let Ok(note) = rx.recv() {
                let batch = match note {
                    DeliveryNote::Critical(d) => vec![d],
                    DeliveryNote::Changes => shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .drain()
                        .map(|(_, d)| d)
                        .collect(),
                };
                for d in batch {
                    unsafe {
                        let mut pid = 0;
                        GetWindowThreadProcessId(d.caller as HWND, &mut pid);
                        if pid != d.pid {
                            if !d.change {
                                PostMessageW(d.host as HWND, FAILED, d.caller, d.session as isize);
                            }
                            continue;
                        }
                        let packet = COPYDATASTRUCT {
                            dwData: EVENT,
                            cbData: d.bytes.len() as u32,
                            lpData: d.bytes.as_ptr() as _,
                        };
                        let mut result = 0;
                        let delivered = SendMessageTimeoutW(
                            d.caller as HWND,
                            WM_COPYDATA,
                            d.host,
                            (&packet as *const COPYDATASTRUCT) as isize,
                            SMTO_ABORTIFHUNG | SMTO_BLOCK,
                            500,
                            &mut result,
                        );
                        if !d.change && (delivered == 0 || result != 1) {
                            PostMessageW(d.host as HWND, FAILED, d.caller, d.session as isize);
                        }
                    }
                }
            }
        });
        Transport { tx, changes }
    });
    if let Ok(bytes) = serde_json::to_vec(&event) {
        if bytes.len() > 65536 {
            return;
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(caller, &mut pid);
        }
        let delivery = Delivery {
            host: host as usize,
            caller: caller as usize,
            pid,
            session: event.session,
            change: event.action == "change",
            bytes,
        };
        if delivery.change {
            let mut pending = out.changes.lock().unwrap_or_else(|e| e.into_inner());
            let first = pending.is_empty();
            let key = (delivery.caller, delivery.session);
            if pending.len() < 128 || pending.contains_key(&key) {
                pending.insert(key, delivery);
            }
            drop(pending);
            if first {
                let _ = out.tx.send(DeliveryNote::Changes);
            }
        } else {
            let _ = out.tx.send(DeliveryNote::Critical(delivery));
        }
    }
}
unsafe fn send(view: &View, action: &str, values: Values) {
    if view.local_save.is_some() {
        if action == "closed" {
            unsafe {
                PostMessageW(view.host, UPDATE, 0, 0);
            }
        }
        return;
    }
    unsafe {
        emit(
            view.host,
            view.caller,
            EditorEvent {
                session: view.form.session,
                action: action.into(),
                window: view.hwnd as usize,
                input: view
                    .bindings
                    .first()
                    .and_then(|b| b.inputs.first())
                    .copied()
                    .unwrap_or(null_mut()) as usize,
                values,
            },
        );
    }
}
unsafe fn text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd).clamp(0, 32768) as usize;
        let mut data = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, data.as_mut_ptr(), data.len() as i32);
        String::from_utf16_lossy(&data[..n.max(0) as usize])
    }
}
unsafe fn values(view: &View) -> Result<Values> {
    unsafe {
        let mut values = Values::new();
        for b in &view.bindings {
            let h = b.inputs[0];
            let value = match b.field.kind {
                Kind::Select => {
                    let i = SendMessageW(h, CB_GETCURSEL, 0, 0);
                    b.field
                        .options
                        .get(i.max(0) as usize)
                        .map(|o| o.value.clone())
                        .unwrap_or_default()
                }
                Kind::Toggle => {
                    (SendMessageW(h, BM_GETCHECK, 0, 0) == BST_CHECKED as isize).to_string()
                }
                Kind::MultiSelect => serde_json::to_string(
                    &b.inputs
                        .iter()
                        .zip(&b.field.options)
                        .filter(|(h, _)| {
                            SendMessageW(**h, BM_GETCHECK, 0, 0) == BST_CHECKED as isize
                        })
                        .map(|(_, o)| o.value.clone())
                        .collect::<Vec<_>>(),
                )?,
                Kind::Date | Kind::Time => {
                    let mut t: SYSTEMTIME = zeroed();
                    if SendMessageW(
                        h,
                        DTM_GETSYSTEMTIME,
                        0,
                        (&mut t as *mut SYSTEMTIME) as isize,
                    ) != GDT_VALID as isize
                    {
                        values.insert(b.field.id.clone(), text(controls::date_input(h)));
                        continue;
                    }
                    if b.field.kind == Kind::Date {
                        format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
                    } else {
                        format!("{:02}:{:02}", t.wHour, t.wMinute)
                    }
                }
                _ => text(h),
            };
            values.insert(b.field.id.clone(), value);
        }
        Ok(values)
    }
}
fn validate(form: &Form, values: &Values) -> Result<()> {
    for f in &form.fields {
        if !f.visible(values) {
            continue;
        }
        let v = values.get(&f.id).map(String::as_str).unwrap_or("");
        ensure!(
            !f.constraints.required || !v.trim().is_empty(),
            "{}不能为空",
            f.label
        );
        ensure!(
            v.len() <= f.constraints.max_length.unwrap_or(32768),
            "{}内容过长",
            f.label
        );
        if v.is_empty() {
            continue;
        }
        match f.kind {
            Kind::Number => {
                let n: f64 = v
                    .parse()
                    .with_context(|| format!("{}请输入数字", f.label))?;
                ensure!(
                    n.is_finite()
                        && f.constraints.min.is_none_or(|m| n >= m)
                        && f.constraints.max.is_none_or(|m| n <= m),
                    "{}超出允许范围",
                    f.label
                );
            }
            Kind::Date => {
                NaiveDate::parse_from_str(v, "%Y-%m-%d")
                    .with_context(|| format!("{}日期无效", f.label))?;
            }
            Kind::Time => {
                NaiveTime::parse_from_str(v, "%H:%M")
                    .with_context(|| format!("{}时间无效", f.label))?;
            }
            Kind::DateTime => {
                chrono::DateTime::parse_from_rfc3339(v)
                    .with_context(|| format!("{}需为包含时区的日期时间", f.label))?;
            }
            Kind::Json => {
                serde_json::from_str::<serde_json::Value>(v)
                    .with_context(|| format!("{}不是有效的 JSON", f.label))?;
            }
            Kind::Select => ensure!(
                f.options.iter().any(|o| o.value == v),
                "{}选项无效",
                f.label
            ),
            _ => {}
        }
    }
    Ok(())
}
unsafe fn create(
    host: HWND,
    caller: HWND,
    plugin: String,
    form: Form,
    mount: Option<RECT>,
    local_save: Option<LocalSave>,
) -> Result<Window> {
    unsafe {
        form.validate_schema().map_err(anyhow::Error::msg)?;
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_DATE_CLASSES | ICC_UPDOWN_CLASS | ICC_STANDARD_CLASSES,
        });
        let dpi = GetDpiForWindow(caller).max(96) as i32;
        let u = |n| n * dpi / 96;
        let instance = GetModuleHandleW(null());
        let class = wide("UltraBar.Host.Editor.v1");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let make_font = |name: &str, size: i32| {
            ultra_bar_native_ui::fonts::create_font(
                -u(size),
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
        let mut view = Box::new(View {
            hwnd: null_mut(),
            host,
            caller,
            disabled_caller: false,
            plugin,
            form,
            bindings: vec![],
            font: make_font("Segoe UI", 14),
            icons: make_font("Segoe MDL2 Assets", 18),
            dpi,
            scroll: 0,
            total: 0,
            error: String::new(),
            saving: false,
            mount: mount.is_some(),
            loading: true,
            local_save,
        });
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(
            MonitorFromWindow(caller, MONITOR_DEFAULTTONEAREST),
            &mut info,
        );
        let rect = mount.unwrap_or_else(|| {
            let w = u(520);
            let mut rows = 0;
            let mut col = 0;
            let mut row_height = 0;
            for f in &view.form.fields {
                let h = field_height(f);
                if f.span == 2 && col != 0 {
                    rows += row_height;
                    col = 0;
                    row_height = 0;
                }
                row_height = row_height.max(h);
                col += f.span as i32;
                if col >= 2 {
                    rows += row_height;
                    col = 0;
                    row_height = 0;
                }
            }
            let h = u((rows + row_height + 140).clamp(260, 820))
                .min(info.rcWork.bottom - info.rcWork.top - u(24));
            RECT {
                left: (info.rcWork.left + info.rcWork.right - w) / 2,
                top: (info.rcWork.top + info.rcWork.bottom - h) / 2,
                right: (info.rcWork.left + info.rcWork.right + w) / 2,
                bottom: (info.rcWork.top + info.rcWork.bottom + h) / 2,
            }
        });
        let hwnd = CreateWindowExW(
            if view.mount {
                0
            } else {
                WS_EX_TOOLWINDOW | WS_EX_LAYERED
            },
            class.as_ptr(),
            wide(&view.form.title).as_ptr(),
            WS_CLIPCHILDREN
                | if view.mount {
                    WS_CHILD | WS_VISIBLE
                } else {
                    WS_POPUP | WS_SYSMENU
                },
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            caller,
            null_mut(),
            instance,
            (&mut *view as *mut View).cast(),
        );
        ensure!(!hwnd.is_null(), "无法创建编辑器");
        view.hwnd = hwnd;
        if view.mount {
            ultra_bar_native_ui::geometry::attach_rounded(hwnd, 10);
        }
        if !view.mount {
            ultra_bar_native_ui::enable_glass(hwnd);
            SetLayeredWindowAttributes(hwnd, 0, 244, LWA_ALPHA);
            ultra_bar_native_ui::geometry::attach_rounded(hwnd, 20);
        }
        for (index, f) in view.form.fields.iter().enumerate() {
            let id = 1000 + index as i32;
            let mut inputs = vec![];
            let mut browse = null_mut();
            match f.kind {
                Kind::Select => {
                    let h = controls::create(
                        hwnd,
                        "COMBOBOX",
                        "",
                        id,
                        WS_TABSTOP | CBS_DROPDOWNLIST as u32,
                        (0, 0, 200, 28),
                        (view.font, dpi),
                    );
                    for option in &f.options {
                        SendMessageW(h, CB_ADDSTRING, 0, wide(&option.label).as_ptr() as isize);
                    }
                    let selected = f
                        .options
                        .iter()
                        .position(|o| o.value == f.value)
                        .unwrap_or(0);
                    SendMessageW(h, CB_SETCURSEL, selected, 0);
                    inputs.push(h);
                }
                Kind::Toggle | Kind::MultiSelect => {
                    let selected: Vec<String> = serde_json::from_str(&f.value).unwrap_or_default();
                    let options = if f.kind == Kind::Toggle {
                        vec![OptionItem {
                            label: f.label.clone(),
                            value: "true".into(),
                        }]
                    } else {
                        f.options.clone()
                    };
                    for (n, option) in options.iter().enumerate() {
                        let h = controls::create(
                            hwnd,
                            "BUTTON",
                            &option.label,
                            id + n as i32,
                            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                            (0, 0, 200, 28),
                            (view.font, dpi),
                        );
                        let checked = if f.kind == Kind::Toggle {
                            f.value == "true"
                        } else {
                            selected.contains(&option.value)
                        };
                        if f.kind == Kind::MultiSelect {
                            SetPropW(
                                h,
                                wide("UltraBar.HostEditor.Checkbox").as_ptr(),
                                1usize as HANDLE,
                            );
                        }
                        SendMessageW(
                            h,
                            BM_SETCHECK,
                            if checked { BST_CHECKED } else { BST_UNCHECKED } as usize,
                            0,
                        );
                        inputs.push(h);
                    }
                }
                Kind::Date | Kind::Time => {
                    let time = f.kind == Kind::Time;
                    let h = controls::create(
                        hwnd,
                        "SysDateTimePick32",
                        "",
                        if time { 103 } else { 102 },
                        WS_TABSTOP | if time { DTS_TIMEFORMAT | DTS_UPDOWN } else { 0 },
                        (0, 0, 200, 28),
                        (view.font, dpi),
                    );
                    SendMessageW(
                        h,
                        DTM_SETFORMATW,
                        0,
                        wide(if time { "HH:mm" } else { "yyyy-MM-dd" }).as_ptr() as isize,
                    );
                    let mut t: SYSTEMTIME = zeroed();
                    GetLocalTime(&mut t);
                    if time {
                        if let Ok(v) = NaiveTime::parse_from_str(&f.value, "%H:%M") {
                            t.wHour = v.hour() as u16;
                            t.wMinute = v.minute() as u16;
                        }
                    } else if let Ok(v) = NaiveDate::parse_from_str(&f.value, "%Y-%m-%d") {
                        t.wYear = v.year() as u16;
                        t.wMonth = v.month() as u16;
                        t.wDay = v.day() as u16;
                    }
                    SendMessageW(
                        h,
                        DTM_SETSYSTEMTIME,
                        GDT_VALID as usize,
                        (&t as *const SYSTEMTIME) as isize,
                    );
                    let child = controls::date_input(h);
                    if !child.is_null() {
                        SetWindowSubclass(child, Some(temporal_changed), 0x55424d, hwnd as usize);
                    }
                    inputs.push(h);
                }
                Kind::Text
                | Kind::Secret
                | Kind::Search
                | Kind::Multiline
                | Kind::Number
                | Kind::DateTime
                | Kind::Color
                | Kind::File
                | Kind::Directory
                | Kind::Hotkey
                | Kind::Tags
                | Kind::Code
                | Kind::Json => {
                    let multi = matches!(f.kind, Kind::Multiline | Kind::Code | Kind::Json);
                    let h = controls::create(
                        hwnd,
                        "EDIT",
                        &f.value,
                        id,
                        WS_TABSTOP
                            | if multi {
                                ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32
                            } else {
                                ES_AUTOHSCROLL as u32
                            }
                            | if f.kind == Kind::Secret {
                                ES_PASSWORD as u32
                            } else {
                                0
                            },
                        (0, 0, 200, 28),
                        (view.font, dpi),
                    );
                    inputs.push(h);
                    SendMessageW(
                        h,
                        EM_SETLIMITTEXT,
                        f.constraints.max_length.unwrap_or(32768).min(32768),
                        0,
                    );
                    if matches!(f.kind, Kind::Color | Kind::File | Kind::Directory) {
                        browse = controls::create(
                            hwnd,
                            "BUTTON",
                            "…",
                            8000 + index as i32,
                            WS_TABSTOP | BS_PUSHBUTTON as u32,
                            (0, 0, 28, 28),
                            (view.font, dpi),
                        );
                    }
                    if f.kind == Kind::Search {
                        SendMessageW(h, EM_SETCUEBANNER, 0, wide("搜索…").as_ptr() as isize);
                        SendMessageW(h, EM_SETLIMITTEXT, 8192, 0);
                    }
                    if f.kind == Kind::Number {
                        SetWindowSubclass(
                            h,
                            Some(number_keys),
                            0x55424b,
                            Box::into_raw(Box::new(f.constraints.clone())) as usize,
                        );
                    }
                    if matches!(f.kind, Kind::Code | Kind::Json) {
                        SetWindowSubclass(h, Some(code_keys), 0x55424c, 0);
                    }
                    if f.kind == Kind::Hotkey {
                        SetWindowSubclass(h, Some(hotkey_keys), 0x554249, 0);
                    }
                    if matches!(f.kind, Kind::Code | Kind::Json) {
                        let font = ultra_bar_native_ui::fonts::create_font(
                            -u(14),
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
                            wide("Consolas").as_ptr(),
                        );
                        SetPropW(h, wide("UltraBar.HostEditor.Font").as_ptr(), font);
                        SendMessageW(h, WM_SETFONT, font as usize, 0);
                        SetWindowSubclass(h, Some(release_font), 0x55424a, 0);
                    }
                    if view.mount && f.kind == Kind::Search {
                        SetWindowSubclass(h, Some(search_keys), 0x554248, caller as usize);
                    }
                }
            }
            ensure!(!inputs.is_empty(), "{}缺少选项", f.label);
            view.bindings.push(Binding {
                field: f.clone(),
                inputs,
                browse,
                bounds: zeroed(),
                visible: true,
            });
        }
        if !view.mount {
            for (id, glyph) in [
                (9001, "\u{e74e}"),
                (9002, "\u{e711}"),
                (9003, "\u{e74d}"),
                (9004, "\u{e8bb}"),
            ] {
                if id != 9003 || view.form.can_delete {
                    let h = CreateWindowExW(
                        0,
                        wide("BUTTON").as_ptr(),
                        wide(glyph).as_ptr(),
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                        0,
                        0,
                        u(36),
                        u(30),
                        hwnd,
                        id as _,
                        instance,
                        null(),
                    );
                    SendMessageW(h, WM_SETFONT, view.icons as usize, 0);
                    ultra_bar_native_ui::input::attach_label(h);
                    if view.local_save.is_some() && id == 9002 {
                        let tip = CreateWindowExW(
                            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                            TOOLTIPS_CLASSW,
                            null(),
                            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
                            0,
                            0,
                            0,
                            0,
                            hwnd,
                            null_mut(),
                            instance,
                            null(),
                        );
                        let info = TTTOOLINFOW {
                            cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32,
                            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                            hwnd,
                            uId: h as usize,
                            lpszText: windows_sys::core::w!("取消，保留原值").cast_mut(),
                            ..zeroed()
                        };
                        SendMessageW(tip, TTM_ADDTOOLW, 0, (&info as *const TTTOOLINFOW) as isize);
                    }
                }
            }
        }
        layout(&mut view);
        view.loading = false;
        if !view.mount {
            // Local settings use the shell itself as caller. Disabling it would
            // disable the entire taskbar, including application and tray clicks.
            // A plugin's separate caller can still be modal; restore it only if
            // this editor actually disabled it.
            if caller != host && IsWindowEnabled(caller) != 0 {
                EnableWindow(caller, 0);
                view.disabled_caller = true;
            }
            if !std::env::args().any(|a| a == "--editor-host-smoke" || a == "--script-drop-smoke") {
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
                if let Some(input) = view
                    .bindings
                    .iter()
                    .filter(|b| b.visible)
                    .flat_map(|b| b.inputs.iter())
                    .find(|h| IsWindowVisible(**h) != 0)
                {
                    SetFocus(*input);
                    if view.local_save.is_some() {
                        SendMessageW(*input, EM_SETSEL, 0, -1);
                    }
                }
            } else {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    -32000,
                    -32000,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
        send(&view, "opened", Values::new());
        ultra_bar_native_ui::diagnostics::event(
            "render",
            "editor.opened",
            serde_json::json!({"plugin":view.plugin,"session":view.form.session,"fields":view.form.fields.len(),"mounted":view.mount,
                "host_enabled":IsWindowEnabled(host)!=0,"caller_disabled":view.disabled_caller}),
        );
        Ok(Window(view))
    }
}
unsafe fn focus_next(view: &mut View, backward: bool) -> bool {
    unsafe {
        let focus = GetFocus();
        if view
            .bindings
            .iter()
            .any(|b| matches!(b.field.kind, Kind::Code | Kind::Json) && b.inputs.contains(&focus))
            && GetKeyState(VK_CONTROL as i32) >= 0
        {
            return false;
        }
        let mut list = Vec::new();
        for (index, b) in view.bindings.iter().enumerate() {
            if b.visible
                && b.field
                    .enabled_when
                    .iter()
                    .all(|c| c.matches(&values(view).unwrap_or_default()))
            {
                for h in &b.inputs {
                    list.push((Some(index), *h));
                }
                if !b.browse.is_null() {
                    list.push((Some(index), b.browse));
                }
            }
        }
        for id in [9001, 9002, 9003] {
            let h = GetDlgItem(view.hwnd, id);
            if !h.is_null() {
                list.push((None, h));
            }
        }
        if list.is_empty() {
            return false;
        }
        let current = list
            .iter()
            .position(|(_, h)| *h == focus || IsChild(*h, focus) != 0)
            .unwrap_or(list.len() - 1);
        let next = if backward {
            (current + list.len() - 1) % list.len()
        } else {
            (current + 1) % list.len()
        };
        if let Some(index) = list[next].0 {
            let Some(b) = view.bindings.get(index) else {
                return false;
            };
            let mut area: RECT = zeroed();
            GetClientRect(view.hwnd, &mut area);
            let top = b.bounds.top * 96 / view.dpi;
            let bottom = b.bounds.bottom * 96 / view.dpi;
            let end = area.bottom * 96 / view.dpi - 76;
            if top < 60 {
                view.scroll = (view.scroll - (60 - top)).max(0);
            } else if bottom > end {
                view.scroll += bottom - end;
            }
            layout(view);
            InvalidateRect(view.hwnd, null(), 0);
        }
        SetFocus(list[next].1);
        true
    }
}
unsafe extern "system" fn number_keys(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            drop(Box::from_raw(data as *mut Constraints));
            RemoveWindowSubclass(hwnd, Some(number_keys), id);
            return DefSubclassProc(hwnd, m, w, l);
        }
        if m == WM_GETDLGCODE {
            return DefSubclassProc(hwnd, m, w, l) | DLGC_WANTARROWS as isize;
        }
        if m == WM_KEYDOWN && matches!(w as u16, VK_UP | VK_DOWN) {
            let c = &*(data as *const Constraints);
            let step = c.step.unwrap_or(1.);
            let value = text(hwnd).parse::<f64>().unwrap_or(c.min.unwrap_or(0.));
            let value = (value + if w == VK_UP as usize { step } else { -step })
                .max(c.min.unwrap_or(f64::MIN))
                .min(c.max.unwrap_or(f64::MAX));
            let digits = step
                .to_string()
                .split('.')
                .nth(1)
                .map(str::len)
                .unwrap_or(0)
                .min(12);
            SetWindowTextW(hwnd, wide(&format!("{value:.digits$}")).as_ptr());
            return 0;
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn code_keys(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(code_keys), id);
        }
        if m == WM_GETDLGCODE {
            return DefSubclassProc(hwnd, m, w, l) | DLGC_WANTTAB as isize;
        }
        if m == WM_KEYDOWN && w == VK_TAB as usize {
            SendMessageW(hwnd, EM_REPLACESEL, 1, wide("    ").as_ptr() as isize);
            return 0;
        }
        if m == WM_CHAR && w == 9 {
            return 0;
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn temporal_changed(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    parent: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(temporal_changed), id);
        }
        let result = DefSubclassProc(hwnd, m, w, l);
        if matches!(m, WM_KEYUP | WM_KILLFOCUS | WM_SETTEXT) {
            PostMessageW(parent as HWND, WM_APP + 32, 0, 0);
        }
        result
    }
}
unsafe extern "system" fn release_font(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            let font = RemovePropW(hwnd, wide("UltraBar.HostEditor.Font").as_ptr());
            if !font.is_null() {
                ultra_bar_native_ui::fonts::delete_object(font);
            }
            RemoveWindowSubclass(hwnd, Some(release_font), id);
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn hotkey_keys(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(hotkey_keys), id);
        }
        if m == WM_CHAR {
            return 0;
        }
        if m == WM_KEYDOWN || m == WM_SYSKEYDOWN {
            if matches!(
                w as u16,
                VK_CONTROL | VK_SHIFT | VK_MENU | VK_LWIN | VK_RWIN | VK_ESCAPE
            ) {
                return 0;
            }
            let mut parts = Vec::new();
            for (key, label) in [
                (VK_CONTROL, "Ctrl"),
                (VK_MENU, "Alt"),
                (VK_SHIFT, "Shift"),
                (VK_LWIN, "Win"),
            ] {
                if GetKeyState(key as i32) < 0 {
                    parts.push(label.to_string());
                }
            }
            if parts.is_empty() && matches!(w as u16, VK_DELETE | VK_BACK) {
                SetWindowTextW(hwnd, wide("").as_ptr());
                return 0;
            }
            let mut name = [0u16; 64];
            let n = GetKeyNameTextW(l as i32, name.as_mut_ptr(), name.len() as i32);
            parts.push(if n > 0 {
                String::from_utf16_lossy(&name[..n as usize])
            } else {
                format!("VK{w:02X}")
            });
            SetWindowTextW(hwnd, wide(&parts.join("+")).as_ptr());
            return 0;
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn search_keys(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    caller: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(search_keys), id);
        }
        if m == WM_KEYDOWN
            && (matches!(w as u16, VK_RETURN | VK_ESCAPE | VK_UP | VK_DOWN)
                || w == b'C' as usize
                    && GetKeyState(VK_CONTROL as i32) < 0
                    && GetKeyState(VK_SHIFT as i32) < 0)
        {
            PostMessageW(caller as HWND, m, w, l);
            return 0;
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
fn field_height(field: &Field) -> i32 {
    match field.kind {
        Kind::Multiline | Kind::Code | Kind::Json => 92,
        Kind::MultiSelect => 30 + field.options.len().div_ceil(2) as i32 * 28,
        Kind::Toggle => 38,
        _ => 60,
    }
}
unsafe fn layout(view: &mut View) {
    unsafe {
        let u = |n| n * view.dpi / 96;
        let mut area: RECT = zeroed();
        GetClientRect(view.hwnd, &mut area);
        let width = area.right * 96 / view.dpi;
        let height = area.bottom * 96 / view.dpi;
        let vals = values(view).unwrap_or_else(|_| {
            view.form
                .fields
                .iter()
                .map(|f| (f.id.clone(), f.value.clone()))
                .collect()
        });
        let mut y = 64;
        let mut col = 0;
        let mut row_height = 0;
        for b in &mut view.bindings {
            b.visible = b.field.visible(&vals);
            let multiline = matches!(b.field.kind, Kind::Multiline | Kind::Code | Kind::Json);
            let h = field_height(&b.field);
            let span = b.field.span as i32;
            if span == 2 && col != 0 {
                y += row_height;
                col = 0;
                row_height = 0;
            }
            if b.visible {
                let x = 20 + col * (width - 40 + 16) / 2;
                let w = if span == 2 {
                    width - 40
                } else {
                    (width - 56) / 2
                };
                let top = y - view.scroll;
                b.bounds = RECT {
                    left: u(x),
                    top: u(top),
                    right: u(x + w),
                    bottom: u(top + h),
                };
                let show = view.mount || top >= 60 && top + h <= height - 76;
                for (n, input) in b.inputs.iter().enumerate() {
                    let (px, py, pw, ph) = if view.mount {
                        if b.field.kind == Kind::MultiSelect {
                            (n as i32 % 2 * (width / 2), n as i32 / 2 * 28, width / 2, 26)
                        } else {
                            (
                                0,
                                0,
                                width - if b.browse.is_null() { 0 } else { 32 },
                                height,
                            )
                        }
                    } else if b.field.kind == Kind::MultiSelect {
                        (
                            x + (n as i32 % 2) * (w / 2),
                            top + 22 + (n as i32 / 2) * 28,
                            w / 2,
                            26,
                        )
                    } else if b.field.kind == Kind::Toggle {
                        (x, top, w, 30)
                    } else {
                        (
                            x,
                            top + 22,
                            w - if b.browse.is_null() { 0 } else { 32 },
                            if multiline { 64 } else { 28 },
                        )
                    };
                    MoveWindow(
                        *input,
                        u(px),
                        u(py),
                        u(pw),
                        u(ph + if b.field.kind == Kind::Select {
                            24 * 8
                        } else {
                            0
                        }),
                        1,
                    );
                    EnableWindow(
                        *input,
                        i32::from(
                            !view.saving && b.field.enabled_when.iter().all(|c| c.matches(&vals)),
                        ),
                    );
                    ShowWindow(*input, if show { SW_SHOWNA } else { SW_HIDE });
                }
                if !b.browse.is_null() {
                    if view.mount {
                        MoveWindow(b.browse, u(width - 28), 0, u(28), u(height), 1);
                    } else {
                        MoveWindow(b.browse, u(x + w - 28), u(top + 22), u(28), u(28), 1);
                    }
                    ShowWindow(b.browse, if show { SW_SHOWNA } else { SW_HIDE });
                }
                row_height = row_height.max(h);
                col += span;
                if col >= 2 {
                    y += row_height;
                    col = 0;
                    row_height = 0;
                }
            } else {
                for h in &b.inputs {
                    ShowWindow(*h, SW_HIDE);
                }
                if !b.browse.is_null() {
                    ShowWindow(b.browse, SW_HIDE);
                }
            }
        }
        view.total = y + row_height;
        view.scroll = view.scroll.min((view.total - (height - 76)).max(0));
        let close = GetDlgItem(view.hwnd, 9004);
        if !close.is_null() {
            MoveWindow(close, u(width - 44), u(8), u(32), u(32), 1);
        }
        for (id, x) in [(9001, width - 100), (9002, width - 52), (9003, 20)] {
            let h = GetDlgItem(view.hwnd, id);
            if !h.is_null() {
                MoveWindow(h, u(x), u(height - 46), u(36), u(30), 1);
                EnableWindow(h, i32::from(!view.saving));
            }
        }
    }
}
unsafe fn browse(view: &mut View, index: usize) {
    unsafe {
        let Some(b) = view.bindings.get(index) else {
            return;
        };
        let value = match b.field.kind {
            Kind::Color => crate::color_picker::choose(
                view.hwnd,
                theme::parse_hex(&text(b.inputs[0])).unwrap_or(theme::accent_rgb()),
            )
            .map(theme::hex),
            Kind::File => {
                let mut buffer = vec![0u16; 32768];
                let mut dialog: OPENFILENAMEW = zeroed();
                dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
                dialog.hwndOwner = view.hwnd;
                dialog.lpstrFile = buffer.as_mut_ptr();
                dialog.nMaxFile = buffer.len() as u32;
                dialog.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR;
                if GetOpenFileNameW(&mut dialog) != 0 {
                    Some(String::from_utf16_lossy(
                        &buffer[..buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len())],
                    ))
                } else {
                    None
                }
            }
            Kind::Directory => {
                let mut info: BROWSEINFOW = zeroed();
                info.hwndOwner = view.hwnd;
                info.ulFlags = BIF_RETURNONLYFSDIRS | (BIF_NEWDIALOGSTYLE | BIF_EDITBOX);
                let pidl = SHBrowseForFolderW(&info);
                if pidl.is_null() {
                    None
                } else {
                    let mut path = vec![0u16; 32768];
                    let ok = SHGetPathFromIDListEx(pidl, path.as_mut_ptr(), path.len() as u32, 0);
                    CoTaskMemFree(pidl.cast());
                    if ok != 0 {
                        Some(String::from_utf16_lossy(
                            &path[..path.iter().position(|c| *c == 0).unwrap_or(path.len())],
                        ))
                    } else {
                        None
                    }
                }
            }
            _ => None,
        };
        if let Some(value) = value {
            SetWindowTextW(view.bindings[index].inputs[0], wide(&value).as_ptr());
        }
    }
}
unsafe fn paint(view: &View, dc: HDC) {
    unsafe {
        let mut area: RECT = zeroed();
        GetClientRect(view.hwnd, &mut area);
        if let Some(surface) = Surface::new(area.right, area.bottom) {
            FillRect(
                surface.dc,
                &area,
                if view.mount {
                    theme::field_brush()
                } else {
                    theme::background_brush()
                },
            );
            SetBkMode(surface.dc, TRANSPARENT as i32);
            SelectObject(surface.dc, view.font);
            SetTextColor(surface.dc, theme::palette().text);
            if !view.mount {
                let u = |n| n * view.dpi / 96;
                let mut title = RECT {
                    left: u(20),
                    top: u(12),
                    right: area.right - u(48),
                    bottom: u(44),
                };
                DrawTextW(
                    surface.dc,
                    wide(&view.form.title).as_ptr(),
                    -1,
                    &mut title,
                    DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
                for b in &view.bindings {
                    if b.visible
                        && b.bounds.top >= u(60)
                        && b.bounds.bottom <= area.bottom - u(76)
                        && b.field.kind != Kind::Toggle
                    {
                        let mut rect = b.bounds;
                        rect.bottom = rect.top + u(20);
                        SetTextColor(surface.dc, theme::palette().muted);
                        DrawTextW(
                            surface.dc,
                            wide(&b.field.label).as_ptr(),
                            -1,
                            &mut rect,
                            DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                        );
                    }
                }
                if !view.error.is_empty() {
                    let mut r = RECT {
                        left: u(20),
                        top: area.bottom - u(76),
                        right: area.right - u(20),
                        bottom: area.bottom - u(50),
                    };
                    SetTextColor(surface.dc, theme::palette().text);
                    DrawTextW(
                        surface.dc,
                        wide(&view.error).as_ptr(),
                        -1,
                        &mut r,
                        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );
                }
            }
            surface.blit_opaque(dc);
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, m: u32, w: usize, l: isize) -> LRESULT {
    unsafe {
        if m == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut View;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, m, w, l);
        }
        let view = &mut *pointer;
        if theme::handle_message(hwnd, m, w) {
            InvalidateRect(hwnd, null(), 1);
            return 0;
        }
        match m {
            WM_NCHITTEST if !view.mount => {
                let mut point = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                ScreenToClient(hwnd, &mut point);
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                if point.y < 48 * view.dpi / 96 && point.x < rect.right - 48 * view.dpi / 96 {
                    HTCAPTION as isize
                } else {
                    HTCLIENT as isize
                }
            }
            WM_ERASEBKGND => 1,
            WM_PRINTCLIENT => {
                paint(view, w as HDC);
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut area: RECT = zeroed();
                GetClientRect(hwnd, &mut area);
                paint(view, dc);
                EndPaint(hwnd, &ps);
                0
            }
            WM_SIZE => {
                layout(view);
                0
            }
            WM_MOUSEWHEEL => {
                if !view.mount {
                    let mut r: RECT = zeroed();
                    GetClientRect(hwnd, &mut r);
                    let max = (view.total - (r.bottom * 96 / view.dpi - 76)).max(0);
                    view.scroll =
                        (view.scroll - ((w >> 16) as u16 as i16 as i32 / 120) * 48).clamp(0, max);
                    layout(view);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_DRAWITEM => {
                let item = &*(l as *const DRAWITEMSTRUCT);
                if item.CtlType == ODT_COMBOBOX {
                    controls::draw_combo_item(item);
                } else {
                    FillRect(item.hDC, &item.rcItem, theme::field_brush());
                    SetBkMode(item.hDC, TRANSPARENT as i32);
                    SetTextColor(item.hDC, theme::palette().text);
                    SelectObject(item.hDC, view.icons);
                    let mut r = item.rcItem;
                    DrawTextW(
                        item.hDC,
                        wide(&text(item.hwndItem)).as_ptr(),
                        -1,
                        &mut r,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
                1
            }
            WM_MEASUREITEM => {
                let item = &mut *(l as *mut MEASUREITEMSTRUCT);
                if item.CtlType == ODT_COMBOBOX {
                    item.itemHeight = (24 * view.dpi / 96) as u32;
                }
                1
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => {
                controls::control_color(m, w, l)
            }
            m if m == WM_NOTIFY || m == WM_APP + 32 => {
                if !view.loading
                    && let Ok(values) = values(view)
                {
                    send(view, "change", values);
                }
                0
            }
            WM_COMMAND => {
                let id = (w & 0xffff) as i32;
                let notification = (w >> 16) as u32;
                if notification == CBN_DROPDOWN {
                    controls::prepare_combo_popup(l as HWND);
                    return 0;
                }
                if id == 9002 || id == 2 || id == 9004 {
                    DestroyWindow(hwnd);
                    return 0;
                }
                if id == 9001 || id == 9003 {
                    if id == 9003
                        && MessageBoxW(
                            hwnd,
                            wide("删除这条记录？").as_ptr(),
                            wide("删除").as_ptr(),
                            MB_YESNO | MB_DEFBUTTON2,
                        ) != IDYES
                    {
                        return 0;
                    }
                    let result = if id == 9003 {
                        Ok(Values::new())
                    } else {
                        values(view).and_then(|values| {
                            validate(&view.form, &values)?;
                            Ok(values)
                        })
                    };
                    match result {
                        Ok(values) => {
                            if let Some(save) = &view.local_save {
                                match save(&values) {
                                    Ok(()) => {
                                        DestroyWindow(hwnd);
                                    }
                                    Err(error) => {
                                        view.error = format!("{error:#}");
                                        InvalidateRect(hwnd, null(), 0);
                                    }
                                }
                                return 0;
                            }
                            view.saving = true;
                            layout(view);
                            send(view, if id == 9003 { "delete" } else { "submit" }, values);
                        }
                        Err(error) => {
                            view.error = format!("{error:#}");
                            InvalidateRect(hwnd, null(), 0);
                        }
                    }
                    return 0;
                }
                if (8000..8064).contains(&id) {
                    browse(view, (id - 8000) as usize);
                    return 0;
                }
                if !view.loading && matches!(notification, EN_CHANGE | CBN_SELCHANGE | BN_CLICKED) {
                    if let Ok(values) = values(view) {
                        send(view, "change", values);
                    }
                    if !view.mount && notification != EN_CHANGE {
                        layout(view);
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                0
            }
            DM_GETDEFID => ((DC_HASDEFID as usize) << 16 | 9001) as isize,
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                if view.disabled_caller {
                    EnableWindow(view.caller, 1);
                }
                send(view, "closed", Values::new());
                0
            }
            _ => DefWindowProcW(hwnd, m, w, l),
        }
    }
}

unsafe fn pump_test_events() {
    unsafe {
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(100) {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
/// Exercise the real host controls without activating a window or modifying plugin data.
pub unsafe fn smoke(directory: &std::path::Path, schema: Option<&std::path::Path>) -> Result<()> {
    unsafe extern "system" fn caller(hwnd: HWND, m: u32, w: usize, l: isize) -> LRESULT {
        unsafe {
            if m == WM_NCCREATE {
                SetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                    (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
                );
            }
            if m == WM_COPYDATA && l != 0 {
                let packet = &*(l as *const COPYDATASTRUCT);
                if packet.dwData == EVENT {
                    let bytes = std::slice::from_raw_parts(
                        packet.lpData as *const u8,
                        packet.cbData as usize,
                    );
                    if let Ok(event) = serde_json::from_slice::<EditorEvent>(bytes) {
                        (*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Vec<EditorEvent>))
                            .push(event);
                        return 1;
                    }
                }
            }
            DefWindowProcW(hwnd, m, w, l)
        }
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, dc: HDC, flags: u32) -> i32;
    }
    unsafe {
        ensure!(directory.is_absolute(), "验证目录必须为绝对路径");
        std::fs::create_dir_all(directory)?;
        let mut events = Box::new(Vec::<EditorEvent>::new());
        let instance = GetModuleHandleW(null());
        let class = wide("UltraBar.Host.Editor.TestOwner");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(caller),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        });
        let owner = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            -32000,
            -32000,
            10,
            10,
            null_mut(),
            null_mut(),
            instance,
            (&mut *events as *mut Vec<EditorEvent>).cast(),
        );
        ensure!(!owner.is_null(), "无法创建验证窗口");
        let form = if let Some(path) = schema {
            serde_json::from_slice::<Form>(&std::fs::read(path)?)?
        } else {
            let mut fields = vec![
                Field::new("title", "名称", Kind::Text, "示例"),
                Field::new("secret", "密码", Kind::Secret, "test"),
                Field::new("number", "数值", Kind::Number, 12),
                Field::new("date", "日期", Kind::Date, "2026-11-12"),
                Field::new("time", "时间", Kind::Time, "09:00"),
                Field::new("notes", "备注", Kind::Multiline, "第一行\r\n第二行"),
                Field::new("color", "颜色", Kind::Color, "#2D79E7"),
                Field::new("path", "文件", Kind::File, ""),
                Field::new("hotkey", "快捷键", Kind::Hotkey, ""),
            ];
            let mut choice = Field::new("choice", "选项", Kind::Select, "a");
            choice.options = vec![
                OptionItem {
                    value: "a".into(),
                    label: "选项 A".into(),
                },
                OptionItem {
                    value: "b".into(),
                    label: "选项 B".into(),
                },
            ];
            fields.insert(3, choice);
            fields.insert(4, Field::new("toggle", "启用", Kind::Toggle, true));
            Form {
                session: 42,
                title: "编辑器".into(),
                fields,
                can_delete: false,
            }
        };
        let mut manager = Manager::default();
        ensure!(
            manager.accept(
                owner,
                Request {
                    protocol: PROTOCOL,
                    plugin_id: "fixture".into(),
                    operation: Operation::Open { form: form.clone() }
                }
            ),
            "编辑器定义未被接受"
        );
        manager.apply(owner);
        let result = (|| -> Result<()> {
            pump_test_events();
            let window = manager.windows.first_mut().context("宿主编辑器创建失败")?;
            let view = &mut window.0;
            ensure!(IsWindowEnabled(owner) != 0, "打开编辑器禁用了宿主任务栏");
            pump_test_events();
            ensure!(
                events.iter().any(|e| e.action == "opened"),
                "未收到编辑器打开事件"
            );
            let pid = windows_sys::Win32::System::Threading::GetCurrentProcessId();
            let mut checked = 0;
            for b in &view.bindings {
                for input in &b.inputs {
                    let mut actual = 0;
                    GetWindowThreadProcessId(*input, &mut actual);
                    ensure!(actual == pid, "编辑控件不属于宿主进程");
                }
                if matches!(
                    b.field.kind,
                    Kind::Text
                        | Kind::Number
                        | Kind::Secret
                        | Kind::Search
                        | Kind::Tags
                        | Kind::Hotkey
                ) {
                    let h = b.inputs[0];
                    let mut rect: RECT = zeroed();
                    GetWindowRect(h, &mut rect);
                    let mut client: RECT = zeroed();
                    GetClientRect(h, &mut client);
                    let mut point = POINT { x: 0, y: 0 };
                    ClientToScreen(h, &mut point);
                    let top = point.y - rect.top;
                    let bottom = rect.bottom - (point.y + client.bottom);
                    ensure!(
                        (top - bottom).abs() <= 1,
                        "{}文字区域没有居中",
                        b.field.label
                    );
                    checked += 1;
                }
            }
            let mut area: RECT = zeroed();
            GetClientRect(view.hwnd, &mut area);
            let surface = Surface::new(area.right, area.bottom).context("无法捕获编辑器")?;
            if let Some(combo) = view
                .bindings
                .iter()
                .find(|b| b.field.kind == Kind::Select)
                .map(|b| b.inputs[0])
            {
                let region = CreateRectRgn(0, 0, 0, 0);
                let mut rect: RECT = zeroed();
                GetWindowRect(combo, &mut rect);
                ensure!(GetWindowRgn(combo, region) != 0, "下拉框缺少点击区域");
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;
                for x in [
                    width / 10,
                    width / 2,
                    width * 9 / 10,
                    width - 8 * view.dpi / 96,
                ] {
                    ensure!(
                        PtInRegion(region, x, height / 2) != 0,
                        "下拉框右侧被旧尺寸的窗口区域裁掉"
                    );
                }
                ultra_bar_native_ui::fonts::delete_object(region);
                let mut info: COMBOBOXINFO = zeroed();
                info.cbSize = size_of::<COMBOBOXINFO>() as u32;
                ensure!(
                    GetComboBoxInfo(combo, &mut info) != 0
                        && info.rcButton.right >= width - 8 * view.dpi / 96,
                    "下拉箭头命中区域没有随宽度更新"
                );
                for _ in 0..3 {
                    SendMessageW(combo, CB_SHOWDROPDOWN, 1, 0);
                    pump_test_events();
                    let mut info: COMBOBOXINFO = zeroed();
                    info.cbSize = size_of::<COMBOBOXINFO>() as u32;
                    ensure!(
                        GetComboBoxInfo(combo, &mut info) != 0
                            && IsWindowVisible(info.hwndList) != 0,
                        "下拉框没有展开"
                    );
                    let mut bounds: RECT = zeroed();
                    GetWindowRect(info.hwndList, &mut bounds);
                    ensure!(
                        bounds.bottom - bounds.top >= 48 * view.dpi / 96,
                        "下拉列表被压缩成输入框高度"
                    );
                    SendMessageW(combo, CB_SHOWDROPDOWN, 0, 0);
                }
                std::fs::write(
                    directory.join("dropdown-result.json"),
                    b"{\"opened_repeatedly\":true,\"height_reserved\":true,\"full_width_hit_region\":true,\"arrow_hit_region\":true}",
                )?;
            }
            let _ = PrintWindow(view.hwnd, surface.dc, 0);
            paint(view, surface.dc);
            let mut root = POINT { x: 0, y: 0 };
            ClientToScreen(view.hwnd, &mut root);
            let mut children: Vec<HWND> = view
                .bindings
                .iter()
                .flat_map(|b| {
                    b.inputs
                        .iter()
                        .copied()
                        .chain((!b.browse.is_null()).then_some(b.browse))
                })
                .collect();
            children.extend(
                [9001, 9002, 9003, 9004]
                    .iter()
                    .map(|id| GetDlgItem(view.hwnd, *id))
                    .filter(|h| !h.is_null()),
            );
            for child in children {
                if GetWindowLongPtrW(child, GWL_STYLE) & WS_VISIBLE as isize == 0 {
                    continue;
                }
                let mut rect: RECT = zeroed();
                GetWindowRect(child, &mut rect);
                let saved = SaveDC(surface.dc);
                SetViewportOrgEx(
                    surface.dc,
                    rect.left - root.x,
                    rect.top - root.y,
                    null_mut(),
                );
                SendMessageW(
                    child,
                    WM_PRINT,
                    surface.dc as usize,
                    (PRF_CLIENT | PRF_NONCLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as isize,
                );
                RestoreDC(surface.dc, saved);
            }
            surface.save_bmp(&directory.join("host-editor.bmp"))?;
            let title = view
                .bindings
                .iter()
                .find(|b| b.field.id == "title")
                .or_else(|| view.bindings.iter().find(|b| b.field.kind == Kind::Text))
                .context("缺少文本字段")?
                .inputs[0];
            SendMessageW(title, EM_SETSEL, 0, -1);
            SendMessageW(
                title,
                EM_REPLACESEL,
                1,
                wide("宿主编辑验证").as_ptr() as isize,
            );
            SendMessageW(view.hwnd, WM_COMMAND, 9001, 0);
            pump_test_events();
            ensure!(events.iter().any(|e|e.action=="submit"&&e.values.values().any(|v|v=="宿主编辑验证")),"编辑结果未返回业务方");
            let hwnd = view.hwnd;
            manager.accept(
                owner,
                Request {
                    protocol: PROTOCOL,
                    plugin_id: "fixture".into(),
                    operation: Operation::Complete {
                        session: form.session,
                        error: "校验信息".into(),
                        close: false,
                    },
                },
            );
            manager.apply(owner);
            ensure!(
                text(title) == "宿主编辑验证" && IsWindow(hwnd) != 0,
                "校验失败丢失了编辑内容"
            );
            ensure!(
                SendMessageW(title, EM_CANUNDO, 0, 0) != 0,
                "业务校验破坏了撤销历史"
            );
            let fonts = crate::visual::fonts::installed();
            let family = ["Arial", "Microsoft YaHei", "Calibri"]
                .into_iter()
                .find(|f| fonts.iter().any(|n| n == f))
                .unwrap_or("Segoe UI");
            let font_path = directory.join("font-settings.json");
            std::fs::write(
                &font_path,
                serde_json::to_vec(&serde_json::json!({"font_family":family}))?,
            )?;
            crate::visual::fonts::configure(font_path, family);
            SendMessageW(hwnd, crate::visual::fonts::message(), 0, 0);
            ensure!(
                text(title) == "宿主编辑验证" && IsWindow(hwnd) != 0,
                "切换字体丢失了草稿"
            );
            let mut actual: LOGFONTW = zeroed();
            GetObjectW(
                SendMessageW(title, WM_GETFONT, 0, 0) as _,
                size_of::<LOGFONTW>() as i32,
                (&mut actual as *mut LOGFONTW).cast(),
            );
            let n = actual.lfFaceName.iter().position(|v| *v == 0).unwrap_or(32);
            ensure!(
                String::from_utf16_lossy(&actual.lfFaceName[..n]) == family,
                "编辑控件没有采用默认字体"
            );
            GetObjectW(
                SendMessageW(GetDlgItem(hwnd, 9001), WM_GETFONT, 0, 0) as _,
                size_of::<LOGFONTW>() as i32,
                (&mut actual as *mut LOGFONTW).cast(),
            );
            let n = actual.lfFaceName.iter().position(|v| *v == 0).unwrap_or(32);
            ensure!(
                String::from_utf16_lossy(&actual.lfFaceName[..n]).contains("Assets"),
                "字体设置覆盖了图标字形"
            );
            std::fs::write(
                directory.join("font-result.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"installed_families":fonts.len(),"family":family,"draft_preserved":true,"icons_preserved":true}),
                )?,
            )?;
            std::fs::write(
                directory.join("host-editor-result.json"),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"renderer_pid":pid,"fields":form.fields.len(),"centered_fields":checked,"events_round_trip":true,"validation_preserves_draft":true,"global_input":false}),
                )?,
            )?;
            Ok(())
        })();
        drop(manager);
        let result = result.and_then(|()| {
            let saved = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
            let mut local = Manager::default();
            for name in ["first", "second"] {
                let saved = saved.clone();
                local.open_local(Form {
                    session: 0, title: "脚本别名".into(),
                    fields: vec![Field::new("alias", "脚本", Kind::Text, name)],
                    can_delete: false,
                }, move |values| { saved.borrow_mut().push(values["alias"].clone()); Ok(()) })?;
            }
            local.apply(owner);
            ensure!(local.windows.len() == 1 && local.local_pending.len() == 1, "批量别名弹窗没有顺序展示");
            let h = local.windows[0].0.hwnd;
            SetWindowTextW(local.windows[0].0.bindings[0].inputs[0], wide("我的脚本").as_ptr());
            SendMessageW(h, WM_COMMAND, 9001, 0);
            ensure!(IsWindow(h) == 0 && *saved.borrow() == ["我的脚本"], "别名保存未完成或未关闭弹窗");
            local.apply(owner);
            let h = local.windows[0].0.hwnd;
            SendMessageW(h, WM_COMMAND, 9002, 0);
            local.apply(owner);
            ensure!(!local.busy() && saved.borrow().len() == 1 && IsWindowEnabled(owner) != 0, "跳过别名没有保留原状态");
            let field=Field::new("query","",Kind::Search,"initial");
            let mount=|width,height,value:&str| Request{protocol:PROTOCOL,plugin_id:"fixture_mount".into(),operation:Operation::Mount{
                session:70,field:Field{value:value.into(),..field.clone()},x:20,y:30,width,height}};
            ensure!(local.accept(owner,mount(200,28,"initial")),"首次挂载失败");local.apply(owner);
            let mounted=local.windows.iter().find(|w|w.0.mount).context("缺少挂载编辑器")?;
            let mounted_hwnd=mounted.0.hwnd;let input=mounted.0.bindings[0].inputs[0];
            SendMessageW(input,EM_SETSEL,0,-1);SendMessageW(input,EM_REPLACESEL,1,wide("draft search").as_ptr() as isize);
            ensure!(local.accept(owner,mount(640,44,"reset")),"挂载布局更新失败");local.apply(owner);
            let mounted=local.windows.iter().find(|w|w.0.mount).context("更新后挂载丢失")?;
            ensure!(mounted.0.hwnd==mounted_hwnd&&mounted.0.bindings[0].inputs[0]==input,"挂载更新重建了输入框");
            let mut rect:RECT=zeroed();GetWindowRect(mounted_hwnd,&mut rect);
            ensure!(rect.right-rect.left==640&&rect.bottom-rect.top==44&&text(input)=="draft search","挂载更新没有应用尺寸或覆盖了草稿");
            ensure!(SendMessageW(input,EM_CANUNDO,0,0)!=0,"挂载更新破坏了撤销历史");
            std::fs::write(directory.join("mount-layout-result.json"),b"{\"resize\":true,\"same_control\":true,\"draft_preserved\":true,\"undo_preserved\":true}")?;
            std::fs::write(directory.join("local-editor-result.json"), serde_json::to_vec_pretty(&serde_json::json!({"save":true,"skip":true,"batch_sequential":true,"global_input":false}))?)?;
            Ok(())
        });
        DestroyWindow(owner);
        result
    }
}
