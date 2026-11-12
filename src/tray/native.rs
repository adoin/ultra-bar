use crate::visual::wide;
use anyhow::{Result, bail};
use std::{
    cell::Cell,
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::ScreenToClient,
    System::{Diagnostics::Debug::*, Memory::*, SystemInformation::GetTickCount64, Threading::*},
    UI::{Controls::*, Shell::*, WindowsAndMessaging::*},
};
use windows_sys::core::BOOL;

pub static DIRTY: AtomicBool = AtomicBool::new(true);
static MAIN: AtomicUsize = AtomicUsize::new(0);
static OVERFLOW: AtomicUsize = AtomicUsize::new(0);
static MAIN_ROOT: AtomicUsize = AtomicUsize::new(0);
static OVERFLOW_ROOT: AtomicUsize = AtomicUsize::new(0);
pub static SKIPPED: AtomicUsize = AtomicUsize::new(0);
static BLOCKED_PID: AtomicUsize = AtomicUsize::new(0);
static BLOCKED_AT: AtomicU64 = AtomicU64::new(0);

pub fn event(hwnd: HWND, event: u32) {
    let hwnd = hwnd as usize;
    if hwnd != 0
        && [
            MAIN.load(Ordering::Relaxed),
            OVERFLOW.load(Ordering::Relaxed),
            MAIN_ROOT.load(Ordering::Relaxed),
            OVERFLOW_ROOT.load(Ordering::Relaxed),
        ]
        .contains(&hwnd)
    {
        DIRTY.store(true, Ordering::Relaxed);
    } else if hwnd != 0 && event == 0x8000 {
        unsafe {
            let mut class = [0u16; 64];
            let length = GetClassNameW(hwnd as _, class.as_mut_ptr(), class.len() as _);
            if matches!(
                String::from_utf16_lossy(&class[..length.max(0) as usize]).as_str(),
                "Shell_TrayWnd" | "NotifyIconOverflowWindow" | "ToolbarWindow32"
            ) {
                DIRTY.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone)]
pub struct Endpoint {
    toolbar: usize,
    command: i32,
    owner: usize,
    id: u32,
    pid: u32,
}
pub struct Icon(pub isize, std::sync::OnceLock<u64>);
thread_local! {static FALLBACK_ICONS:std::cell::RefCell<std::collections::VecDeque<(String,Icon)>>=const{std::cell::RefCell::new(std::collections::VecDeque::new())};}
unsafe fn executable_icon(path: &str) -> HICON {
    unsafe {
        FALLBACK_ICONS.with(|icons| {
            let mut icons = icons.borrow_mut();
            if let Some(i) = icons.iter().position(|(old, _)| old == path) {
                let entry = icons.remove(i).unwrap();
                let handle = CopyIcon(entry.1.handle());
                icons.push_back(entry);
                return handle;
            }
            let source = crate::visual::AppIcon::from_file(path, 192, 16);
            let icon = Icon::owned(CopyIcon(source.handle));
            let result = CopyIcon(icon.handle());
            if icons.len() >= 32 {
                icons.pop_front();
            }
            icons.push_back((path.into(), icon));
            result
        })
    }
}
impl Icon {
    pub(super) fn owned(handle: HICON) -> Self {
        Self(handle as isize, std::sync::OnceLock::new())
    }
    pub(super) fn fingerprint(&self) -> u64 {
        *self.1.get_or_init(|| unsafe {
            if self.0 == 0 {
                return 1;
            }
            use windows_sys::Win32::Graphics::Gdi::*;
            let Some(surface) = crate::visual::Surface::new(16, 16) else {
                return self.0 as u64;
            };
            let area = RECT {
                left: 0,
                top: 0,
                right: 16,
                bottom: 16,
            };
            let mut hash = 0xcbf29ce484222325u64;
            for rgb in [0x00102030, 0x00efcfaf] {
                let brush = CreateSolidBrush(rgb);
                FillRect(surface.dc, &area, brush);
                ultra_bar_native_ui::fonts::delete_object(brush);
                DrawIconEx(
                    surface.dc,
                    0,
                    0,
                    self.handle(),
                    16,
                    16,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
                for pixel in surface.copy_pixels() {
                    hash ^= (pixel & 0xffffff) as u64;
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
            hash
        })
    }
    pub fn handle(&self) -> HICON {
        self.0 as _
    }
}
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            if self.0 != 0 {
                DestroyIcon(self.handle());
            }
        }
    }
}
#[derive(Clone)]
pub struct Item {
    pub key: String,
    pub preference_key: String,
    pub executable: String,
    pub uid: u32,
    pub title: String,
    pub collapsed: bool,
    pub icon: std::sync::Arc<Icon>,
    pub endpoint: Endpoint,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TrayData {
    owner: usize,
    id: u32,
    callback: u32,
    flags: u32,
    version: u32,
    icon: usize,
}

struct Remote {
    process: HANDLE,
    buffer: *mut std::ffi::c_void,
    pid: u32,
    uncertain: Cell<bool>,
}
impl Drop for Remote {
    fn drop(&mut self) {
        unsafe {
            // A timed-out window procedure may still own the pointer. Retain
            // this 8 KiB buffer and stop querying this Explorer instance rather
            // than freeing memory it could still write to. Explorer reclaims it
            // on exit; no retry allocations are allowed for the blocked PID.
            if !self.uncertain.get() {
                VirtualFreeEx(self.process, self.buffer, 0, MEM_RELEASE);
            }
            CloseHandle(self.process);
        }
    }
}
impl Remote {
    unsafe fn open(toolbar: HWND) -> Result<Self> {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(toolbar, &mut pid);
            if pid == 0 {
                bail!("托盘宿主已退出");
            }
            if BLOCKED_PID.load(Ordering::Relaxed) == pid as usize {
                let now = GetTickCount64();
                if now.saturating_sub(BLOCKED_AT.load(Ordering::Relaxed)) < 5000 {
                    bail!("Explorer 托盘查询超时，稍后重试");
                }
                BLOCKED_AT.store(now, Ordering::Relaxed);
                // Probe without a remote-memory pointer. The timed-out allocation
                // stays quarantined by Remote::drop; retries use a fresh buffer.
                message(toolbar as _, WM_NULL, 0, 0)?;
                ultra_bar_native_ui::diagnostics::event(
                    "tray",
                    "bridge.resumed",
                    serde_json::json!({"explorer_pid":pid,"toolbar":toolbar as usize,"backoff_ms":5000}),
                );
                BLOCKED_PID.store(0, Ordering::Relaxed);
            }
            let process = OpenProcess(
                PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_VM_OPERATION
                    | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if process.is_null() {
                bail!("无法读取 Explorer 托盘（错误 {}）", GetLastError());
            }
            let mut machine = 0;
            let mut native = 0;
            if IsWow64Process2(process, &mut machine, &mut native) == 0
                || machine != 0
                || native != 0x8664
                || size_of::<usize>() != 8
            {
                CloseHandle(process);
                bail!("当前托盘桥接仅支持 x64 Explorer");
            }
            let mut path = [0u16; 32768];
            let mut length = path.len() as u32;
            let valid = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0
                && String::from_utf16_lossy(&path[..length as usize])
                    .to_lowercase()
                    .ends_with("\\explorer.exe");
            if !valid {
                CloseHandle(process);
                bail!("托盘宿主不是 Explorer");
            }
            let buffer = VirtualAllocEx(
                process,
                null(),
                8192,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            if buffer.is_null() {
                CloseHandle(process);
                bail!("无法分配托盘查询缓冲区");
            }
            Ok(Self {
                process,
                buffer,
                pid,
                uncertain: Cell::new(false),
            })
        }
    }
    unsafe fn query(&self, hwnd: HWND, msg: u32, w: usize) -> Result<usize> {
        if self.uncertain.get() {
            bail!("托盘查询已暂停");
        }
        let result = unsafe { message(hwnd, msg, w, self.buffer as isize) };
        if result.is_err() {
            self.uncertain.set(true);
            BLOCKED_PID.store(self.pid as usize, Ordering::Relaxed);
            BLOCKED_AT.store(unsafe { GetTickCount64() }, Ordering::Relaxed);
        }
        result
    }
    unsafe fn read<T: Copy>(&self, address: usize) -> Result<T> {
        unsafe {
            let mut result: T = zeroed();
            let mut read = 0;
            if ReadProcessMemory(
                self.process,
                address as _,
                (&mut result as *mut T).cast(),
                size_of::<T>(),
                &mut read,
            ) == 0
                || read != size_of::<T>()
            {
                bail!("托盘结构读取失败");
            }
            Ok(result)
        }
    }
    unsafe fn write<T>(&self, value: &T) -> Result<()> {
        unsafe {
            let mut written = 0;
            if WriteProcessMemory(
                self.process,
                self.buffer,
                (value as *const T).cast(),
                size_of::<T>(),
                &mut written,
            ) == 0
                || written != size_of::<T>()
            {
                bail!("托盘查询参数写入失败");
            }
            Ok(())
        }
    }
    unsafe fn button(&self, toolbar: HWND, index: usize) -> Result<(TBBUTTON, TrayData)> {
        unsafe {
            if self.query(toolbar, TB_GETBUTTON, index)? == 0 {
                bail!("托盘按钮已移除");
            }
            let button: TBBUTTON = self.read(self.buffer as usize)?;
            if button.dwData < 65536 {
                bail!("不支持的托盘按钮结构");
            }
            let data: TrayData = self.read(button.dwData)?;
            if data.owner == 0 || data.callback < WM_USER {
                bail!("不支持的 Explorer 托盘数据格式");
            }
            Ok((button, data))
        }
    }
    unsafe fn title(&self, toolbar: HWND, command: i32) -> String {
        unsafe {
            let text_address = self.buffer.byte_add(512);
            let empty = [0u16; 512];
            let mut written = 0;
            if WriteProcessMemory(
                self.process,
                text_address,
                empty.as_ptr().cast(),
                size_of_val(&empty),
                &mut written,
            ) == 0
            {
                return String::new();
            }
            let info = TBBUTTONINFOW {
                cbSize: size_of::<TBBUTTONINFOW>() as _,
                dwMask: TBIF_TEXT,
                pszText: text_address.cast(),
                cchText: 512,
                ..zeroed()
            };
            if self.write(&info).is_err()
                || !self
                    .query(toolbar, TB_GETBUTTONINFOW, command as _)
                    .is_ok_and(|value| value != usize::MAX)
            {
                return String::new();
            }
            let Ok(text) = self.read::<[u16; 512]>(text_address as usize) else {
                return String::new();
            };
            String::from_utf16_lossy(
                &text[..text.iter().position(|ch| *ch == 0).unwrap_or(text.len())],
            )
        }
    }
}

unsafe fn message(hwnd: HWND, msg: u32, w: usize, l: isize) -> Result<usize> {
    unsafe {
        let started = GetTickCount64();
        let mut result = 0;
        if SendMessageTimeoutW(
            hwnd,
            msg,
            w,
            l,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            500,
            &mut result,
        ) == 0
        {
            let error = GetLastError();
            ultra_bar_native_ui::diagnostics::event(
                "tray",
                "query.failed",
                serde_json::json!({"hwnd":hwnd as usize,"message_id":msg,"win32_error":error,"elapsed_ms":GetTickCount64()-started}),
            );
            bail!("Explorer 托盘查询失败（错误 {}，消息 {msg}）", error);
        }
        Ok(result)
    }
}

unsafe extern "system" fn collect(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as _);
        if String::from_utf16_lossy(&class[..length.max(0) as usize]) == "ToolbarWindow32" {
            (*(param as *mut Vec<usize>)).push(hwnd as usize);
        }
    }
    1
}

pub fn enumerate() -> Result<Vec<Item>> {
    unsafe {
        let main = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        let notify = FindWindowExW(main, null_mut(), wide("TrayNotifyWnd").as_ptr(), null());
        let overflow = FindWindowW(wide("NotifyIconOverflowWindow").as_ptr(), null());
        MAIN_ROOT.store(main as usize, Ordering::Relaxed);
        OVERFLOW_ROOT.store(overflow as usize, Ordering::Relaxed);
        let mut items = Vec::new();
        let mut found = 0;
        let mut rejected = 0;
        for (root, collapsed) in [(notify, false), (overflow, true)] {
            if root.is_null() {
                continue;
            }
            let mut bars: Vec<usize> = Vec::new();
            EnumChildWindows(root, Some(collect), (&mut bars as *mut Vec<usize>) as _);
            for toolbar in bars {
                if collapsed {
                    OVERFLOW.store(toolbar, Ordering::Relaxed);
                } else {
                    MAIN.store(toolbar, Ordering::Relaxed);
                }
                let count = message(toolbar as _, TB_BUTTONCOUNT, 0, 0)?;
                if count > 1024 {
                    bail!("不支持的托盘按钮数量");
                }
                found += count;
                if count == 0 {
                    continue;
                }
                let remote = Remote::open(toolbar as _)?;
                for index in 0..count {
                    let Ok((button, data)) = remote.button(toolbar as _, index) else {
                        rejected += 1;
                        continue;
                    };
                    if IsWindow(data.owner as _) == 0 {
                        continue;
                    }
                    if super::observer::is_probe_owner(data.owner as _) {
                        continue;
                    }
                    let mut pid = 0;
                    GetWindowThreadProcessId(data.owner as _, &mut pid);
                    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                    if process.is_null() {
                        rejected += 1;
                        continue;
                    }
                    let mut path = [0u16; 32768];
                    let mut length = path.len() as u32;
                    let readable =
                        QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
                    CloseHandle(process);
                    if !readable {
                        rejected += 1;
                        continue;
                    }
                    let executable = String::from_utf16_lossy(&path[..length as usize]);
                    let preference_key = super::store::identity(&executable, data.id);
                    let key = format!("{preference_key}@{:x}", data.owner);
                    if items.iter().any(|item: &Item| item.key == key) {
                        continue;
                    }
                    let mut title = remote.title(toolbar as _, button.idCommand);
                    if title.is_empty() {
                        title = Path::new(&executable)
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                    }
                    let mut icon = CopyIcon(data.icon as _);
                    if icon.is_null() {
                        icon = executable_icon(&executable);
                    }
                    items.push(Item {
                        key,
                        preference_key,
                        executable,
                        uid: data.id,
                        title,
                        collapsed,
                        icon: std::sync::Arc::new(Icon::owned(icon)),
                        endpoint: Endpoint {
                            toolbar,
                            command: button.idCommand,
                            owner: data.owner,
                            id: data.id,
                            pid,
                        },
                    });
                }
            }
        }
        if found == 0 && notify.is_null() {
            bail!("当前 Explorer 没有兼容的原生托盘接口");
        }
        SKIPPED.store(rejected, Ordering::Relaxed);
        if rejected > 0 && items.is_empty() {
            bail!("当前托盘图标的数据格式无法识别");
        }
        Ok(items)
    }
}
pub(super) fn owner_identity(item: &Item) -> (usize, u32, u32) {
    (item.endpoint.owner, item.uid, item.endpoint.pid)
}
#[cfg(test)]
pub(super) fn sample_item(owner: usize, id: u32, pid: u32, icon: std::sync::Arc<Icon>) -> Item {
    Item {
        key: "sample".into(),
        preference_key: "sample".into(),
        executable: "sample.exe".into(),
        uid: id,
        title: "sample".into(),
        collapsed: false,
        icon,
        endpoint: Endpoint {
            toolbar: 0,
            command: 0,
            owner,
            id,
            pid,
        },
    }
}

#[derive(Clone, Copy)]
pub enum Click {
    Left,
    Right,
    Double,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HoverEvent {
    Move,
    Open,
    Close,
}
pub struct HoverTarget {
    data: TrayData,
    pid: u32,
    toolbar: HWND,
}
impl HoverTarget {
    pub fn new(endpoint: &Endpoint) -> Result<Self> {
        unsafe {
            let remote = Remote::open(endpoint.toolbar as _)?;
            let index = message(
                endpoint.toolbar as _,
                TB_COMMANDTOINDEX,
                endpoint.command as usize,
                0,
            )?;
            if index > 1024 {
                bail!("托盘图标已移除");
            }
            let (_, data) = remote.button(endpoint.toolbar as _, index)?;
            let mut pid = 0;
            GetWindowThreadProcessId(data.owner as _, &mut pid);
            if data.owner != endpoint.owner
                || data.id != endpoint.id
                || pid != endpoint.pid
                || data.callback == 0
            {
                bail!("托盘图标已变更");
            }
            Ok(Self {
                data,
                pid,
                toolbar: endpoint.toolbar as _,
            })
        }
    }
    pub fn relay(&self, event: HoverEvent, point: (i32, i32)) -> Result<()> {
        unsafe {
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(self.data.owner as _, &mut pid);
            if pid == 0 || pid != self.pid {
                bail!("托盘程序已经退出");
            }
            if event != HoverEvent::Close {
                let foreground = GetForegroundWindow();
                let mut foreground_pid = 0;
                let foreground_thread = GetWindowThreadProcessId(foreground, &mut foreground_pid);
                let mut info: GUITHREADINFO = zeroed();
                info.cbSize = size_of::<GUITHREADINFO>() as u32;
                let available = GetGUIThreadInfo(foreground_thread, &mut info) != 0;
                let mut class = [0u16; 128];
                let n = GetClassNameW(info.hwndFocus, class.as_mut_ptr(), 128);
                eprintln!(
                    "[tray-hover-context] event={event:?} pid={pid} foreground={:#x} foreground_pid={foreground_pid} gui={available} focus={:#x} focus_class={} capture={:#x} flags={:x}",
                    foreground as usize,
                    info.hwndFocus as usize,
                    String::from_utf16_lossy(&class[..n.max(0) as usize]),
                    info.hwndCapture as usize,
                    info.flags
                );
            }
            if event != HoverEvent::Close {
                align_hover_anchor(self.toolbar, &self.data, point);
            }
            let transfer_to_preview = event == HoverEvent::Close
                && super::hover_popup::visible(self.data.owner, pid, self.data.id);
            super::hover_popup::source(self.data.owner, pid, self.data.id, thread, point, event);
            if transfer_to_preview {
                return Ok(());
            }
            let event = match event {
                HoverEvent::Move => WM_MOUSEMOVE,
                HoverEvent::Open if self.data.version == 4 => 0x400 + 6,
                HoverEvent::Close if self.data.version == 4 => 0x400 + 7,
                _ => return Ok(()),
            };
            // Explorer can finish laying out the transparent toolbar after the
            // entry notification. Qt-based v4 clients open previews on MOUSEMOVE,
            // rather than NIN_POPUPOPEN, so repeat it once at the hover deadline
            // after refreshing the native anchor. Legacy clients must not receive
            // this repeat: some rebuild their preview for every mouse move.
            if event == 0x400 + 6
                && !super::hover_popup::visible(self.data.owner, pid, self.data.id)
            {
                let (w, l) = notification_packet(
                    self.data.id,
                    self.data.version,
                    WM_MOUSEMOVE,
                    POINT {
                        x: point.0,
                        y: point.1,
                    },
                );
                if PostMessageW(self.data.owner as _, self.data.callback, w, l) == 0 {
                    bail!("无法重发托盘悬停通知");
                }
            }
            let (w, l) = notification_packet(
                self.data.id,
                self.data.version,
                event,
                POINT {
                    x: point.0,
                    y: point.1,
                },
            );
            if PostMessageW(self.data.owner as _, self.data.callback, w, l) == 0 {
                bail!("无法发送托盘悬停通知");
            }
            Ok(())
        }
    }
}
const ANCHOR_X: &str = "UltraBar.TrayAnchor.X.v1";
const ANCHOR_Y: &str = "UltraBar.TrayAnchor.Y.v1";
unsafe fn restore_hover_anchor(hwnd: HWND) {
    unsafe {
        let x = GetPropW(hwnd, wide(ANCHOR_X).as_ptr()) as usize;
        let y = GetPropW(hwnd, wide(ANCHOR_Y).as_ptr()) as usize;
        if x != 0 && y != 0 {
            SetWindowPos(
                hwnd,
                null_mut(),
                (x as i64 - 0x80000001) as i32,
                (y as i64 - 0x80000001) as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        RemovePropW(hwnd, wide(ANCHOR_X).as_ptr());
        RemovePropW(hwnd, wide(ANCHOR_Y).as_ptr());
    }
}
pub unsafe fn restore_hover_positions(root: HWND) {
    unsafe extern "system" fn restore(hwnd: HWND, _: LPARAM) -> BOOL {
        unsafe {
            restore_hover_anchor(hwnd);
        }
        1
    }
    unsafe {
        EnumChildWindows(root, Some(restore), 0);
    }
}
impl Drop for HoverTarget {
    fn drop(&mut self) {
        unsafe {
            restore_hover_anchor(self.toolbar);
        }
    }
}
unsafe fn align_hover_anchor(toolbar: HWND, data: &TrayData, point: (i32, i32)) {
    unsafe {
        let root = GetAncestor(toolbar, GA_ROOT);
        // Only touch the invisible Explorer toolbar managed by our takeover guard.
        if GetPropW(
            root,
            wide("UltraBar.NativeTaskbar.OriginalExStyle.v1").as_ptr(),
        )
        .is_null()
        {
            return;
        }
        let identity = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: data.owner as _,
            uID: data.id,
            guidItem: zeroed(),
        };
        let mut icon: RECT = zeroed();
        if Shell_NotifyIconGetRect(&identity, &mut icon) < 0 {
            return;
        }
        let dx = point.0 - (icon.left + icon.right) / 2;
        let dy = point.1 - (icon.top + icon.bottom) / 2;
        if dx == 0 && dy == 0 {
            return;
        }
        let mut rect: RECT = zeroed();
        if GetWindowRect(toolbar, &mut rect) == 0 {
            return;
        }
        let mut origin = POINT {
            x: rect.left,
            y: rect.top,
        };
        ScreenToClient(GetParent(toolbar), &mut origin);
        if GetPropW(toolbar, wide(ANCHOR_X).as_ptr()).is_null() {
            if SetPropW(
                toolbar,
                wide(ANCHOR_X).as_ptr(),
                (origin.x as i64 + 0x80000001) as usize as _,
            ) == 0
            {
                return;
            }
            if SetPropW(
                toolbar,
                wide(ANCHOR_Y).as_ptr(),
                (origin.y as i64 + 0x80000001) as usize as _,
            ) == 0
            {
                RemovePropW(toolbar, wide(ANCHOR_X).as_ptr());
                return;
            }
        }
        SetWindowPos(
            toolbar,
            null_mut(),
            origin.x + dx,
            origin.y + dy,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let result = Shell_NotifyIconGetRect(&identity, &mut icon);
        if result < 0 {
            restore_hover_anchor(toolbar);
        }
        eprintln!(
            "[tray-anchor] id={} hr={result:x} cursor={point:?} rect={},{},{},{}",
            data.id, icon.left, icon.top, icon.right, icon.bottom
        );
    }
}
fn notification_packet(id: u32, version: u32, event: u32, point: POINT) -> (usize, isize) {
    if version == 4 {
        let position = (point.x as u16 as u32) | ((point.y as u16 as u32) << 16);
        (
            position as usize,
            ((id as u16 as u32) << 16 | event & 0xffff) as isize,
        )
    } else {
        (id as usize, event as isize)
    }
}
pub fn diagnose_details(path: &Path) -> Result<()> {
    unsafe {
        let main = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        let notify = FindWindowExW(main, null_mut(), wide("TrayNotifyWnd").as_ptr(), null());
        let overflow = FindWindowW(wide("NotifyIconOverflowWindow").as_ptr(), null());
        let mut rows = vec![];
        for (root, collapsed) in [(notify, false), (overflow, true)] {
            if root.is_null() {
                continue;
            }
            let mut bars = vec![];
            EnumChildWindows(root, Some(collect), (&mut bars as *mut Vec<usize>) as _);
            for toolbar in bars {
                let count = message(toolbar as _, TB_BUTTONCOUNT, 0, 0)?.min(1024);
                let remote = Remote::open(toolbar as _)?;
                for index in 0..count {
                    remote.query(toolbar as _, TB_GETBUTTON, index)?;
                    let button: TBBUTTON = remote.read(remote.buffer as usize)?;
                    let data: TrayData = remote.read(button.dwData)?;
                    let raw = remote.read::<[u64; 4]>(button.dwData).unwrap_or_default();
                    let icon = CopyIcon(data.icon as _);
                    let copied = !icon.is_null();
                    if copied {
                        DestroyIcon(icon);
                    }
                    let mut pid = 0;
                    GetWindowThreadProcessId(data.owner as _, &mut pid);
                    rows.push(serde_json::json!({"collapsed":collapsed,"index":index,"command":button.idCommand,"bitmap":button.iBitmap,"title":remote.title(toolbar as _,button.idCommand),"owner":format!("{:x}",data.owner),"owner_valid":IsWindow(data.owner as _)!=0,"pid":pid,"id":data.id,"callback":data.callback,"icon":format!("{:x}",data.icon),"copy_icon_ok":copied,"raw_words":raw.iter().map(|n|format!("{n:016x}")).collect::<Vec<_>>()}));
                }
            }
        }
        let items = enumerate()?;
        let width = 640;
        let height = ((items.len().div_ceil(10) as i32) * 54).max(54);
        let surface = crate::visual::Surface::new(width, height)
            .ok_or_else(|| anyhow::anyhow!("Cannot render tray diagnostic"))?;
        let area = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        use windows_sys::Win32::Graphics::Gdi::*;
        FillRect(surface.dc, &area, crate::visual::theme::background_brush());
        SetBkMode(surface.dc, TRANSPARENT as _);
        SetTextColor(surface.dc, crate::visual::theme::palette().text);
        let font = SelectObject(surface.dc, GetStockObject(DEFAULT_GUI_FONT));
        let mut rendered = vec![];
        for (i, item) in items.iter().enumerate() {
            let x = (i % 10) as i32 * 64;
            let y = (i / 10) as i32 * 54;
            let icon_ok = DrawIconEx(
                surface.dc,
                x + 24,
                y + 4,
                item.icon.handle(),
                16,
                16,
                0,
                null_mut(),
                DI_NORMAL,
            ) != 0;
            let app = Path::new(&item.executable)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let mut label = RECT {
                left: x + 2,
                top: y + 24,
                right: x + 62,
                bottom: y + 48,
            };
            crate::visual::text::draw_text(
                surface.dc,
                wide(&app).as_ptr(),
                -1,
                &mut label,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            rendered.push(serde_json::json!({"app":app,"drawable":icon_ok,"id":item.uid}));
        }
        SelectObject(surface.dc, font);
        surface.save_bmp(&path.with_extension("bmp"))?;
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({"buttons":rows,"icons":rendered}))?,
        )?;
        Ok(())
    }
}

pub fn relay(endpoint: &Endpoint, click: Click) -> Result<()> {
    unsafe {
        if IsWindow(endpoint.owner as _) == 0 {
            bail!("托盘程序已经退出");
        }
        let mut current_pid = 0;
        GetWindowThreadProcessId(endpoint.owner as _, &mut current_pid);
        if current_pid != endpoint.pid {
            bail!("托盘程序已更换，请刷新后重试");
        }
        let remote = Remote::open(endpoint.toolbar as _)?;
        let index = message(
            endpoint.toolbar as _,
            TB_COMMANDTOINDEX,
            endpoint.command as usize,
            0,
        )?;
        if index > 1024 {
            bail!("托盘图标已移除");
        }
        let (_, data) = remote.button(endpoint.toolbar as _, index)?;
        if data.owner != endpoint.owner || data.id != endpoint.id {
            bail!("托盘图标已变更，请刷新后重试");
        }
        let mut pid = 0;
        GetWindowThreadProcessId(endpoint.owner as _, &mut pid);
        AllowSetForegroundWindow(pid);
        if matches!(click, Click::Right) && data.version == 4 {
            let mut point: POINT = zeroed();
            GetCursorPos(&mut point);
            let (w, l) = notification_packet(data.id, data.version, WM_CONTEXTMENU, point);
            if PostMessageW(data.owner as _, data.callback, w, l) == 0 {
                bail!("无法发送版本 4 托盘菜单通知（错误 {}）", GetLastError());
            }
            eprintln!("[tray] context menu version=4 pid={pid} id={}", data.id);
            return Ok(());
        }
        if remote.query(endpoint.toolbar as _, TB_GETITEMRECT, index)? == 0 {
            bail!("无法定位托盘图标");
        }
        let rect: RECT = remote.read(remote.buffer as usize)?;
        let x = (rect.left + rect.right) / 2;
        let y = (rect.top + rect.bottom) / 2;
        let location = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
        let (down, up, flags) = match click {
            Click::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, 1),
            Click::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, 2),
            Click::Double => (WM_LBUTTONDBLCLK, WM_LBUTTONUP, 1),
        };
        // Relay only to Explorer's specific toolbar. Explorer retains the icon's
        // callback version/packing; no global input and no guessed callbacks.
        if PostMessageW(endpoint.toolbar as _, WM_MOUSEMOVE, 0, location) == 0
            || PostMessageW(endpoint.toolbar as _, down, flags, location) == 0
            || PostMessageW(endpoint.toolbar as _, up, 0, location) == 0
        {
            bail!("无法转发托盘操作");
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_restores_saved_toolbar_position_and_removes_properties() {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -120,
                80,
                100,
                30,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            assert!(!hwnd.is_null());
            let mut original: RECT = zeroed();
            GetWindowRect(hwnd, &mut original);
            assert_ne!(
                SetPropW(
                    hwnd,
                    wide(ANCHOR_X).as_ptr(),
                    (original.left as i64 + 0x80000001) as usize as _
                ),
                0
            );
            assert_ne!(
                SetPropW(
                    hwnd,
                    wide(ANCHOR_Y).as_ptr(),
                    (original.top as i64 + 0x80000001) as usize as _
                ),
                0
            );
            SetWindowPos(
                hwnd,
                null_mut(),
                200,
                300,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            restore_hover_anchor(hwnd);
            restore_hover_anchor(hwnd);
            let mut restored: RECT = zeroed();
            GetWindowRect(hwnd, &mut restored);
            assert_eq!((restored.left, restored.top), (original.left, original.top));
            assert!(GetPropW(hwnd, wide(ANCHOR_X).as_ptr()).is_null());
            assert!(GetPropW(hwnd, wide(ANCHOR_Y).as_ptr()).is_null());
            DestroyWindow(hwnd);
        }
    }
    unsafe extern "system" fn hover_probe(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
        unsafe {
            if msg == WM_NCCREATE {
                SetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                    (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
                );
            }
            if msg == WM_APP + 88 {
                let received = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Vec<(usize, isize)>;
                if !received.is_null() {
                    (*received).push((w, l));
                }
                return 0;
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
    }
    #[test]
    fn hover_callbacks_use_the_registered_protocol_without_sending_clicks() {
        unsafe {
            let _test = crate::tray::hover_popup::TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let class = wide(&format!("UltraBar.TrayHoverProbe.{}", GetCurrentThreadId()));
            let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(hover_probe),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let mut received: Vec<(usize, isize)> = vec![];
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                module,
                (&mut received as *mut Vec<(usize, isize)>).cast(),
            );
            assert!(!hwnd.is_null());
            let mut target = HoverTarget {
                data: TrayData {
                    owner: hwnd as usize,
                    id: 42,
                    callback: WM_APP + 88,
                    flags: 0,
                    version: 4,
                    icon: 0,
                },
                pid: GetCurrentProcessId(),
                toolbar: null_mut(),
            };
            for event in [HoverEvent::Move, HoverEvent::Open, HoverEvent::Close] {
                target.relay(event, (-120, 1080)).unwrap();
            }
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, hwnd, WM_APP + 88, WM_APP + 88, PM_REMOVE) != 0 {
                DispatchMessageW(&message);
            }
            assert_eq!(received.len(), 4);
            for ((w, l), event) in received
                .iter()
                .zip([WM_MOUSEMOVE, WM_MOUSEMOVE, 0x406, 0x407])
            {
                assert_eq!(*w as u16 as i16, -120);
                assert_eq!((*w >> 16) as u16 as i16, 1080);
                assert_eq!(*l as u16 as u32, event);
                assert_eq!((*l >> 16) as u16, 42);
            }
            target.data.version = 0;
            target.data.id = 0x12345678;
            for event in [HoverEvent::Move, HoverEvent::Open, HoverEvent::Close] {
                target.relay(event, (1, 2)).unwrap();
            }
            while PeekMessageW(&mut message, hwnd, WM_APP + 88, WM_APP + 88, PM_REMOVE) != 0 {
                DispatchMessageW(&message);
            }
            assert_eq!(received.len(), 5);
            assert_eq!(received[4], (0x12345678, WM_MOUSEMOVE as isize));
            target.pid = GetCurrentProcessId().wrapping_add(1);
            assert!(target.relay(HoverEvent::Move, (0, 0)).is_err());
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
    #[test]
    fn modern_menu_notification_preserves_id_and_signed_screen_anchor() {
        let (w, l) = notification_packet(42, 4, WM_CONTEXTMENU, POINT { x: -120, y: 1080 });
        assert_eq!(w as u16 as i16, -120);
        assert_eq!((w >> 16) as u16 as i16, 1080);
        assert_eq!(l as u16 as u32, WM_CONTEXTMENU);
        assert_eq!((l >> 16) as u16, 42);
    }
    #[test]
    fn legacy_notifications_keep_the_full_identifier() {
        let (w, l) = notification_packet(0x12345678, 0, WM_RBUTTONUP, POINT { x: 12, y: 34 });
        assert_eq!(w, 0x12345678);
        assert_eq!(l, WM_RBUTTONUP as isize);
    }
}
