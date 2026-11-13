//! Shared, non-editing confirmation surface. Cancel is the default action.
use crate::{Surface, text::draw_text, theme, wide};
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
struct State {
    title: String,
    body: String,
    accept: String,
    dpi: i32,
    font: HFONT,
    heading: HFONT,
    choice: Option<bool>,
    accept_focused: bool,
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            crate::fonts::delete_object(self.font);
            crate::fonts::delete_object(self.heading);
        }
    }
}
fn button(accept: bool, dpi: i32) -> RECT {
    let u = |v| v * dpi / 96;
    RECT {
        left: u(if accept { 240 } else { 132 }),
        top: u(136),
        right: u(if accept { 336 } else { 228 }),
        bottom: u(174),
    }
}
unsafe fn paint(s: &State, dc: HDC) {
    unsafe {
        let u = |v| v * s.dpi / 96;
        let p = theme::palette();
        let bounds = RECT {
            left: 0,
            top: 0,
            right: u(360),
            bottom: u(198),
        };
        FillRect(dc, &bounds, theme::background_brush());
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, p.text);
        let old = SelectObject(dc, s.heading);
        let mut title = RECT {
            left: u(24),
            top: u(18),
            right: u(336),
            bottom: u(48),
        };
        draw_text(
            dc,
            wide(&s.title).as_ptr(),
            -1,
            &mut title,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(dc, s.font);
        let mut body = RECT {
            left: u(24),
            top: u(60),
            right: u(336),
            bottom: u(120),
        };
        draw_text(
            dc,
            wide(&s.body).as_ptr(),
            -1,
            &mut body,
            DT_WORDBREAK | DT_NOPREFIX,
        );
        for accept in [false, true] {
            let mut r = button(accept, s.dpi);
            let brush = CreateSolidBrush(if accept { p.active_fill } else { p.field });
            let pen = CreatePen(
                PS_SOLID,
                u(1).max(1),
                if s.accept_focused == accept {
                    p.active_border
                } else {
                    p.border
                },
            );
            let b = SelectObject(dc, brush);
            let o = SelectObject(dc, pen);
            RoundRect(dc, r.left, r.top, r.right, r.bottom, u(10), u(10));
            SelectObject(dc, b);
            SelectObject(dc, o);
            crate::fonts::delete_object(brush);
            crate::fonts::delete_object(pen);
            draw_text(
                dc,
                wide(if accept { &s.accept } else { "取消" }).as_ptr(),
                -1,
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
        SelectObject(dc, old);
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
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, m, w, l);
        }
        let s = &mut *pointer;
        let mut finish = None;
        match m {
            WM_CLOSE => finish = Some(false),
            WM_KEYDOWN => match w as u16 {
                VK_ESCAPE => finish = Some(false),
                VK_RETURN | VK_SPACE => finish = Some(s.accept_focused),
                VK_TAB => {
                    s.accept_focused = !s.accept_focused;
                    InvalidateRect(hwnd, null(), 0);
                }
                VK_LEFT => {
                    s.accept_focused = false;
                    InvalidateRect(hwnd, null(), 0);
                }
                VK_RIGHT => {
                    s.accept_focused = true;
                    InvalidateRect(hwnd, null(), 0);
                }
                _ => {}
            },
            WM_LBUTTONUP => {
                let point = POINT {
                    x: l as u16 as i16 as i32,
                    y: (l >> 16) as u16 as i16 as i32,
                };
                for accept in [false, true] {
                    if PtInRect(&button(accept, s.dpi), point) != 0 {
                        finish = Some(accept);
                    }
                }
            }
            WM_ERASEBKGND => return 1,
            WM_PAINT => {
                let mut ps = zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                if let Some(surface) = Surface::new(360 * s.dpi / 96, 198 * s.dpi / 96) {
                    paint(s, surface.dc);
                    surface.blit_opaque(dc);
                }
                EndPaint(hwnd, &ps);
                return 0;
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                return DefWindowProcW(hwnd, m, w, l);
            }
            _ => return DefWindowProcW(hwnd, m, w, l),
        }
        if let Some(choice) = finish {
            s.choice = Some(choice);
            DestroyWindow(hwnd);
        }
        0
    }
}
unsafe fn make_state(dpi: i32, title: &str, body: &str, accept: &str) -> State {
    unsafe {
        let font = |size: i32, weight: i32| {
            crate::fonts::create_font(
                -size * dpi / 96,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                0,
                0,
                CLEARTYPE_NATURAL_QUALITY as _,
                0,
                wide("Segoe UI").as_ptr(),
            )
        };
        State {
            title: title.into(),
            body: body.into(),
            accept: accept.into(),
            dpi,
            font: font(14, 400),
            heading: font(18, 600),
            choice: None,
            accept_focused: false,
        }
    }
}
/// # Safety
/// Invoke on the owner's UI thread. No global keyboard/mouse hooks are used.
pub unsafe fn confirm(owner: HWND, title: &str, body: &str, accept: &str) -> bool {
    unsafe {
        let dpi = GetDpiForWindow(owner).max(96) as i32;
        let mut state = Box::new(make_state(dpi, title, body, accept));
        let module = GetModuleHandleW(null());
        let class = wide("UltraBar.Confirm.v1");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let mut monitor: MONITORINFO = zeroed();
        monitor.cbSize = std::mem::size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        );
        let mut area = monitor.rcWork;
        let mut parent: RECT = zeroed();
        if GetWindowRect(owner, &mut parent) != 0 && parent.bottom - parent.top > 100 {
            area = parent;
        }
        let width = 360 * dpi / 96;
        let height = 198 * dpi / 96;
        let x = ((area.left + area.right - width) / 2).clamp(
            monitor.rcWork.left,
            (monitor.rcWork.right - width).max(monitor.rcWork.left),
        );
        let y = ((area.top + area.bottom - height) / 2).clamp(
            monitor.rcWork.top,
            (monitor.rcWork.bottom - height).max(monitor.rcWork.top),
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            owner,
            null_mut(),
            module,
            (&mut *state as *mut State).cast(),
        );
        if hwnd.is_null() {
            return false;
        }
        crate::enable_glass(hwnd);
        SetLayeredWindowAttributes(hwnd, 0, 244, LWA_ALPHA);
        crate::geometry::attach_rounded(hwnd, 20);
        let mut name = [0u16; 64];
        let n = GetClassNameW(owner, name.as_mut_ptr(), 64);
        let disabled = IsWindowEnabled(owner) != 0
            && String::from_utf16_lossy(&name[..n.max(0) as usize]) != "UltraBar.Window";
        if disabled {
            EnableWindow(owner, 0);
        }
        #[cfg(test)]
        let automated = TEST_ANSWER.with(|answer| answer.get());
        #[cfg(not(test))]
        let automated: Option<bool> = None;
        if let Some(answer) = automated {
            SetWindowPos(
                hwnd,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            if answer {
                PostMessageW(hwnd, WM_KEYDOWN, VK_RIGHT as usize, 0);
            }
            PostMessageW(hwnd, WM_KEYDOWN, VK_RETURN as usize, 0);
        } else {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
        }
        let mut message: MSG = zeroed();
        while IsWindow(hwnd) != 0 && state.choice.is_none() {
            let status = GetMessageW(&mut message, null_mut(), 0, 0);
            if status <= 0 {
                if status == 0 {
                    PostQuitMessage(message.wParam as i32);
                }
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
        if disabled && IsWindow(owner) != 0 {
            EnableWindow(owner, 1);
            if automated.is_none() {
                SetForegroundWindow(owner);
            }
        }
        state.choice.unwrap_or(false)
    }
}
/// Capture the shared confirmation renderer without opening a dialog.
/// # Safety
/// Call on a UI thread with an absolute task output directory.
pub unsafe fn capture(directory: &std::path::Path) -> std::io::Result<()> {
    unsafe {
        if !directory.is_absolute() {
            return Err(std::io::Error::other("Absolute output directory required"));
        }
        std::fs::create_dir_all(directory)?;
        for dpi in [96, 144, 192] {
            let state = make_state(dpi, "删除日程", "删除这条日程？", "删除");
            let surface = Surface::new(360 * dpi / 96, 198 * dpi / 96)
                .ok_or_else(|| std::io::Error::other("Cannot allocate confirmation surface"))?;
            paint(&state, surface.dc);
            surface.save_bmp(&directory.join(format!("confirm-{dpi}.bmp")))?;
        }
        Ok(())
    }
}
#[cfg(test)]
thread_local! { static TEST_ANSWER:std::cell::Cell<Option<bool>>=const{std::cell::Cell::new(None)}; }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_cancel_and_explicit_accept_restore_owner_state() {
        unsafe {
            let owner = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                400,
                300,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!owner.is_null());
            for answer in [false, true] {
                TEST_ANSWER.with(|value| value.set(Some(answer)));
                assert_eq!(confirm(owner, "删除", "删除这条记录？", "删除"), answer);
                assert_ne!(IsWindowEnabled(owner), 0);
            }
            EnableWindow(owner, 0);
            assert!(confirm(owner, "删除", "删除这条记录？", "删除"));
            assert_eq!(IsWindowEnabled(owner), 0);
            TEST_ANSWER.with(|value| value.set(None));
            DestroyWindow(owner);
        }
    }
}
