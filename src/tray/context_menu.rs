//! Brief focus handoff to a tray application's menu, without mouse hooks or focus stealing.
use std::{
    mem::zeroed,
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::*, UI::WindowsAndMessaging::*};
pub const TIMER: usize = 12;
pub enum Decision {
    Wait,
    Resume,
    Hide,
}
pub struct Handoff {
    pid: u32,
    thread: u32,
    panel: usize,
    baseline: Vec<usize>,
    started: Instant,
    last_menu: Option<Instant>,
}
struct Scan<'a> {
    panel: usize,
    baseline: &'a [usize],
    visible: Vec<usize>,
    found: bool,
}
unsafe extern "system" fn scan(hwnd: HWND, param: isize) -> i32 {
    unsafe {
        let state = &mut *(param as *mut Scan<'_>);
        if hwnd as usize == state.panel || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        state.visible.push(hwnd as usize);
        let mut name = [0u16; 128];
        let n = GetClassNameW(hwnd, name.as_mut_ptr(), 128);
        let class = String::from_utf16_lossy(&name[..n.max(0) as usize]).to_ascii_lowercase();
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let popup = style & WS_POPUP != 0
            && style & WS_CAPTION != WS_CAPTION
            && (!GetWindow(hwnd, GW_OWNER).is_null()
                || GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0);
        state.found |= class == "#32768"
            || (!state.baseline.contains(&(hwnd as usize))
                && (class.contains("popup") || class.contains("menu") || popup));
        1
    }
}
unsafe fn in_menu(thread: u32) -> bool {
    unsafe {
        if thread == 0 {
            return false;
        }
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        GetGUIThreadInfo(thread, &mut info) != 0
            && (info.flags & (GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE) != 0
                || !info.hwndMenuOwner.is_null())
    }
}
impl Handoff {
    #[cfg(test)]
    pub(super) fn expire_for_test(&mut self) {
        self.last_menu = Some(Instant::now() - Duration::from_secs(1));
    }
    pub fn observed(&self) -> bool {
        self.last_menu.is_some()
    }
    pub unsafe fn new(owner: HWND, pid: u32, panel: HWND) -> Self {
        unsafe {
            let thread = GetWindowThreadProcessId(owner, std::ptr::null_mut());
            let mut baseline = Scan {
                panel: panel as usize,
                baseline: &[],
                visible: vec![],
                found: false,
            };
            if thread != 0 {
                EnumThreadWindows(
                    thread,
                    Some(scan),
                    (&mut baseline as *mut Scan<'_>) as isize,
                );
            }
            Self {
                pid,
                thread,
                panel: panel as usize,
                baseline: baseline.visible,
                started: Instant::now(),
                last_menu: None,
            }
        }
    }
    pub unsafe fn tick(&mut self) -> Decision {
        unsafe {
            let foreground = GetForegroundWindow();
            let mut pid = 0;
            let foreground_thread = GetWindowThreadProcessId(foreground, &mut pid);
            if self.last_menu.is_some() && pid != 0 && pid != self.pid && pid != std::process::id()
            {
                return Decision::Hide;
            }
            let mut menu = in_menu(self.thread);
            if pid == self.pid && foreground_thread != self.thread {
                menu |= in_menu(foreground_thread);
            }
            if !menu {
                let mut state = Scan {
                    panel: self.panel,
                    baseline: &self.baseline,
                    visible: vec![],
                    found: false,
                };
                if self.thread != 0 {
                    EnumThreadWindows(
                        self.thread,
                        Some(scan),
                        (&mut state as *mut Scan<'_>) as isize,
                    );
                }
                if pid == self.pid && foreground_thread != self.thread {
                    EnumThreadWindows(
                        foreground_thread,
                        Some(scan),
                        (&mut state as *mut Scan<'_>) as isize,
                    );
                }
                menu = state.found;
            }
            let now = Instant::now();
            if menu {
                self.last_menu = Some(now);
                return Decision::Wait;
            }
            let settling = self.last_menu.map_or(
                now.duration_since(self.started) < Duration::from_secs(2),
                |at| now.duration_since(at) < Duration::from_millis(200),
            );
            if settling {
                return Decision::Wait;
            }
            let panel = self.panel as HWND;
            if foreground == panel {
                return Decision::Resume;
            }
            let mut point = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut point) != 0 {
                let under = WindowFromPoint(point);
                if (under == panel || IsChild(panel, under) != 0)
                    && (pid == self.pid || pid == std::process::id())
                {
                    return Decision::Resume;
                }
            }
            Decision::Hide
        }
    }
}
