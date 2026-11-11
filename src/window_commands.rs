//! Respect the target window's own system menu and command handling.
use windows_sys::Win32::{
    Foundation::*,
    UI::{Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};
pub fn close_available(hwnd: HWND, pid: u32) -> Option<bool> {
    unsafe {
        let mut current = 0;
        if IsWindow(hwnd) == 0
            || GetWindowThreadProcessId(hwnd, &mut current) == 0
            || current != pid
        {
            return None;
        }
        let menu = GetSystemMenu(hwnd, 0);
        if menu.is_null() {
            return None;
        }
        let state = GetMenuState(menu, SC_CLOSE, MF_BYCOMMAND);
        if state == u32::MAX {
            return None;
        }
        Some(state & (MF_DISABLED | MF_GRAYED) == 0 && IsWindowEnabled(hwnd) != 0)
    }
}
pub fn request_close(hwnd: HWND, pid: u32) -> bool {
    unsafe {
        close_available(hwnd, pid) == Some(true)
            && PostMessageW(hwnd, WM_SYSCOMMAND, SC_CLOSE as usize, 0) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        mem::zeroed,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    #[test]
    fn close_availability_respects_missing_disabled_and_removed_system_commands() {
        unsafe {
            let module = GetModuleHandleW(null());
            let class = crate::visual::wide("UltraBar.CloseCapabilityTest");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let create = |style| {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    null(),
                    style,
                    0,
                    0,
                    40,
                    40,
                    null_mut(),
                    null_mut(),
                    module,
                    null(),
                )
            };
            let no_menu = create(WS_POPUP);
            assert!(!no_menu.is_null());
            let pid = std::process::id();
            assert_eq!(close_available(no_menu, pid), None);
            assert!(!request_close(no_menu, pid));
            DestroyWindow(no_menu);
            let window = create(WS_POPUP | WS_SYSMENU);
            assert_eq!(close_available(window, pid), Some(true));
            let menu = GetSystemMenu(window, 0);
            EnableMenuItem(menu, SC_CLOSE, MF_BYCOMMAND | MF_GRAYED);
            assert_eq!(close_available(window, pid), Some(false));
            assert!(!request_close(window, pid));
            EnableMenuItem(menu, SC_CLOSE, MF_BYCOMMAND | MF_ENABLED);
            RemoveMenu(menu, SC_CLOSE, MF_BYCOMMAND);
            assert_eq!(close_available(window, pid), None);
            assert!(!request_close(window, pid));
            DestroyWindow(window);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
}
