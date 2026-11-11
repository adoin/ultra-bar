use crate::visual::text::draw_text as DrawTextW;
use crate::visual::{
    self, Surface,
    theme::{self, Hsl},
    wide,
};
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

struct Picker {
    hwnd: HWND,
    edit: HWND,
    dpi: i32,
    font: HFONT,
    icons: HFONT,
    selection: Hsl,
    original: u32,
    drag: Option<u8>,
    updating: bool,
    invalid: bool,
    done: Cell<bool>,
    result: Option<u32>,
    export: Option<std::path::PathBuf>,
}
impl Drop for Picker {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
            DeleteObject(self.font);
            DeleteObject(self.icons);
        }
    }
}
impl Picker {
    fn unit(&self, n: i32) -> i32 {
        n * self.dpi / 96
    }
    fn rect(&self, x: i32, y: i32, w: i32, h: i32) -> RECT {
        RECT {
            left: self.unit(x),
            top: self.unit(y),
            right: self.unit(x + w),
            bottom: self.unit(y + h),
        }
    }
}
fn contains(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}
unsafe fn create(owner: HWND, initial: u32) -> Option<Box<Picker>> {
    unsafe {
        let module = GetModuleHandleW(null());
        let class = wide("UltraBar.ColorPicker");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let dpi = GetDpiForWindow(owner).max(96) as i32;
        let font = |name: &str, size: i32| {
            CreateFontW(
                -size * dpi / 96,
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
                wide(name).as_ptr(),
            )
        };
        let mut state = Box::new(Picker {
            hwnd: null_mut(),
            edit: null_mut(),
            dpi,
            font: font("Segoe UI", 13),
            icons: font("Segoe MDL2 Assets", 18),
            selection: Hsl::from_rgb(initial),
            original: initial,
            drag: None,
            updating: false,
            invalid: false,
            done: Cell::new(false),
            result: None,
            export: None,
        });
        let mut monitor: MONITORINFO = zeroed();
        monitor.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(
            MonitorFromWindow(owner, MONITOR_DEFAULTTOPRIMARY),
            &mut monitor,
        );
        let mut bounds: RECT = zeroed();
        GetWindowRect(owner, &mut bounds);
        let x = ((bounds.left + bounds.right - state.unit(408)) / 2).clamp(
            monitor.rcWork.left + 8,
            (monitor.rcWork.right - state.unit(408) - 8).max(monitor.rcWork.left + 8),
        );
        let y = (bounds.top - state.unit(398)).clamp(
            monitor.rcWork.top + 8,
            (monitor.rcWork.bottom - state.unit(390) - 8).max(monitor.rcWork.top + 8),
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class.as_ptr(),
            wide("主色").as_ptr(),
            WS_POPUP | WS_SYSMENU | WS_CLIPCHILDREN,
            x,
            y,
            state.unit(408),
            state.unit(390),
            owner,
            null_mut(),
            module,
            (&mut *state as *mut Picker).cast(),
        );
        if hwnd.is_null() {
            return None;
        }
        state.hwnd = hwnd;
        visual::enable_glass(hwnd);
        SetLayeredWindowAttributes(hwnd, 0, 232, LWA_ALPHA);
        SetWindowRgn(
            hwnd,
            CreateRoundRectRgn(
                0,
                0,
                state.unit(408),
                state.unit(390),
                state.unit(22),
                state.unit(22),
            ),
            1,
        );
        state.updating = true;
        state.edit = CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            wide(&theme::hex(initial)).as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            state.unit(24),
            state.unit(294),
            state.unit(132),
            state.unit(28),
            hwnd,
            10usize as _,
            module,
            null(),
        );
        SendMessageW(state.edit, WM_SETFONT, state.font as usize, 1);
        SendMessageW(state.edit, EM_SETLIMITTEXT, 7, 0);
        let margin = state.unit(8) as u32;
        SendMessageW(
            state.edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            (margin | (margin << 16)) as isize,
        );
        SetWindowRgn(
            state.edit,
            CreateRoundRectRgn(
                0,
                0,
                state.unit(132),
                state.unit(28),
                state.unit(8),
                state.unit(8),
            ),
            1,
        );
        state.updating = false;
        Some(state)
    }
}
pub unsafe fn choose(owner: HWND, initial: u32) -> Option<u32> {
    unsafe {
        let mut state = create(owner, initial)?;
        EnableWindow(owner, 0);
        ShowWindow(state.hwnd, SW_SHOW);
        SetForegroundWindow(state.hwnd);
        SetFocus(state.edit);
        let mut msg: MSG = zeroed();
        while !state.done.get() {
            let received = GetMessageW(&mut msg, null_mut(), 0, 0);
            if received <= 0 {
                if received == 0 {
                    PostQuitMessage(msg.wParam as i32);
                }
                break;
            }
            if (msg.hwnd == state.hwnd || IsChild(state.hwnd, msg.hwnd) != 0)
                && msg.message == WM_KEYDOWN
            {
                if msg.wParam == VK_ESCAPE as usize {
                    DestroyWindow(state.hwnd);
                    continue;
                }
                if msg.wParam == VK_RETURN as usize {
                    accept(&mut state);
                    continue;
                }
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        EnableWindow(owner, 1);
        SetForegroundWindow(owner);
        state.result
    }
}
unsafe fn accept(state: &mut Picker) {
    unsafe {
        if !state.invalid {
            state.result = Some(state.selection.rgb());
            DestroyWindow(state.hwnd);
        } else {
            InvalidateRect(state.hwnd, null(), 0);
        }
    }
}
unsafe fn update(state: &mut Picker, x: i32, y: i32) {
    unsafe {
        let plane = state.rect(24, 56, 360, 180);
        let hue = state.rect(24, 254, 360, 16);
        if state.drag == Some(1) {
            state.selection.s =
                ((x - plane.left) as f64 / (plane.right - plane.left - 1) as f64).clamp(0., 1.);
            state.selection.l =
                (1. - (y - plane.top) as f64 / (plane.bottom - plane.top - 1) as f64).clamp(0., 1.);
        }
        if state.drag == Some(2) {
            state.selection.h = (((x - hue.left) as f64 / (hue.right - hue.left - 1) as f64)
                .clamp(0., 1.)
                * 359.99)
                .min(359.99);
        }
        state.invalid = false;
        state.updating = true;
        SetWindowTextW(
            state.edit,
            wide(&theme::hex(state.selection.rgb())).as_ptr(),
        );
        state.updating = false;
        InvalidateRect(state.hwnd, null(), 0);
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Picker;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *pointer;
        match message {
            WM_PAINT => {
                paint(state);
                0
            }
            WM_ERASEBKGND => 1,
            WM_CTLCOLOREDIT => {
                SetTextColor(w as HDC, theme::palette().text);
                SetBkColor(w as HDC, theme::palette().field);
                theme::field_brush() as isize
            }
            WM_COMMAND if w & 0xffff == 10 && (w >> 16) == EN_CHANGE as usize => {
                if !state.updating {
                    let mut text = [0u16; 16];
                    let n = GetWindowTextW(state.edit, text.as_mut_ptr(), text.len() as i32);
                    if let Some(rgb) =
                        theme::parse_hex(&String::from_utf16_lossy(&text[..n.max(0) as usize]))
                    {
                        state.selection = Hsl::from_rgb(rgb);
                        state.invalid = false;
                    } else {
                        state.invalid = true;
                    }
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_LBUTTONDOWN => {
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                state.drag = if contains(state.rect(24, 56, 360, 180), x, y) {
                    Some(1)
                } else if contains(state.rect(24, 246, 360, 32), x, y) {
                    Some(2)
                } else if contains(state.rect(304, 344, 32, 28), x, y) {
                    Some(3)
                } else if contains(state.rect(352, 344, 32, 28), x, y) {
                    Some(4)
                } else {
                    None
                };
                if state.drag.is_some() {
                    SetCapture(hwnd);
                    if matches!(state.drag, Some(1 | 2)) {
                        update(state, x, y);
                    }
                }
                0
            }
            WM_MOUSEMOVE => {
                if matches!(state.drag, Some(1 | 2)) {
                    update(
                        state,
                        l as u16 as i16 as i32,
                        (l >> 16) as u16 as i16 as i32,
                    );
                }
                0
            }
            WM_LBUTTONUP => {
                let pressed = state.drag.take();
                if pressed.is_some() {
                    ReleaseCapture();
                }
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                if pressed == Some(3) && contains(state.rect(304, 344, 32, 28), x, y) {
                    accept(state);
                } else if pressed == Some(4) && contains(state.rect(352, 344, 32, 28), x, y) {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                state.drag = None;
                if message == WM_CANCELMODE && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                0
            }
            WM_NCHITTEST => {
                let mut point = POINT {
                    x: l as u16 as i16 as i32,
                    y: (l >> 16) as u16 as i16 as i32,
                };
                ScreenToClient(hwnd, &mut point);
                if point.y < state.unit(40) {
                    HTCAPTION as isize
                } else {
                    HTCLIENT as isize
                }
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                state.done.set(true);
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
unsafe fn paint(state: &mut Picker) {
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        BeginPaint(state.hwnd, &mut ps);
        let mut area: RECT = zeroed();
        GetClientRect(state.hwnd, &mut area);
        let Some(surface) = Surface::new(area.right, area.bottom) else {
            EndPaint(state.hwnd, &ps);
            return;
        };
        let dc = surface.dc;
        let p = theme::palette();
        let brush = CreateSolidBrush(p.background);
        FillRect(dc, &area, brush);
        DeleteObject(brush);
        SetBkMode(dc, TRANSPARENT as i32);
        let old = SelectObject(dc, state.font);
        let text = |value: &str, mut rect: RECT, color: u32| {
            SetTextColor(dc, color);
            DrawTextW(
                dc,
                wide(value).as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        };
        text("主色", state.rect(24, 12, 360, 30), p.text);
        GdiFlush();
        let plane = state.rect(24, 56, 360, 180);
        for y in plane.top..plane.bottom {
            let lightness = 1. - (y - plane.top) as f64 / (plane.bottom - plane.top - 1) as f64;
            for x in plane.left..plane.right {
                surface.pixel(
                    x,
                    y,
                    Hsl {
                        h: state.selection.h,
                        s: (x - plane.left) as f64 / (plane.right - plane.left - 1) as f64,
                        l: lightness,
                    }
                    .rgb(),
                );
            }
        }
        let hue = state.rect(24, 254, 360, 16);
        for x in hue.left..hue.right {
            let rgb = Hsl {
                h: (x - hue.left) as f64 / (hue.right - hue.left - 1) as f64 * 359.99,
                s: 1.,
                l: 0.5,
            }
            .rgb();
            for y in hue.top..hue.bottom {
                surface.pixel(x, y, rgb);
            }
        }
        let marker = |x: i32, y: i32, radius: i32| {
            let pen = CreatePen(PS_SOLID, state.unit(2).max(1), p.text);
            let old_pen = SelectObject(dc, pen);
            let old_brush = SelectObject(dc, GetStockObject(HOLLOW_BRUSH));
            Ellipse(dc, x - radius, y - radius, x + radius + 1, y + radius + 1);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(pen);
        };
        marker(
            plane.left + (state.selection.s * (plane.right - plane.left - 1) as f64) as i32,
            plane.top + ((1. - state.selection.l) * (plane.bottom - plane.top - 1) as f64) as i32,
            state.unit(5),
        );
        marker(
            hue.left + (state.selection.h / 360. * (hue.right - hue.left - 1) as f64) as i32,
            (hue.top + hue.bottom) / 2,
            state.unit(7),
        );
        let fill = |rect: RECT, color: u32| {
            let brush = CreateSolidBrush(color);
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                state.unit(8),
                state.unit(8),
            );
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            DeleteObject(brush);
        };
        fill(state.rect(24, 294, 132, 28), p.field);
        fill(
            state.rect(180, 294, 48, 28),
            theme::colorref(state.original),
        );
        fill(
            state.rect(232, 294, 48, 28),
            theme::colorref(state.selection.rgb()),
        );
        text(
            &if state.invalid {
                "请输入六位十六进制颜色".into()
            } else {
                format!(
                    "H {:.0}°   S {:.0}%   L {:.0}%",
                    state.selection.h,
                    state.selection.s * 100.,
                    state.selection.l * 100.
                )
            },
            state.rect(24, 338, 272, 32),
            p.muted,
        );
        for (x, glyph, color) in [(304, "\u{e73e}", p.selection), (352, "\u{e711}", p.hover)] {
            let rect = state.rect(x, 344, 32, 28);
            fill(rect, color);
            SelectObject(dc, state.icons);
            SetTextColor(dc, p.text);
            let mut rect = rect;
            DrawTextW(
                dc,
                wide(glyph).as_ptr(),
                -1,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(dc, state.font);
        }
        if let Some(path) = state.export.take() {
            let mut value = [0u16; 16];
            let length = GetWindowTextW(state.edit, value.as_mut_ptr(), value.len() as i32);
            text(
                &String::from_utf16_lossy(&value[..length.max(0) as usize]),
                state.rect(32, 294, 116, 28),
                p.text,
            );
            let _ = surface.save_bmp(&path);
        }
        BitBlt(ps.hdc, 0, 0, area.right, area.bottom, dc, 0, 0, SRCCOPY);
        SelectObject(dc, old);
        EndPaint(state.hwnd, &ps);
    }
}
pub unsafe fn smoke(directory: &std::path::Path) -> std::io::Result<()> {
    unsafe {
        std::fs::create_dir_all(directory)?;
        let Some(mut state) = create(null_mut(), theme::DEFAULT_ACCENT) else {
            return Err(std::io::Error::other("Cannot create colour picker"));
        };
        state.export = Some(directory.join("color-picker.bmp"));
        paint(&mut state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_colour_cannot_commit_and_hex_input_is_saved_exactly() {
        unsafe {
            let picker = create(null_mut(), theme::DEFAULT_ACCENT).unwrap();
            SetWindowTextW(picker.edit, wide("#12XX").as_ptr());
            assert!(picker.invalid);
            let point = ((picker.unit(356) as u32) << 16 | picker.unit(320) as u32) as isize;
            SendMessageW(picker.hwnd, WM_LBUTTONDOWN, 0, point);
            SendMessageW(picker.hwnd, WM_MOUSEMOVE, 0, point);
            SendMessageW(picker.hwnd, WM_LBUTTONUP, 0, point);
            assert!(!picker.done.get());
            assert!(picker.result.is_none());
            SetWindowTextW(picker.edit, wide("#12ABCD").as_ptr());
            assert!(!picker.invalid);
            assert_eq!(picker.selection.rgb(), 0x12abcd);
            SendMessageW(picker.hwnd, WM_LBUTTONDOWN, 0, point);
            SendMessageW(picker.hwnd, WM_LBUTTONUP, 0, point);
            assert!(picker.done.get());
            assert_eq!(picker.result, Some(0x12abcd));
        }
    }
    #[test]
    fn cancelling_does_not_apply_a_preview_colour() {
        unsafe {
            let before = theme::accent_rgb();
            let picker = create(null_mut(), before).unwrap();
            SetWindowTextW(picker.edit, wide("#EE1144").as_ptr());
            assert_eq!(picker.selection.rgb(), 0xee1144);
            SendMessageW(picker.hwnd, WM_CLOSE, 0, 0);
            assert!(picker.done.get());
            assert!(picker.result.is_none());
            assert_eq!(theme::accent_rgb(), before);
        }
    }
}
