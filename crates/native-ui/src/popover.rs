//! Toggle/dismiss behavior shared by plugin popovers, without global mouse hooks.
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
#[derive(Default)]
pub struct Popover {
    dismissed: Option<(Instant, (i32, i32))>,
}
impl Popover {
    /// # Safety
    /// Window belongs to the calling UI thread.
    pub unsafe fn toggle(&mut self, hwnd: HWND) -> bool {
        unsafe {
            if IsWindowVisible(hwnd) != 0 {
                self.dismissed = None;
                ShowWindow(hwnd, SW_HIDE);
                return false;
            }
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            let suppress = self
                .dismissed
                .take()
                .is_some_and(|(at, old)| same_click(at.elapsed(), old, (point.x, point.y)));
            !suppress
        }
    }
    /// # Safety
    /// Window belongs to the calling UI thread. Disabled modal owners remain open.
    pub unsafe fn outside(&mut self, hwnd: HWND) {
        unsafe {
            if IsWindowVisible(hwnd) == 0 || IsWindowEnabled(hwnd) == 0 {
                return;
            }
            self.dismissed = None;
            let owner = GetWindow(hwnd, GW_OWNER);
            let mut point = POINT { x: 0, y: 0 };
            let mut bounds: RECT = std::mem::zeroed();
            if !owner.is_null()
                && GetCursorPos(&mut point) != 0
                && GetWindowRect(owner, &mut bounds) != 0
                && point.x >= bounds.left
                && point.x < bounds.right
                && point.y >= bounds.top
                && point.y < bounds.bottom
            {
                self.dismissed = Some((Instant::now(), (point.x, point.y)));
            }
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}
fn same_click(age: Duration, old: (i32, i32), now: (i32, i32)) -> bool {
    age < Duration::from_millis(300) && (old.0 - now.0).abs() <= 4 && (old.1 - now.1).abs() <= 4
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_outside_dismiss_does_not_reopen_on_the_same_taskbar_click() {
        assert!(same_click(Duration::from_millis(20), (10, 10), (11, 10)));
        assert!(!same_click(Duration::from_millis(20), (10, 10), (40, 10)));
        assert!(!same_click(Duration::from_secs(1), (10, 10), (10, 10)));
    }
    #[test]
    fn toggle_and_outside_hide_only_the_popover_and_preserve_modal_owners() {
        unsafe {
            let window = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                crate::wide("STATIC").as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                -32000,
                -32000,
                160,
                80,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!window.is_null());
            let mut popover = Popover::default();
            assert!(popover.toggle(window));
            ShowWindow(window, SW_SHOWNOACTIVATE);
            assert_ne!(IsWindowVisible(window), 0);
            assert!(!popover.toggle(window));
            assert_eq!(IsWindowVisible(window), 0);
            assert!(popover.toggle(window));
            ShowWindow(window, SW_SHOWNOACTIVATE);
            popover.outside(window);
            assert_eq!(IsWindowVisible(window), 0);
            ShowWindow(window, SW_SHOWNOACTIVATE);
            EnableWindow(window, 0);
            popover.outside(window);
            assert_ne!(IsWindowVisible(window), 0);
            EnableWindow(window, 1);
            DestroyWindow(window);
        }
    }
}
