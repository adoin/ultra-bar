//! Read-only Explorer ITrayNotifyWin8 subscription. No Explorer injection, settings writes or frame polling.
//! ABI reference: Chromium status-icons / ysc3839 ITrayNotify.cpp (Windows 10).
use super::{
    Command,
    native::{self, Icon, Item},
};
use std::{
    collections::HashMap,
    ffi::c_void,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::SyncSender,
    },
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::{Com::*, Threading::*},
        UI::WindowsAndMessaging::*,
    },
    core::GUID,
};
const CLASS: GUID = GUID::from_u128(0x25dead04_1eac_4911_9e3a_ad0a4ab560fd);
const TRAY8: GUID = GUID::from_u128(0xd133ce13_3537_48ba_93a7_afcd5d2053b4);
const CALLBACK: GUID = GUID::from_u128(0xd782ccba_afb0_43f1_94db_fda3779eaccb);
const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const RECONNECT: u32 = WM_APP + 0x6d;
const PROBE_CLASS: &str = "UltraBar.TraySync.Probe";
pub(super) fn is_probe_owner(hwnd: HWND) -> bool {
    unsafe {
        let mut name = [0u16; 64];
        let n = GetClassNameW(hwnd, name.as_mut_ptr(), 64);
        String::from_utf16_lossy(&name[..n.max(0) as usize]) == PROBE_CLASS
    }
}
#[repr(C)]
struct NotifyItem {
    executable: *const u16,
    tip: *const u16,
    icon: HICON,
    owner: HWND,
    preference: u32,
    id: u32,
    guid: GUID,
}
#[derive(Clone)]
struct Frame {
    pid: u32,
    icon: Option<Arc<Icon>>,
    tip: Option<String>,
    deleted: bool,
}
pub struct Inbox {
    latest: Mutex<HashMap<(usize, u32), Frame>>,
    sender: SyncSender<Command>,
    queued: AtomicBool,
    stopped: AtomicBool,
    thread: AtomicU32,
    explorer: AtomicU32,
    active: AtomicBool,
    finished: AtomicBool,
    pub callbacks: AtomicU64,
    include_probe: bool,
}
pub struct Subscription(Arc<Inbox>);
impl Drop for Subscription {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
        let _ = self.0.sender.try_send(Command::Stop);
        unsafe {
            PostThreadMessageW(self.0.thread.load(Ordering::Acquire), WM_QUIT, 0, 0);
        }
        let end = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !self.0.finished.load(Ordering::Acquire) && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
pub fn start(sender: SyncSender<Command>) -> (Subscription, Arc<Inbox>) {
    start_inner(sender, false)
}
fn start_inner(sender: SyncSender<Command>, include_probe: bool) -> (Subscription, Arc<Inbox>) {
    let shared = Arc::new(Inbox {
        latest: Mutex::new(HashMap::new()),
        sender,
        queued: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
        thread: AtomicU32::new(0),
        explorer: AtomicU32::new(explorer_pid()),
        active: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        callbacks: AtomicU64::new(0),
        include_probe,
    });
    let run = shared.clone();
    std::thread::spawn(move || unsafe { listen(run) });
    (Subscription(shared.clone()), shared)
}
fn explorer_pid() -> u32 {
    unsafe {
        let hwnd = FindWindowW(crate::visual::wide("Shell_TrayWnd").as_ptr(), null());
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid
    }
}
impl Inbox {
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub fn reconnect_if_changed(&self) {
        let pid = explorer_pid();
        if self.explorer.swap(pid, Ordering::AcqRel) != pid {
            unsafe {
                PostThreadMessageW(self.thread.load(Ordering::Acquire), RECONNECT, 0, 0);
            }
        }
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn apply(&self, items: &mut Vec<Item>) -> bool {
        let frames = {
            let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            self.queued.store(false, Ordering::Release);
            latest.clone()
        };
        let mut changed = false;
        items.retain(|item| {
            let (owner, id, pid) = native::owner_identity(item);
            if frames
                .get(&(owner, id))
                .is_some_and(|f| f.deleted && (f.pid == 0 || f.pid == pid))
            {
                changed = true;
                false
            } else {
                true
            }
        });
        for item in items {
            let (owner, id, pid) = native::owner_identity(item);
            let Some(frame) = frames.get(&(owner, id)) else {
                continue;
            };
            if frame.pid != pid || frame.deleted {
                continue;
            }
            if let Some(icon) = &frame.icon {
                if icon.fingerprint() != item.icon.fingerprint() {
                    changed = true;
                }
                item.icon = icon.clone();
            }
            if let Some(tip) = &frame.tip
                && !tip.is_empty()
                && item.title != *tip
            {
                item.title = tip.clone();
                changed = true;
            }
        }
        changed
    }
    unsafe fn receive(&self, event: u32, item: &NotifyItem) {
        unsafe {
            if self.stopped.load(Ordering::Acquire) || item.owner.is_null() || event > 2 {
                return;
            }
            if !self.include_probe && is_probe_owner(item.owner) {
                return;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(item.owner, &mut pid);
            if pid == 0 && event != 2 {
                return;
            }
            let icon = if event == 2 {
                None
            } else if item.icon.is_null() {
                Some(Arc::new(Icon::owned(null_mut())))
            } else {
                let copy = CopyIcon(item.icon);
                (!copy.is_null()).then(|| Arc::new(Icon::owned(copy)))
            };
            let tip = if item.tip.is_null() {
                None
            } else {
                let mut n = 0;
                while n < 512 && *item.tip.add(n) != 0 {
                    n += 1;
                }
                Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                    item.tip, n,
                )))
            };
            let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
            if latest.len() >= 1024 && !latest.contains_key(&(item.owner as usize, item.id)) {
                return;
            }
            latest.insert(
                (item.owner as usize, item.id),
                Frame {
                    pid,
                    icon,
                    tip,
                    deleted: event == 2,
                },
            );
            drop(latest);
            self.callbacks.fetch_add(1, Ordering::Relaxed);
            if event != 1 {
                native::DIRTY.store(true, Ordering::Relaxed);
            }
            if !self.queued.swap(true, Ordering::AcqRel)
                && self.sender.try_send(Command::FramesChanged).is_err()
            {
                self.queued.store(false, Ordering::Release);
            }
        }
    }
}
#[repr(C)]
struct CallbackObject {
    vtable: *const CallbackTable,
    references: AtomicU32,
    inbox: Arc<Inbox>,
}
#[repr(C)]
struct CallbackTable {
    query: unsafe extern "system" fn(*mut CallbackObject, *const GUID, *mut *mut c_void) -> i32,
    add: unsafe extern "system" fn(*mut CallbackObject) -> u32,
    release: unsafe extern "system" fn(*mut CallbackObject) -> u32,
    notify: unsafe extern "system" fn(*mut CallbackObject, u32, *const NotifyItem) -> i32,
}
unsafe extern "system" fn query(
    this: *mut CallbackObject,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> i32 {
    unsafe {
        if out.is_null() || iid.is_null() {
            return 0x80004003u32 as i32;
        }
        *out = null_mut();
        let equal = |a: &GUID, b: &GUID| {
            a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
        };
        if equal(&*iid, &UNKNOWN) || equal(&*iid, &CALLBACK) {
            *out = this.cast();
            add(this);
            0
        } else {
            0x80004002u32 as i32
        }
    }
}
unsafe extern "system" fn add(this: *mut CallbackObject) -> u32 {
    unsafe { (*this).references.fetch_add(1, Ordering::Relaxed) + 1 }
}
unsafe extern "system" fn release(this: *mut CallbackObject) -> u32 {
    unsafe {
        let left = (*this).references.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this));
        }
        left
    }
}
unsafe extern "system" fn notify(
    this: *mut CallbackObject,
    event: u32,
    item: *const NotifyItem,
) -> i32 {
    unsafe {
        if item.is_null() {
            return 0x80004003u32 as i32;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (*this).inbox.receive(event, &*item)
        }));
        if result.is_ok() {
            0
        } else {
            0x80004005u32 as i32
        }
    }
}
static CALLBACK_TABLE: CallbackTable = CallbackTable {
    query,
    add,
    release,
    notify,
};
struct Connection {
    object: *mut c_void,
    callback: *mut CallbackObject,
    cookie: u32,
    registered: bool,
}
impl Connection {
    unsafe fn method(&self, index: usize) -> *const c_void {
        unsafe { *(*(self.object as *const *const *const c_void)).add(index) }
    }
    unsafe fn connect(inbox: Arc<Inbox>) -> std::result::Result<Self, i32> {
        unsafe {
            let mut object = null_mut();
            let result =
                CoCreateInstance(&CLASS, null_mut(), CLSCTX_LOCAL_SERVER, &TRAY8, &mut object);
            if result < 0 {
                return Err(result);
            }
            let callback = Box::into_raw(Box::new(CallbackObject {
                vtable: &CALLBACK_TABLE,
                references: AtomicU32::new(1),
                inbox,
            }));
            let mut connection = Self {
                object,
                callback,
                cookie: 0,
                registered: false,
            };
            let register: unsafe extern "system" fn(
                *mut c_void,
                *mut CallbackObject,
                *mut u32,
            ) -> i32 = std::mem::transmute(connection.method(3));
            let result = register(object, callback, &mut connection.cookie);
            if result < 0 {
                return Err(result);
            }
            connection.registered = true;
            Ok(connection)
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            if self.registered {
                let unregister: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                    std::mem::transmute(self.method(4));
                unregister(self.object, &mut self.cookie);
            }
            let release_object: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(self.method(2));
            release_object(self.object);
            release(self.callback);
        }
    }
}
unsafe fn listen(inbox: Arc<Inbox>) {
    unsafe {
        if CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) < 0 {
            inbox.finished.store(true, Ordering::Release);
            return;
        }
        let mut message: MSG = zeroed();
        PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
        inbox.thread.store(GetCurrentThreadId(), Ordering::Release);
        let connect = || match Connection::connect(inbox.clone()) {
            Ok(connection) => {
                inbox.active.store(true, Ordering::Release);
                eprintln!("[tray] Explorer live icon subscription enabled");
                Some(connection)
            }
            Err(hr) => {
                inbox.active.store(false, Ordering::Release);
                eprintln!("[tray] live subscription unavailable ({hr:#x}); event bridge retained");
                None
            }
        };
        let mut connection = connect();
        while !inbox.stopped.load(Ordering::Acquire)
            && GetMessageW(&mut message, null_mut(), 0, 0) > 0
        {
            if message.message == RECONNECT {
                drop(connection.take());
                inbox
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                connection = connect();
            } else {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        drop(connection);
        inbox.active.store(false, Ordering::Release);
        CoUninitialize();
        inbox.finished.store(true, Ordering::Release);
    }
}
pub fn smoke(directory: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::{Graphics::Gdi::*, System::LibraryLoader::*, UI::Shell::*};
    unsafe {
        ensure!(
            directory.is_absolute(),
            "Absolute output directory required"
        );
        std::fs::create_dir_all(directory)?;
        let (tx, rx) = std::sync::mpsc::sync_channel(32);
        let (subscription, inbox) = start_inner(tx, true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !inbox.active() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        ensure!(inbox.active(), "Explorer live subscription unavailable");
        let (peer_tx, _peer_rx) = std::sync::mpsc::sync_channel(32);
        let (peer_subscription, peer) = start_inner(peer_tx, true);
        let mut peer_subscription = Some(peer_subscription);
        let peer_deadline = Instant::now() + Duration::from_secs(5);
        while !peer.active() && Instant::now() < peer_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        ensure!(peer.active(), "Second Explorer observer could not attach");
        let module = GetModuleHandleW(null());
        let class = crate::visual::wide(PROBE_CLASS);
        RegisterClassW(&WNDCLASSW {
            hInstance: module,
            lpszClassName: class.as_ptr(),
            lpfnWndProc: Some(DefWindowProcW),
            ..zeroed()
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            null(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            module,
            null(),
        );
        ensure!(!hwnd.is_null(), "Cannot create tray fixture");
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let _window = Window(hwnd);
        let uid = 0xfa60u32;
        let guid =
            GUID::from_u128(0x9dfca4d0_31f0_4aaf_9110_000000000000 + GetCurrentProcessId() as u128);
        let mut nid: NOTIFYICONDATAW = zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as _;
        nid.hWnd = hwnd;
        nid.uID = uid;
        nid.uFlags = NIF_GUID | NIF_ICON | NIF_MESSAGE;
        nid.guidItem = guid;
        nid.uCallbackMessage = WM_APP + 17;
        struct Registered(NOTIFYICONDATAW);
        impl Drop for Registered {
            fn drop(&mut self) {
                unsafe {
                    Shell_NotifyIconW(NIM_DELETE, &self.0);
                }
            }
        }
        let fixture_icon = |rgb: Option<u32>| -> anyhow::Result<Arc<Icon>> {
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as _,
                biWidth: 16,
                biHeight: -16,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            };
            let mut bits = null_mut();
            let bitmap =
                CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            ensure!(!bitmap.is_null(), "Cannot create avatar fixture");
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), 16 * 16);
            for y in 0..16 {
                for x in 0..16 {
                    pixels[y * 16 + x] = rgb
                        .map(|color| {
                            if (x as i32 - 8).pow(2) + (y as i32 - 8).pow(2) < 45 {
                                0xff000000
                                    | if y > 8 && x > 3 && x < 13 {
                                        0xf8f8f8
                                    } else {
                                        color
                                    }
                            } else {
                                0
                            }
                        })
                        .unwrap_or(0);
                }
            }
            let mut and_mask = [0xffu8; 32];
            if rgb.is_some() {
                for y in 0..16 {
                    for x in 0..16 {
                        if (x as i32 - 8).pow(2) + (y as i32 - 8).pow(2) < 45 {
                            and_mask[y * 2 + x / 8] &= !(0x80 >> (x % 8));
                        }
                    }
                }
            }
            let mask = CreateBitmap(16, 16, 1, 1, and_mask.as_ptr().cast());
            let icon = CreateIconIndirect(&ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: bitmap,
            });
            DeleteObject(bitmap);
            DeleteObject(mask);
            ensure!(!icon.is_null(), "Cannot create tray fixture icon");
            Ok(Arc::new(Icon::owned(icon)))
        };
        let mut registration = None;
        let mut stages = vec![];
        for (index, color) in [Some(0x287cdd), None, Some(0xd34485), None, Some(0x287cdd)]
            .into_iter()
            .enumerate()
        {
            let icon = fixture_icon(color)?;
            let expected = icon.fingerprint();
            nid.hIcon = icon.handle();
            let started = Instant::now();
            ensure!(
                Shell_NotifyIconW(if index == 0 { NIM_ADD } else { NIM_MODIFY }, &nid) != 0,
                "Cannot update tray fixture"
            );
            if index == 0 {
                registration = Some(Registered(nid));
            }
            // Match real clients that destroy the input HICON immediately after Shell_NotifyIcon.
            drop(icon);
            let end = Instant::now() + Duration::from_secs(3);
            let mut received = false;
            while Instant::now() < end {
                while rx.try_recv().is_ok() {}
                inbox.queued.store(false, Ordering::Release);
                let frame = inbox
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(hwnd as usize, uid))
                    .cloned();
                if frame
                    .and_then(|f| f.icon)
                    .is_some_and(|i| i.fingerprint() == expected)
                    && (index > 2
                        || peer
                            .latest
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&(hwnd as usize, uid))
                            .and_then(|f| f.icon.as_ref())
                            .is_some_and(|i| i.fingerprint() == expected))
                {
                    received = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            stages.push(serde_json::json!({"stage":index,"transparent_frame":color.is_none(),"received":received,"latency_ms":started.elapsed().as_millis(),"fingerprint":format!("{expected:016x}")}));
            let current = inbox
                .latest
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(hwnd as usize, uid))
                .cloned();
            if let Some(image) = current.and_then(|f| f.icon) {
                stages.last_mut().unwrap()["observed_fingerprint"] =
                    serde_json::json!(format!("{:016x}", image.fingerprint()));
                let canvas =
                    crate::visual::Surface::new(32, 32).context("Cannot render received avatar")?;
                let area = RECT {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 32,
                };
                FillRect(canvas.dc, &area, crate::visual::theme::background_brush());
                DrawIconEx(
                    canvas.dc,
                    8,
                    8,
                    image.handle(),
                    16,
                    16,
                    0,
                    null_mut(),
                    DI_NORMAL,
                );
                canvas.save_bmp(&directory.join(format!("received-{index}.bmp")))?;
            }
            std::thread::sleep(Duration::from_millis(80));
            if index == 2 {
                drop(peer_subscription.take());
            }
        }
        drop(registration);
        let stopped_at = Instant::now();
        drop(subscription);
        while inbox.active() && stopped_at.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let success = stages.iter().all(|s| s["received"] == true);
        std::fs::write(
            directory.join("tray-sync.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"all_frames_received":success,"stages":stages,"callbacks":inbox.callbacks.load(Ordering::Relaxed),"unregistered":!inbox.active(),"no_global_input":true,"independent_subscriptions":success&&!peer.active()}),
            )?,
        )?;
        ensure!(
            success,
            "Tray avatar/blank frame stream did not match Shell_NotifyIcon updates"
        );
        ensure!(!inbox.active(), "Tray observer failed to detach");
        let _ = std::fs::metadata(directory).context("Missing tray output")?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn inbox() -> Arc<Inbox> {
        let (sender, _) = std::sync::mpsc::sync_channel(4);
        Arc::new(Inbox {
            latest: Mutex::new(HashMap::new()),
            sender,
            queued: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            thread: AtomicU32::new(0),
            explorer: AtomicU32::new(0),
            active: AtomicBool::new(false),
            finished: AtomicBool::new(true),
            callbacks: AtomicU64::new(0),
            include_probe: true,
        })
    }
    unsafe fn icon(id: *const u16) -> Arc<Icon> {
        unsafe { Arc::new(Icon::owned(CopyIcon(LoadIconW(null_mut(), id)))) }
    }
    #[test]
    fn avatar_blank_and_restore_keep_the_same_clickable_slot() {
        unsafe {
            let inbox = inbox();
            let pid = GetCurrentProcessId();
            let owner = 0x1234;
            let avatar = icon(IDI_INFORMATION);
            let expected = avatar.fingerprint();
            let mut items = vec![native::sample_item(owner, 7, pid, icon(IDI_WARNING))];
            inbox.latest.lock().unwrap().insert(
                (owner, 7),
                Frame {
                    pid,
                    icon: Some(avatar.clone()),
                    tip: None,
                    deleted: false,
                },
            );
            assert!(inbox.apply(&mut items));
            assert_eq!(items[0].icon.fingerprint(), expected);
            inbox
                .latest
                .lock()
                .unwrap()
                .get_mut(&(owner, 7))
                .unwrap()
                .icon = Some(Arc::new(Icon::owned(null_mut())));
            assert!(inbox.apply(&mut items));
            assert_eq!(items.len(), 1);
            assert!(items[0].icon.handle().is_null());
            // A later metadata scan must not substitute the executable logo into the blank blink frame.
            items[0].icon = icon(IDI_WARNING);
            assert!(inbox.apply(&mut items));
            assert!(items[0].icon.handle().is_null());
            inbox
                .latest
                .lock()
                .unwrap()
                .get_mut(&(owner, 7))
                .unwrap()
                .icon = Some(avatar);
            assert!(inbox.apply(&mut items));
            assert_eq!(items[0].icon.fingerprint(), expected);
            assert_eq!(items[0].uid, 7);
        }
    }
    #[test]
    fn identical_images_skip_repaint_and_stale_process_events_are_ignored() {
        unsafe {
            let inbox = inbox();
            let pid = GetCurrentProcessId();
            let owner = 0x1234;
            let mut items = vec![native::sample_item(owner, 8, pid, icon(IDI_WARNING))];
            inbox.latest.lock().unwrap().insert(
                (owner, 8),
                Frame {
                    pid,
                    icon: Some(icon(IDI_WARNING)),
                    tip: None,
                    deleted: false,
                },
            );
            assert!(!inbox.apply(&mut items));
            let expected = items[0].icon.fingerprint();
            inbox.latest.lock().unwrap().insert(
                (owner, 8),
                Frame {
                    pid: pid + 1,
                    icon: Some(icon(IDI_INFORMATION)),
                    tip: None,
                    deleted: false,
                },
            );
            assert!(!inbox.apply(&mut items));
            assert_eq!(items[0].icon.fingerprint(), expected);
            inbox.latest.lock().unwrap().insert(
                (owner, 8),
                Frame {
                    pid: 0,
                    icon: None,
                    tip: None,
                    deleted: true,
                },
            );
            assert!(inbox.apply(&mut items));
            assert!(items.is_empty());
        }
    }
}
