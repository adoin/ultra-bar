//! Correct legacy Qt tray previews still anchored to Explorer's hidden notification area.
//! Uses the existing WinEvent hook and only a newly shown tool window from the hovered icon's thread.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::*,
};
struct Target {
    owner: usize,
    pid: u32,
    id: u32,
    thread: u32,
    point: (i32, i32),
    entered: Instant,
    popup: Option<usize>,
    closing_since: Option<Instant>,
}
static TARGET: Mutex<Option<Target>> = Mutex::new(None);
const TRANSFER_GRACE: Duration = Duration::from_millis(300);
pub(super) fn pending_close() -> bool {
    TARGET
        .lock()
        .ok()
        .is_some_and(|t| t.as_ref().is_some_and(|t| t.closing_since.is_some()))
}
pub(super) fn tick() {
    if !pending_close() {
        return;
    }
    let mut cursor = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut cursor) } != 0 {
        tick_at(cursor, Instant::now());
    }
}
fn tick_at(cursor: POINT, now: Instant) {
    let Ok(mut guard) = TARGET.lock() else {
        return;
    };
    let Some(target) = guard.as_mut() else {
        return;
    };
    let Some(since) = target.closing_since else {
        return;
    };
    if let Some(hwnd) = target.popup {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd as HWND, &mut pid);
            let mut rect: RECT = std::mem::zeroed();
            if pid == target.pid
                && IsWindowVisible(hwnd as HWND) != 0
                && GetWindowRect(hwnd as HWND, &mut rect) != 0
                && cursor.x >= rect.left
                && cursor.x < rect.right
                && cursor.y >= rect.top
                && cursor.y < rect.bottom
            {
                target.closing_since = Some(now);
                return;
            }
        }
    }
    if now.saturating_duration_since(since) < TRANSFER_GRACE {
        return;
    }
    let thread = target.thread;
    *guard = None;
    drop(guard);
    unsafe {
        EnumThreadWindows(thread, Some(close_preview), 0);
    }
}
pub(super) fn visible(owner: usize, pid: u32, id: u32) -> bool {
    let Ok(target) = TARGET.lock() else {
        return false;
    };
    target.as_ref().is_some_and(|t| {
        t.owner == owner
            && t.pid == pid
            && t.id == id
            && t.popup.is_some_and(|hwnd| unsafe {
                let mut actual_pid = 0;
                GetWindowThreadProcessId(hwnd as HWND, &mut actual_pid);
                actual_pid == pid && IsWindowVisible(hwnd as HWND) != 0
            })
    })
}
#[cfg(test)]
pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());
pub fn source(
    owner: usize,
    pid: u32,
    id: u32,
    thread: u32,
    point: (i32, i32),
    event: super::HoverEvent,
) {
    let Ok(mut target) = TARGET.lock() else {
        return;
    };
    if event == super::HoverEvent::Close {
        if target
            .as_ref()
            .is_some_and(|t| t.owner == owner && t.pid == pid && t.id == id)
        {
            target.as_mut().unwrap().closing_since = Some(Instant::now());
        }
        return;
    }
    let entered = !target
        .as_ref()
        .is_some_and(|t| t.owner == owner && t.pid == pid && t.id == id);
    if entered {
        let previous = target.as_ref().and_then(|t| t.popup);
        eprintln!("[tray-preview] enter pid={pid} id={id} point={point:?}");
        *target = Some(Target {
            owner,
            pid,
            id,
            thread,
            point,
            entered: Instant::now(),
            popup: None,
            closing_since: None,
        });
        drop(target);
        if let Some(previous) = previous {
            unsafe {
                close_preview(previous as HWND, 0);
            }
        }
    } else {
        target.as_mut().unwrap().closing_since = None;
        drop(target);
    }
    // On restart the external app may keep its preview visible, without a new SHOW event.
    // Inspect this icon's UI thread once on entry, rather than polling or moving the mouse.
    if entered || event == super::HoverEvent::Open {
        unsafe {
            EnumThreadWindows(thread, Some(recover_visible), 0);
        }
    }
}
unsafe extern "system" fn close_preview(hwnd: HWND, _: isize) -> i32 {
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), 128);
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        if candidate(
            &String::from_utf16_lossy(&class[..n.max(0) as usize]),
            GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            rect.right - rect.left,
            rect.bottom - rect.top,
        ) {
            let sent = PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0;
            eprintln!("[tray-preview] close hwnd={:#x} sent={sent}", hwnd as usize);
        }
    }
    1
}
unsafe extern "system" fn recover_visible(hwnd: HWND, _: isize) -> i32 {
    event(hwnd, 0x8002);
    1
}
fn eligible_event(popup: Option<usize>, window: usize, age: Duration) -> bool {
    match popup {
        Some(current) => current == window,
        None => age <= Duration::from_secs(2),
    }
}
fn candidate(class: &str, style: u32, extended: u32, width: i32, height: i32) -> bool {
    // IGG uses a popup; Weixin uses a layered, topmost tool window with
    // WS_SYSMENU and no WS_POPUP (observed style 0x160a0000). Excluding all
    // system-menu windows silently excluded its message preview from cleanup.
    let legacy_popup = style & WS_POPUP != 0 && style & WS_SYSMENU == 0;
    let weixin_tool = style & (WS_POPUP | WS_CHILD) == 0
        && style & WS_SYSMENU != 0
        && extended & (WS_EX_LAYERED | WS_EX_TOPMOST) == (WS_EX_LAYERED | WS_EX_TOPMOST);
    class.ends_with("QWindowToolSaveBits")
        && (legacy_popup || weixin_tool)
        && style & (WS_CAPTION | WS_THICKFRAME | WS_CHILD) == 0
        && extended & WS_EX_TOOLWINDOW != 0
        && (40..=640).contains(&width)
        && (20..=480).contains(&height)
}
fn near(rect: &RECT, point: (i32, i32), margin: i32) -> bool {
    point.0 >= rect.left - margin
        && point.0 <= rect.right + margin
        && (rect.bottom - point.1).abs() <= margin
}
fn placement(width: i32, height: i32, point: (i32, i32), work: &RECT, gap: i32) -> (i32, i32) {
    let max_x = (work.right - width - gap).max(work.left);
    let max_y = (work.bottom - height - gap).max(work.top);
    (
        (point.0 - width / 2).clamp(work.left, max_x),
        (point.1 - height - gap).clamp(work.top, max_y),
    )
}
pub fn event(hwnd: HWND, event: u32) {
    if hwnd.is_null() || !matches!(event, 0x8002 | 0x8003 | 0x800b) {
        return;
    }
    let Ok(mut guard) = TARGET.lock() else {
        return;
    };
    let Some(target) = guard.as_mut() else {
        return;
    };
    unsafe {
        if matches!(event, 0x8002 | 0x8003) {
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == target.pid {
                let mut class = [0u16; 128];
                let n = GetClassNameW(hwnd, class.as_mut_ptr(), 128);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut rect);
                eprintln!(
                    "[tray-preview] event={event:x} pid={pid} thread={thread}/{} hwnd={:#x} class={} visible={} style={:x} extended={:x} rect={},{},{},{}",
                    target.thread,
                    hwnd as usize,
                    String::from_utf16_lossy(&class[..n.max(0) as usize]),
                    IsWindowVisible(hwnd) != 0,
                    GetWindowLongPtrW(hwnd, GWL_STYLE),
                    GetWindowLongPtrW(hwnd, GWL_EXSTYLE),
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom
                );
            }
        }
        if event == 0x8003 {
            if target.popup == Some(hwnd as usize) {
                let mut point = POINT { x: 0, y: 0 };
                GetCursorPos(&mut point);
                eprintln!(
                    "[tray-preview] hidden pid={} popup={:#x} cursor={},{} under={:#x}",
                    target.pid,
                    hwnd as usize,
                    point.x,
                    point.y,
                    WindowFromPoint(point) as usize
                );
            }
            return;
        }
        let mut pid = 0;
        let thread = GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != target.pid || thread != target.thread || IsWindowVisible(hwnd) == 0 {
            return;
        }
        if !eligible_event(target.popup, hwnd as usize, target.entered.elapsed()) {
            return;
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as _);
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if !candidate(
            &class,
            GetWindowLongPtrW(hwnd, GWL_STYLE) as u32,
            GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            w,
            h,
        ) {
            return;
        }
        if target.popup.is_none() || event == 0x8002 {
            eprintln!(
                "[tray-preview] shown pid={} popup={:#x} rect={},{},{},{}",
                target.pid, hwnd as usize, rect.left, rect.top, rect.right, rect.bottom
            );
        }
        target.popup = Some(hwnd as usize);
        // Weixin's non-popup tool window positions itself using its own Qt
        // geometry and transparent margins. Moving its native HWND triggers a
        // client leave/hide and disagrees with that geometry. Only legacy popup
        // windows need our placement correction; both types need close cleanup.
        if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_POPUP == 0 {
            return;
        }
        let dpi = windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32;
        if near(&rect, target.point, 64 * dpi / 96) && rect.bottom <= target.point.1 - 12 * dpi / 96
        {
            return;
        }
        let monitor = MonitorFromPoint(
            POINT {
                x: target.point.0,
                y: target.point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as _;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let (x, y) = placement(w, h, target.point, &info.rcWork, 20 * dpi / 96);
        if x != rect.left || y != rect.top {
            drop(guard);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS,
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leaving_posts_close_to_the_preview_instead_of_hiding_qt_behind_its_back() {
        unsafe {
            let _test = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            use windows_sys::Win32::System::{LibraryLoader::GetModuleHandleW, Threading::*};
            let class = crate::visual::wide("UltraBar.CloseTest.QWindowToolSaveBits");
            let module = GetModuleHandleW(std::ptr::null());
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            });
            for style in [WS_POPUP | WS_VISIBLE, 0x160a0000] {
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TOPMOST,
                    class.as_ptr(),
                    std::ptr::null(),
                    style,
                    -32000,
                    -32000,
                    252,
                    140,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    module,
                    std::ptr::null(),
                );
                assert!(!hwnd.is_null());
                // Win32 adds a caption to an overlapped window at creation;
                // Qt removes it afterwards for Weixin's frameless tool window.
                SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
                for event in [
                    super::super::HoverEvent::Move,
                    super::super::HoverEvent::Close,
                ] {
                    source(
                        hwnd as usize,
                        GetCurrentProcessId(),
                        5,
                        GetCurrentThreadId(),
                        (-31874, -31850),
                        event,
                    );
                    if event == super::super::HoverEvent::Move {
                        assert!(visible(hwnd as usize, GetCurrentProcessId(), 5));
                        assert!(!visible(hwnd as usize, GetCurrentProcessId(), 6));
                        if style & WS_POPUP == 0 {
                            let mut rect: RECT = std::mem::zeroed();
                            GetWindowRect(hwnd, &mut rect);
                            assert_eq!((rect.left, rect.top), (-32000, -32000));
                        }
                    } else {
                        let now = Instant::now();
                        let outside = POINT {
                            x: -1000000,
                            y: -1000000,
                        };
                        tick_at(outside, now + Duration::from_millis(100));
                        assert!(visible(hwnd as usize, GetCurrentProcessId(), 5));
                        let mut rect: RECT = std::mem::zeroed();
                        GetWindowRect(hwnd, &mut rect);
                        tick_at(
                            POINT {
                                x: (rect.left + rect.right) / 2,
                                y: (rect.top + rect.bottom) / 2,
                            },
                            now + Duration::from_millis(500),
                        );
                        assert!(visible(hwnd as usize, GetCurrentProcessId(), 5));
                        tick_at(outside, now + Duration::from_millis(600));
                        assert!(visible(hwnd as usize, GetCurrentProcessId(), 5));
                        tick_at(outside, now + Duration::from_millis(900));
                        assert!(!visible(hwnd as usize, GetCurrentProcessId(), 5));
                    }
                }
                let mut msg: MSG = std::mem::zeroed();
                assert_ne!(
                    PeekMessageW(&mut msg, hwnd, WM_CLOSE, WM_CLOSE, PM_REMOVE),
                    0
                );
                DispatchMessageW(&msg);
                assert_eq!(IsWindow(hwnd), 0);
                assert!(TARGET.lock().unwrap().is_none());
            }
            UnregisterClassW(class.as_ptr(), module);
        }
    }
    #[test]
    fn entry_recovers_an_existing_visible_preview_without_a_new_show_event() {
        unsafe {
            let _test = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            use windows_sys::Win32::System::{
                LibraryLoader::GetModuleHandleW,
                Threading::{GetCurrentProcessId, GetCurrentThreadId},
            };
            let module = GetModuleHandleW(std::ptr::null());
            let class = crate::visual::wide("UltraBar.Test.Qt5QWindowToolSaveBits");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | WS_VISIBLE,
                -32000,
                -32000,
                252,
                140,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            assert!(!hwnd.is_null());
            for _ in 0..2 {
                source(
                    hwnd as usize,
                    GetCurrentProcessId(),
                    7,
                    GetCurrentThreadId(),
                    (-31874, -31850),
                    super::super::HoverEvent::Move,
                );
                assert_eq!(
                    TARGET.lock().unwrap().as_ref().unwrap().popup,
                    Some(hwnd as usize)
                );
                source(
                    hwnd as usize,
                    GetCurrentProcessId(),
                    7,
                    GetCurrentThreadId(),
                    (-31874, -31850),
                    super::super::HoverEvent::Close,
                );
            }
            tick_at(
                POINT {
                    x: -1000000,
                    y: -1000000,
                },
                Instant::now() + Duration::from_secs(1),
            );
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), module);
        }
    }
    #[test]
    fn a_known_preview_can_be_shown_again_without_restarting_the_hover_session() {
        assert!(eligible_event(Some(42), 42, Duration::from_secs(20)));
        assert!(!eligible_event(Some(42), 99, Duration::from_millis(10)));
        assert!(eligible_event(None, 42, Duration::from_millis(10)));
        assert!(!eligible_event(None, 42, Duration::from_secs(20)));
    }
    #[test]
    fn only_captionless_qt_previews_are_candidates() {
        assert!(candidate(
            "Qt51514QWindowToolSaveBits",
            0x160a0000,
            0x80088,
            290,
            152
        ));
        assert!(!candidate(
            "Qt51514QWindowToolSaveBits",
            0x160a0000 | WS_CHILD,
            0x80088,
            290,
            152
        ));
        assert!(!candidate(
            "Qt51514QWindowToolSaveBits",
            0x160a0000 | WS_CAPTION,
            0x80088,
            290,
            152
        ));
        assert!(candidate(
            "Qt5QWindowToolSaveBits",
            WS_POPUP,
            WS_EX_TOOLWINDOW,
            252,
            140
        ));
        assert!(!candidate(
            "Qt5QWindowToolSaveBits",
            WS_POPUP | WS_SYSMENU,
            WS_EX_TOOLWINDOW,
            481,
            293
        ));
        assert!(!candidate(
            "Qt5QWindowIcon",
            WS_POPUP,
            WS_EX_TOOLWINDOW,
            252,
            140
        ));
        assert!(!candidate(
            "Qt5QWindowPopupDropShadowSaveBits",
            WS_POPUP,
            WS_EX_TOOLWINDOW,
            252,
            140
        ));
    }
    #[test]
    fn placement_uses_the_hovered_monitor_and_preserves_nearby_native_placement() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1049,
        };
        let (x, y) = placement(252, 140, (-24, 1064), &work, 6);
        assert_eq!(x, -258);
        assert_eq!(y, 903);
        let rect = RECT {
            left: x,
            top: y,
            right: x + 252,
            bottom: y + 140,
        };
        assert!(near(&rect, (-24, 1064), 64));
        assert!(!near(
            &RECT {
                left: 0,
                top: 909,
                right: 252,
                bottom: 1049
            },
            (1780, 1064),
            64
        ));
    }
}
