use crate::visual::text::draw_text as DrawTextW;
use crate::visual::wide;
use std::{
    cell::Cell,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};
struct Input {
    edit: HWND,
    value: Option<String>,
    done: Cell<bool>,
    icons: HFONT,
    tips: Vec<Vec<u16>>,
}
fn background() -> HBRUSH {
    crate::visual::theme::background_brush()
}
fn field() -> HBRUSH {
    crate::visual::theme::field_brush()
}
pub unsafe fn edit(owner: HWND, title: &str, hint: &str, initial: &str) -> Option<String> {
    unsafe {
        let module = GetModuleHandleW(null());
        let class = wide("UltraBar.Input");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let dpi = GetDpiForWindow(owner).max(96) as i32;
        let unit = |n| n * dpi / 96;
        let mut rect: RECT = zeroed();
        GetWindowRect(owner, &mut rect);
        let mut state = Box::new(Input {
            edit: null_mut(),
            value: None,
            done: Cell::new(false),
            icons: CreateFontW(
                -19 * dpi / 96,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_DEFAULT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                CLEARTYPE_NATURAL_QUALITY as _,
                DEFAULT_PITCH as _,
                wide("Segoe MDL2 Assets").as_ptr(),
            ),
            tips: vec![wide("确定"), wide("取消")],
        });
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromWindow(owner, MONITOR_DEFAULTTOPRIMARY),
            &mut info,
        );
        let x = ((rect.left + rect.right - unit(480)) / 2).clamp(
            info.rcWork.left + 8,
            (info.rcWork.right - unit(480) - 8).max(info.rcWork.left + 8),
        );
        let y = ((rect.top + rect.bottom - unit(200)) / 2).clamp(
            info.rcWork.top + 8,
            (info.rcWork.bottom - unit(200) - 8).max(info.rcWork.top + 8),
        );
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_SYSMENU,
            x,
            y,
            unit(480),
            unit(200),
            owner,
            null_mut(),
            module,
            (&mut *state as *mut Input).cast(),
        );
        if window.is_null() {
            DeleteObject(state.icons);
            return None;
        }
        crate::visual::enable_glass(window);
        SetLayeredWindowAttributes(window, 0, 232, LWA_ALPHA);
        SetWindowRgn(
            window,
            CreateRoundRectRgn(0, 0, unit(480), unit(200), unit(20), unit(20)),
            1,
        );
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let tips = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            window,
            null_mut(),
            module,
            null(),
        );
        for (class, text, style, x, y, width, height, id) in [
            ("STATIC", title, 0, 16, 12, 440, 24, 11),
            ("STATIC", hint, 0, 16, 44, 440, 32, 0),
            (
                "EDIT",
                initial,
                ES_AUTOHSCROLL as u32 | WS_TABSTOP,
                16,
                84,
                440,
                28,
                10,
            ),
            (
                "BUTTON",
                "确定",
                WS_TABSTOP | BS_OWNERDRAW as u32,
                360,
                144,
                40,
                28,
                1,
            ),
            (
                "BUTTON",
                "取消",
                WS_TABSTOP | BS_OWNERDRAW as u32,
                416,
                144,
                40,
                28,
                2,
            ),
        ] {
            let child = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                unit(x),
                unit(y),
                unit(width),
                unit(height),
                window,
                id as _,
                module,
                null(),
            );
            SendMessageW(child, WM_SETFONT, font as _, 1);
            if id == 10 {
                state.edit = child;
                SendMessageW(child, EM_SETLIMITTEXT, 256, 0);
                let margin = unit(8) as u32;
                SendMessageW(
                    child,
                    EM_SETMARGINS,
                    (EC_LEFTMARGIN | EC_RIGHTMARGIN) as _,
                    (margin | (margin << 16)) as _,
                );
                SetWindowRgn(
                    child,
                    CreateRoundRectRgn(0, 0, unit(width), unit(height), unit(10), unit(10)),
                    1,
                );
            }
            if id == 1 || id == 2 {
                let info = TTTOOLINFOW {
                    cbSize: size_of::<TTTOOLINFOW>() as _,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd: window,
                    uId: child as usize,
                    lpszText: state.tips[id as usize - 1].as_mut_ptr(),
                    ..zeroed()
                };
                SendMessageW(tips, TTM_ADDTOOLW, 0, (&info as *const TTTOOLINFOW) as _);
            }
        }
        EnableWindow(owner, 0);
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
        SetFocus(state.edit);
        let mut msg: MSG = zeroed();
        loop {
            if state.done.get() {
                break;
            }
            let received = GetMessageW(&mut msg, null_mut(), 0, 0);
            if received <= 0 {
                if received == 0 {
                    PostQuitMessage(msg.wParam as i32);
                }
                break;
            }
            if IsDialogMessageW(window, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if IsWindow(window) != 0 {
            DestroyWindow(window);
        }
        EnableWindow(owner, 1);
        SetForegroundWindow(owner);
        DeleteObject(state.icons);
        state.value.take()
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if crate::visual::theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_ERASEBKGND {
            let mut rect: RECT = zeroed();
            GetClientRect(hwnd, &mut rect);
            FillRect(w as HDC, &rect, background());
            return 1;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
            );
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Input;
        if !state.is_null() {
            if message == WM_CTLCOLORSTATIC || message == WM_CTLCOLOREDIT {
                SetTextColor(
                    w as _,
                    if message == WM_CTLCOLORSTATIC && GetDlgCtrlID(l as HWND) != 11 {
                        crate::visual::theme::palette().muted
                    } else {
                        crate::visual::theme::palette().text
                    },
                );
                SetBkMode(w as _, TRANSPARENT as _);
                SetBkColor(
                    w as _,
                    if message == WM_CTLCOLORSTATIC {
                        crate::visual::theme::palette().background
                    } else {
                        crate::visual::theme::palette().field
                    },
                );
                return if message == WM_CTLCOLORSTATIC {
                    background() as _
                } else {
                    field() as _
                };
            }
            if message == WM_DRAWITEM {
                let item = &*(l as *const DRAWITEMSTRUCT);
                let dc = item.hDC;
                let old = SelectObject(dc, (*state).icons);
                let brush = CreateSolidBrush(if item.CtlID == 1 {
                    crate::visual::theme::palette().selection
                } else {
                    crate::visual::theme::palette().hover
                });
                let old_brush = SelectObject(dc, brush);
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                RoundRect(
                    dc,
                    item.rcItem.left,
                    item.rcItem.top,
                    item.rcItem.right,
                    item.rcItem.bottom,
                    10,
                    10,
                );
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                DeleteObject(brush);
                SetTextColor(dc, crate::visual::theme::palette().text);
                SetBkMode(dc, TRANSPARENT as _);
                let mut rect = item.rcItem;
                DrawTextW(
                    dc,
                    wide(if item.CtlID == 1 {
                        "\u{e73e}"
                    } else {
                        "\u{e711}"
                    })
                    .as_ptr(),
                    -1,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                SelectObject(dc, old);
                return 1;
            }
            if message == DM_GETDEFID {
                return ((DC_HASDEFID as usize) << 16 | 1) as _;
            }
            if message == WM_COMMAND && w & 0xffff == 1 {
                let mut value = [0u16; 257];
                let len = GetWindowTextW((*state).edit, value.as_mut_ptr(), value.len() as _);
                (*state).value = Some(String::from_utf16_lossy(&value[..len.max(0) as usize]));
                DestroyWindow(hwnd);
                return 0;
            }
            if message == WM_CLOSE || (message == WM_COMMAND && w & 0xffff == 2) {
                DestroyWindow(hwnd);
                return 0;
            }
            if message == WM_DESTROY {
                (*state).done.set(true);
                return 0;
            }
        }
        DefWindowProcW(hwnd, message, w, l)
    }
}
