//! Monitor-scoped fullscreen detection, including borderless games.
use std::mem::zeroed;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
pub fn covers(window: RECT, monitor: RECT, captioned: bool, minimized: bool) -> bool {
    !captioned
        && !minimized
        && window.left <= monitor.left + 2
        && window.top <= monitor.top + 2
        && window.right >= monitor.right - 2
        && window.bottom >= monitor.bottom - 2
}
pub unsafe fn hide_owned(owner: HWND) -> Vec<(usize, u32)> {
    unsafe extern "system" fn collect(hwnd: HWND, param: isize) -> i32 {
        unsafe {
            let (owner, windows) = &mut *(param as *mut (HWND, Vec<(usize, u32)>));
            if hwnd != *owner
                && GetAncestor(hwnd, GA_ROOTOWNER) == *owner
                && IsWindowVisible(hwnd) != 0
            {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, &mut pid);
                windows.push((hwnd as usize, pid));
            }
            1
        }
    }
    unsafe {
        let mut data = (owner, vec![]);
        EnumWindows(
            Some(collect),
            (&mut data as *mut (HWND, Vec<(usize, u32)>)) as isize,
        );
        let mut restore = vec![];
        for (window, pid) in data.1 {
            let hwnd = window as HWND;
            let mut name = [0u16; 128];
            let n = GetClassNameW(hwnd, name.as_mut_ptr(), 128);
            let class = String::from_utf16_lossy(&name[..n.max(0) as usize]);
            if (IsWindowEnabled(hwnd) == 0 && class.starts_with("UltraBar."))
                || matches!(
                    class.as_str(),
                    "UltraBar.Host.Editor.v1"
                        | "UltraBar.Settings"
                        | "UltraBar.Input"
                        | "UltraBar.ColorPicker"
                        | "#32770"
                )
            {
                restore.push((window, pid));
            }
            ShowWindow(hwnd, SW_HIDE);
        }
        restore
    }
}
pub unsafe fn restore_owned(owner: HWND, windows: Vec<(usize, u32)>) {
    unsafe {
        for (window, pid) in windows.into_iter().rev() {
            let hwnd = window as HWND;
            let mut current = 0;
            GetWindowThreadProcessId(hwnd, &mut current);
            if current == pid && IsWindow(hwnd) != 0 && GetAncestor(hwnd, GA_ROOTOWNER) == owner {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
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
    fn fullscreen_temporarily_hides_editors_without_destroying_their_drafts() {
        unsafe {
            use std::ptr::{null, null_mut};
            let owner = CreateWindowExW(
                WS_EX_NOACTIVATE,
                crate::visual::wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                20,
                20,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            let class = crate::visual::wide("UltraBar.FullscreenEditorTest");
            let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let editor = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class.as_ptr(),
                crate::visual::wide("draft").as_ptr(),
                WS_POPUP | WS_VISIBLE,
                -32000,
                -32000,
                200,
                120,
                owner,
                null_mut(),
                module,
                null(),
            );
            EnableWindow(editor, 0); // Modal owners must also be restored.
            let saved = hide_owned(owner);
            assert_eq!(saved.len(), 1);
            assert_eq!(IsWindowVisible(editor), 0);
            assert_ne!(IsWindow(editor), 0);
            restore_owned(owner, saved);
            assert_ne!(IsWindowVisible(editor), 0);
            let mut text = [0u16; 16];
            let n = GetWindowTextW(editor, text.as_mut_ptr(), 16);
            assert_eq!(String::from_utf16_lossy(&text[..n as usize]), "draft");
            DestroyWindow(editor);
            DestroyWindow(owner);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
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
