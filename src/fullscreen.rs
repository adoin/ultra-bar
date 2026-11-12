//! Monitor-scoped fullscreen detection, including borderless games.
use std::mem::zeroed;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};
pub fn covers(window: RECT, monitor: RECT, captioned: bool, minimized: bool) -> bool {
    !captioned
        && !minimized
        && window.left <= monitor.left + 2
        && window.top <= monitor.top + 2
        && window.right >= monitor.right - 2
        && window.bottom >= monitor.bottom - 2
}
pub unsafe fn active(owner: HWND, native: Option<usize>) -> bool {
    unsafe {
        let window = GetAncestor(GetForegroundWindow(), GA_ROOT);
        if window.is_null()
            || window == owner
            || IsWindowVisible(window) == 0
            || IsIconic(window) != 0
        {
            return false;
        }
        let mut class = [0u16; 128];
        let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == std::process::id() || GetAncestor(window, GA_ROOTOWNER) == owner {
            return false;
        }
        // Shell flyouts can own monitor-sized transparent CoreWindows.
        if class == "Windows.UI.Core.CoreWindow" || class == "XamlExplorerHostIslandWindow" {
            let path = crate::task_apps::process_path(window);
            let name = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "startmenuexperiencehost.exe"
                    | "shellexperiencehost.exe"
                    | "searchhost.exe"
                    | "searchapp.exe"
                    | "searchui.exe"
                    | "explorer.exe"
            ) {
                return false;
            }
        }
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        if MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) != monitor {
            return false;
        }
        if native == Some(window as usize) {
            let mut notification = 0;
            if SHQueryUserNotificationState(&mut notification) >= 0
                && notification == QUNS_RUNNING_D3D_FULL_SCREEN
            {
                return true;
            }
        }
        let mut info: MONITORINFO = zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return false;
        }
        let mut rect: RECT = zeroed();
        if GetWindowRect(window, &mut rect) == 0 {
            return false;
        }
        let mut bounds: RECT = zeroed();
        if DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        ) >= 0
        {
            rect = bounds;
        }
        covers(
            rect,
            info.rcMonitor,
            GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CAPTION == WS_CAPTION,
            false,
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borderless_fullscreen_differs_from_maximized_and_other_monitor_windows() {
        let monitor = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert!(covers(monitor, monitor, false, false));
        assert!(!covers(
            RECT {
                bottom: 1040,
                ..monitor
            },
            monitor,
            false,
            false
        ));
        assert!(!covers(monitor, monitor, true, false));
        assert!(!covers(monitor, monitor, false, true));
        assert!(!covers(
            RECT {
                left: 0,
                right: 1920,
                ..monitor
            },
            monitor,
            false,
            false
        ));
    }
}
