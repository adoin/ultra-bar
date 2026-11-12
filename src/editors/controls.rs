//! Native editing primitives owned by the host; no calendar business logic.
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Timelike};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use ultra_bar_native_ui::{text::draw_text as DrawTextW, wide};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
const PREPARE_POPUP: u32 = WM_APP + 0x52;
const STEP_TIME: u32 = WM_APP + 0x53;
const DATE_EDIT: &str = "UltraBar.HostEditor.DateTime.v1";
pub(super) fn background() -> HBRUSH {
    ultra_bar_native_ui::theme::background_brush()
}

pub(super) fn field_brush() -> HBRUSH {
    ultra_bar_native_ui::theme::field_brush()
}

pub(super) unsafe fn date_input(hwnd: HWND) -> HWND {
    unsafe { GetPropW(hwnd, wide(DATE_EDIT).as_ptr()) as HWND }
}

pub(super) unsafe fn input_time(hwnd: HWND, st: &mut SYSTEMTIME) -> bool {
    unsafe {
        let edit = date_input(hwnd);
        if edit.is_null() {
            return true;
        }
        let mut value = [0u16; 64];
        let n = GetWindowTextW(edit, value.as_mut_ptr(), 64);
        let value = String::from_utf16_lossy(&value[..n.max(0) as usize]);
        if GetDlgCtrlID(hwnd) == 103 {
            if let Ok(time) = NaiveTime::parse_from_str(value.trim(), "%H:%M") {
                st.wHour = time.hour() as _;
                st.wMinute = time.minute() as _;
                return true;
            }
        } else if let Ok(date) = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d") {
            st.wYear = date.year() as _;
            st.wMonth = date.month() as _;
            st.wDay = date.day() as _;
            return true;
        }
        false
    }
}

pub(super) unsafe fn sync_date_input(hwnd: HWND, st: &SYSTEMTIME) {
    unsafe {
        let edit = date_input(hwnd);
        if edit.is_null() {
            return;
        }
        let text = if GetDlgCtrlID(hwnd) == 103 {
            format!("{:02}:{:02}", st.wHour, st.wMinute)
        } else {
            format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
        };
        SetWindowTextW(edit, wide(&text).as_ptr());
    }
}

pub(super) unsafe fn layout_date_input(hwnd: HWND, dpi: i32) {
    unsafe {
        let edit = date_input(hwnd);
        if edit.is_null() {
            return;
        }
        let mut r: RECT = zeroed();
        GetClientRect(hwnd, &mut r);
        let u = |n| n * dpi / 96;
        SetWindowPos(
            edit,
            HWND_TOP,
            u(4),
            u(2),
            (r.right - u(30)).max(1),
            (r.bottom - u(4)).max(1),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

pub(super) unsafe fn step_time(hwnd: HWND, delta: i64) {
    unsafe {
        let mut st: SYSTEMTIME = zeroed();
        SendMessageW(
            hwnd,
            DTM_GETSYSTEMTIME,
            0,
            (&mut st as *mut SYSTEMTIME) as isize,
        );
        if let Some(time) = NaiveTime::from_hms_opt(st.wHour as _, st.wMinute as _, 0) {
            let next = time.overflowing_add_signed(Duration::minutes(delta)).0;
            st.wHour = next.hour() as _;
            st.wMinute = next.minute() as _;
            SendMessageW(
                hwnd,
                DTM_SETSYSTEMTIME,
                GDT_VALID as _,
                (&st as *const SYSTEMTIME) as isize,
            );
        }
    }
}

unsafe extern "system" fn date_edit_proc(
    hwnd: HWND,
    message: u32,
    w: usize,
    l: isize,
    id: usize,
    parent: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(date_edit_proc), id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == WM_KEYDOWN
            && GetDlgCtrlID(parent as HWND) == 103
            && matches!(w as u16, VK_UP | VK_DOWN)
        {
            SendMessageW(
                parent as HWND,
                STEP_TIME,
                if w == VK_UP as usize { 1 } else { usize::MAX },
                0,
            );
            return 0;
        }
        if message == WM_KILLFOCUS {
            let mut st: SYSTEMTIME = zeroed();
            if SendMessageW(
                parent as HWND,
                DTM_GETSYSTEMTIME,
                0,
                (&mut st as *mut SYSTEMTIME) as isize,
            ) == GDT_VALID as isize
            {
                SendMessageW(
                    parent as HWND,
                    DTM_SETSYSTEMTIME,
                    GDT_VALID as _,
                    (&st as *const SYSTEMTIME) as isize,
                );
            }
        }
        DefSubclassProc(hwnd, message, w, l)
    }
}

unsafe extern "system" fn stepper_proc(
    hwnd: HWND,
    message: u32,
    w: usize,
    l: isize,
    id: usize,
    parent: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(stepper_proc), id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == WM_ERASEBKGND {
            return 1;
        }
        if matches!(message, WM_PAINT | WM_PRINTCLIENT | WM_PRINT) {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = if message == WM_PAINT {
                BeginPaint(hwnd, &mut ps)
            } else {
                w as HDC
            };
            let mut r: RECT = zeroed();
            GetClientRect(hwnd, &mut r);
            if let Some(surface) = ultra_bar_native_ui::Surface::new(r.right, r.bottom) {
                FillRect(surface.dc, &r, field_brush());
                let old = SelectObject(
                    surface.dc,
                    SendMessageW(parent as HWND, WM_GETFONT, 0, 0) as HGDIOBJ,
                );
                SetBkMode(surface.dc, TRANSPARENT as _);
                SetTextColor(surface.dc, ultra_bar_native_ui::theme::palette().text);
                let mut top = r;
                top.bottom = r.bottom / 2;
                let mut bottom = r;
                bottom.top = top.bottom;
                for (text, rect) in [("▴", &mut top), ("▾", &mut bottom)] {
                    DrawTextW(
                        surface.dc,
                        wide(text).as_ptr(),
                        -1,
                        rect,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                    );
                }
                SelectObject(surface.dc, old);
                surface.blit_opaque(dc);
            }
            if message == WM_PAINT {
                EndPaint(hwnd, &ps);
            }
            return 0;
        }
        DefSubclassProc(hwnd, message, w, l)
    }
}

pub(super) unsafe fn attach_date_input(hwnd: HWND, font: HFONT, dpi: i32) {
    unsafe {
        let mut st: SYSTEMTIME = zeroed();
        SendMessageW(
            hwnd,
            DTM_GETSYSTEMTIME,
            0,
            (&mut st as *mut SYSTEMTIME) as isize,
        );
        let edit = CreateWindowExW(
            0,
            wide("EDIT").as_ptr(),
            null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            0,
            0,
            1,
            1,
            hwnd,
            310 as _,
            GetModuleHandleW(null()),
            null(),
        );
        SetPropW(hwnd, wide(DATE_EDIT).as_ptr(), edit as _);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            GetWindowLongPtrW(hwnd, GWL_STYLE) | WS_CLIPCHILDREN as isize,
        );
        SendMessageW(edit, WM_SETFONT, font as _, 0);
        SendMessageW(
            edit,
            EM_SETLIMITTEXT,
            if GetDlgCtrlID(hwnd) == 103 { 5 } else { 10 },
            0,
        );
        let margin = (4 * dpi / 96) as u32;
        SendMessageW(
            edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as _,
            (margin | margin << 16) as _,
        );
        SetWindowSubclass(edit, Some(date_edit_proc), 2, hwnd as usize);
        ultra_bar_native_ui::input::attach_composite(edit);
        let mut info: DATETIMEPICKERINFO = zeroed();
        info.cbSize = size_of::<DATETIMEPICKERINFO>() as u32;
        SendMessageW(
            hwnd,
            DTM_GETDATETIMEPICKERINFO,
            0,
            (&mut info as *mut DATETIMEPICKERINFO) as isize,
        );
        if !info.hwndUD.is_null() {
            SetWindowTheme(info.hwndUD, wide("").as_ptr(), wide("").as_ptr());
            SetWindowSubclass(info.hwndUD, Some(stepper_proc), 3, hwnd as usize);
        }
        theme_time_children(hwnd);
        sync_date_input(hwnd, &st);
        layout_date_input(hwnd, dpi);
    }
}

pub(super) unsafe fn theme_time_children(parent: HWND) {
    unsafe extern "system" fn child(hwnd: HWND, data: isize) -> i32 {
        unsafe {
            let mut name = [0u16; 64];
            let n = GetClassNameW(hwnd, name.as_mut_ptr(), 64);
            if String::from_utf16_lossy(&name[..n.max(0) as usize])
                .eq_ignore_ascii_case("msctls_updown32")
            {
                if !GetPropW(hwnd, wide("UltraBar.TimeStepperStyled.v1").as_ptr()).is_null() {
                    return 1;
                }
                SetWindowTheme(hwnd, wide("").as_ptr(), wide("").as_ptr());
                SetWindowSubclass(hwnd, Some(stepper_proc), 3, data as usize);
                SetPropW(
                    hwnd,
                    wide("UltraBar.TimeStepperStyled.v1").as_ptr(),
                    1usize as _,
                );
                InvalidateRect(hwnd, null(), 0);
            }
            1
        }
    }
    unsafe {
        EnumChildWindows(parent, Some(child), parent as isize);
    }
}

pub(super) unsafe fn opaque_popup(hwnd: HWND) {
    unsafe {
        if hwnd.is_null() {
            return;
        }
        #[repr(C)]
        struct Accent {
            state: i32,
            flags: i32,
            tint: u32,
            animation: i32,
        }
        #[repr(C)]
        struct Attribute {
            attribute: i32,
            data: *mut std::ffi::c_void,
            size: usize,
        }
        let module = GetModuleHandleW(wide("user32.dll").as_ptr());
        if let Some(address) =
            GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast())
        {
            let set: unsafe extern "system" fn(HWND, *mut Attribute) -> i32 =
                std::mem::transmute(address);
            let mut accent = Accent {
                state: 0,
                flags: 0,
                tint: 0,
                animation: 0,
            };
            let mut data = Attribute {
                attribute: 19,
                data: (&mut accent as *mut Accent).cast(),
                size: size_of::<Accent>(),
            };
            set(hwnd, &mut data);
        }
        use windows_sys::Win32::Graphics::Dwm::*;
        let blur = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE,
            fEnable: 0,
            hRgnBlur: null_mut(),
            fTransitionOnMaximized: 0,
        };
        DwmEnableBlurBehindWindow(hwnd, &blur);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let next = ex & !((WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_CLIENTEDGE) as isize);
        if ex != next {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
        }
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        InvalidateRect(hwnd, null(), 1);
    }
}

pub(super) unsafe fn prepare_combo_popup(combo: HWND) {
    unsafe {
        let mut info: COMBOBOXINFO = zeroed();
        info.cbSize = size_of::<COMBOBOXINFO>() as _;
        if GetComboBoxInfo(combo, &mut info) != 0 && !info.hwndList.is_null() {
            SetWindowTheme(info.hwndList, wide("DarkMode_Explorer").as_ptr(), null());
            SetWindowSubclass(info.hwndList, Some(popup_proc), 9, combo as usize);
            PostMessageW(info.hwndList, PREPARE_POPUP, 0, 0);
        }
    }
}

pub(super) unsafe fn draw_combo_item(item: &DRAWITEMSTRUCT) {
    unsafe {
        let mut r = item.rcItem;
        let brush = if item.itemState & ODS_SELECTED != 0 {
            CreateSolidBrush(ultra_bar_native_ui::theme::palette().selection)
        } else {
            field_brush()
        };
        FillRect(item.hDC, &r, brush);
        if item.itemState & ODS_SELECTED != 0 {
            DeleteObject(brush);
        }
        if item.itemID == u32::MAX {
            return;
        }
        let n = SendMessageW(item.hwndItem, CB_GETLBTEXTLEN, item.itemID as usize, 0);
        if !(0..=4096).contains(&n) {
            return;
        }
        let mut text = vec![0u16; n as usize + 1];
        SendMessageW(
            item.hwndItem,
            CB_GETLBTEXT,
            item.itemID as usize,
            text.as_mut_ptr() as isize,
        );
        let old = SelectObject(
            item.hDC,
            SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ,
        );
        SetBkMode(item.hDC, TRANSPARENT as _);
        SetTextColor(item.hDC, ultra_bar_native_ui::theme::palette().text);
        r.left +=
            8 * windows_sys::Win32::UI::HiDpi::GetDpiForWindow(item.hwndItem).max(96) as i32 / 96;
        DrawTextW(
            item.hDC,
            text.as_ptr(),
            -1,
            &mut r,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(item.hDC, old);
    }
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(popup_proc), id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == PREPARE_POPUP {
            opaque_popup(hwnd);
            return 0;
        }
        if message == WM_WINDOWPOSCHANGING && l != 0 {
            let position = &mut *(l as *mut WINDOWPOS);
            if position.flags & SWP_HIDEWINDOW == 0 {
                position.hwndInsertAfter = HWND_TOPMOST;
                position.flags &= !SWP_NOZORDER;
                position.flags |= SWP_NOACTIVATE;
            }
        }
        if message == WM_SHOWWINDOW && w != 0 {
            let result = DefSubclassProc(hwnd, message, w, l);
            PostMessageW(hwnd, PREPARE_POPUP, 0, 0);
            return result;
        }
        if message == WM_STYLECHANGED
            && w as i32 == GWL_EXSTYLE
            && l != 0
            && (*(l as *const STYLESTRUCT)).styleNew & (WS_EX_LAYERED | WS_EX_TRANSPARENT) != 0
        {
            PostMessageW(hwnd, PREPARE_POPUP, 0, 0);
        }
        if data != 0 && message == WM_ERASEBKGND {
            let mut area = zeroed();
            GetClientRect(hwnd, &mut area);
            FillRect(w as _, &area, field_brush());
            return 1;
        }
        // Native list tracking now paints each row through WM_DRAWITEM. Do not
        // repaint a second selection layer for every mouse move.
        DefSubclassProc(hwnd, message, w, l)
    }
}

pub(super) unsafe fn control_color(message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let dc = w as HDC;
        SetBkMode(dc, TRANSPARENT as _);
        let static_label = message == WM_CTLCOLORSTATIC;
        SetTextColor(
            dc,
            if static_label && GetDlgCtrlID(l as HWND) != 250 {
                ultra_bar_native_ui::theme::palette().muted
            } else {
                ultra_bar_native_ui::theme::palette().text
            },
        );
        SetBkColor(
            dc,
            if static_label {
                ultra_bar_native_ui::theme::palette().background
            } else {
                ultra_bar_native_ui::theme::palette().field
            },
        );
        if static_label {
            background() as _
        } else {
            field_brush() as _
        }
    }
}

pub(super) unsafe fn create(
    parent: HWND,
    class: &str,
    label: &str,
    id: i32,
    style: u32,
    bounds: (i32, i32, i32, i32),
    typography: (HFONT, i32),
) -> HWND {
    unsafe {
        let (x, y, width, height) = bounds;
        let (font, dpi) = typography;
        let unit = |n: i32| n * dpi / 96;
        let child = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD
                | WS_VISIBLE
                | style
                | if class == "COMBOBOX" {
                    CBS_OWNERDRAWFIXED as u32 | CBS_HASSTRINGS as u32
                } else {
                    0
                },
            unit(x),
            unit(y),
            unit(width),
            unit(height),
            parent,
            id as _,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(child, WM_SETFONT, font as _, 1);
        if class == "EDIT" || class == "SysDateTimePick32" {
            SetWindowLongPtrW(
                child,
                GWL_STYLE,
                GetWindowLongPtrW(child, GWL_STYLE) & !(WS_BORDER as isize),
            );
            SetWindowLongPtrW(
                child,
                GWL_EXSTYLE,
                GetWindowLongPtrW(child, GWL_EXSTYLE) & !(WS_EX_CLIENTEDGE as isize),
            );
            SetWindowPos(
                child,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if class == "STATIC" {
            ultra_bar_native_ui::input::attach_label(child);
        }
        if class == "EDIT" {
            ultra_bar_native_ui::input::attach(child);
            let margin = unit(8) as u32;
            SendMessageW(
                child,
                EM_SETMARGINS,
                (EC_LEFTMARGIN | EC_RIGHTMARGIN) as _,
                (margin | (margin << 16)) as _,
            );
        }
        if class == "EDIT" {
            let empty = wide("");
            SetWindowTheme(child, empty.as_ptr(), empty.as_ptr());
        } else {
            SetWindowTheme(child, wide("DarkMode_CFD").as_ptr(), null());
        }
        if class == "COMBOBOX" {
            SetWindowSubclass(child, Some(combo_proc), 1, dpi as _);
            SendMessageW(child, CB_SETITEMHEIGHT, 0, unit(24) as _);
        }
        if class == "SysDateTimePick32" {
            SetWindowSubclass(child, Some(date_proc), 1, dpi as _);
            attach_date_input(child, font, dpi);
        }
        if class == "BUTTON" && (style & 0xf) == BS_AUTOCHECKBOX as u32 {
            SetWindowSubclass(child, Some(check_proc), 1, dpi as _);
        }
        if class == "EDIT" || class == "COMBOBOX" || class == "SysDateTimePick32" {
            let mut bounds: RECT = zeroed();
            GetWindowRect(child, &mut bounds);
            let region = CreateRoundRectRgn(
                0,
                0,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                unit(10),
                unit(10),
            );
            SetWindowRgn(child, region, 1);
        }
        child
    }
}

pub(super) unsafe fn paint_control(hwnd: HWND, dc: HDC, dpi: i32, kind: u8) {
    unsafe {
        let mut rect: RECT = zeroed();
        GetClientRect(hwnd, &mut rect);
        let unit = |n: i32| n * dpi / 96;
        FillRect(dc, &rect, background());
        let old_font = SelectObject(dc, SendMessageW(hwnd, WM_GETFONT, 0, 0) as HGDIOBJ);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, ultra_bar_native_ui::theme::palette().text);
        let mut caption = [0u16; 512];
        let length = GetWindowTextW(hwnd, caption.as_mut_ptr(), caption.len() as _);
        let mut text_rect = rect;
        if kind == 0 && GetPropW(hwnd, wide("UltraBar.HostEditor.Checkbox").as_ptr()).is_null() {
            let top = (rect.bottom - unit(18)) / 2;
            ultra_bar_native_ui::form::draw_switch(
                dc,
                RECT {
                    left: 0,
                    top,
                    right: unit(34),
                    bottom: top + unit(18),
                },
                SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize,
                IsWindowEnabled(hwnd) != 0,
            );
            text_rect.left = unit(44);
            DrawTextW(
                dc,
                caption.as_ptr(),
                length,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            SelectObject(dc, old_font);
            return;
        }
        if kind == 0 {
            let size = unit(16);
            let top = (rect.bottom - size) / 2;
            let box_rect = RECT {
                left: unit(1),
                top,
                right: unit(1) + size,
                bottom: top + size,
            };
            let brush = CreateSolidBrush(
                if SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
                    ultra_bar_native_ui::theme::palette().active_fill
                } else {
                    ultra_bar_native_ui::theme::palette().field
                },
            );
            FillRect(dc, &box_rect, brush);
            DeleteObject(brush);
            if SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
                let pen = CreatePen(
                    PS_SOLID,
                    unit(2).max(1),
                    ultra_bar_native_ui::theme::palette().text,
                );
                let old = SelectObject(dc, pen);
                MoveToEx(dc, unit(4), top + unit(8), null_mut());
                LineTo(dc, unit(7), top + unit(11));
                LineTo(dc, unit(13), top + unit(4));
                SelectObject(dc, old);
                DeleteObject(pen);
            }
            text_rect.left = unit(24);
            DrawTextW(
                dc,
                caption.as_ptr(),
                length,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        } else {
            let old_brush = SelectObject(dc, field_brush());
            let focused = GetFocus() == hwnd || IsChild(hwnd, GetFocus()) != 0;
            let pen = CreatePen(
                PS_SOLID,
                1,
                if focused {
                    ultra_bar_native_ui::theme::palette().active_border
                } else {
                    ultra_bar_native_ui::theme::palette().border
                },
            );
            let old_pen = SelectObject(dc, pen);
            RoundRect(dc, 0, 0, rect.right, rect.bottom, unit(10), unit(10));
            SelectObject(dc, old_pen);
            DeleteObject(pen);
            SelectObject(dc, old_brush);
            let value = if kind == 2 {
                let mut time: SYSTEMTIME = zeroed();
                SendMessageW(
                    hwnd,
                    DTM_GETSYSTEMTIME,
                    0,
                    (&mut time as *mut SYSTEMTIME) as _,
                );
                if GetDlgCtrlID(hwnd) == 103 {
                    format!("{:02}:{:02}", time.wHour, time.wMinute)
                } else {
                    format!("{:04}-{:02}-{:02}", time.wYear, time.wMonth, time.wDay)
                }
            } else {
                String::from_utf16_lossy(&caption[..length.max(0) as usize])
            };
            text_rect.left = unit(8);
            text_rect.right -= unit(26);
            DrawTextW(
                dc,
                wide(&value).as_ptr(),
                -1,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            if GetDlgCtrlID(hwnd) != 103 {
                let points = [
                    POINT {
                        x: rect.right - unit(18),
                        y: rect.bottom / 2 - unit(2),
                    },
                    POINT {
                        x: rect.right - unit(10),
                        y: rect.bottom / 2 - unit(2),
                    },
                    POINT {
                        x: rect.right - unit(14),
                        y: rect.bottom / 2 + unit(3),
                    },
                ];
                let brush = CreateSolidBrush(ultra_bar_native_ui::theme::palette().muted);
                let old_brush = SelectObject(dc, brush);
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                Polygon(dc, points.as_ptr(), 3);
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                DeleteObject(brush);
            }
        }
        if GetFocus() == hwnd {
            let mut focus = rect;
            InflateRect(&mut focus, -unit(2), -unit(2));
            DrawFocusRect(dc, &focus);
        }
        SelectObject(dc, old_font);
    }
}

pub(super) unsafe fn themed_control(
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    dpi: usize,
    kind: u8,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            let callback: SUBCLASSPROC = match kind {
                0 => Some(check_proc),
                1 => Some(combo_proc),
                _ => Some(date_proc),
            };
            RemoveWindowSubclass(hwnd, callback, id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == WM_ERASEBKGND {
            return 1;
        }
        if matches!(message, WM_PAINT | WM_PRINT | WM_PRINTCLIENT) {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = if message == WM_PAINT {
                BeginPaint(hwnd, &mut ps)
            } else {
                w as HDC
            };
            let mut rect: RECT = zeroed();
            GetClientRect(hwnd, &mut rect);
            if let Some(surface) = ultra_bar_native_ui::Surface::new(rect.right, rect.bottom) {
                paint_control(hwnd, surface.dc, dpi as _, kind);
                surface.blit_opaque(dc);
            }
            if message == WM_PAINT {
                EndPaint(hwnd, &ps);
            }
            return 0;
        }
        if message == WM_PRINT || message == WM_PRINTCLIENT {
            paint_control(hwnd, w as _, dpi as _, kind);
            return 0;
        }
        let result = DefSubclassProc(hwnd, message, w, l);
        if matches!(
            message,
            BM_SETCHECK | CB_SETCURSEL | WM_SETFOCUS | WM_KILLFOCUS | WM_LBUTTONUP | WM_KEYUP
        ) {
            InvalidateRect(hwnd, null(), 0);
        }
        result
    }
}

unsafe extern "system" fn check_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe { themed_control(hwnd, m, w, l, id, data, 0) }
}

unsafe extern "system" fn combo_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe { themed_control(hwnd, m, w, l, id, data, 1) }
}

unsafe extern "system" fn date_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        if m == WM_COMMAND && l as HWND == date_input(hwnd) {
            return 0;
        }
        if m == DTM_GETSYSTEMTIME && l != 0 {
            let result = DefSubclassProc(hwnd, m, w, l);
            let editable = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE != 0
                && IsWindowEnabled(hwnd) != 0;
            if result == GDT_VALID as isize
                && editable
                && !input_time(hwnd, &mut *(l as *mut SYSTEMTIME))
            {
                return GDT_ERROR as isize;
            }
            return result;
        }
        if m == DTM_SETSYSTEMTIME {
            let result = DefSubclassProc(hwnd, m, w, l);
            if w == GDT_VALID as usize && l != 0 {
                sync_date_input(hwnd, &*(l as *const SYSTEMTIME));
            }
            theme_time_children(hwnd);
            InvalidateRect(hwnd, null(), 0);
            return result;
        }
        if matches!(m, WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN) {
            return control_color(m, w, l);
        }
        if m == STEP_TIME {
            step_time(hwnd, if w == 1 { 1 } else { -1 });
            return 0;
        }
        if m == WM_NOTIFY && l != 0 && (*(l as *const NMHDR)).code == UDN_DELTAPOS {
            let change = &*(l as *const NMUPDOWN);
            SetFocus(date_input(hwnd));
            step_time(hwnd, -change.iDelta as i64);
            return 1;
        }
        if m == WM_SETFOCUS
            && !date_input(hwnd).is_null()
            && GetPropW(hwnd, wide("UltraBar.DateDropdownPress").as_ptr()).is_null()
        {
            SetFocus(date_input(hwnd));
            return 0;
        }
        if m == WM_LBUTTONDOWN && GetDlgCtrlID(hwnd) != 103 {
            SetPropW(
                hwnd,
                wide("UltraBar.DateDropdownPress").as_ptr(),
                1usize as _,
            );
            let result = themed_control(hwnd, m, w, l, id, data, 2);
            RemovePropW(hwnd, wide("UltraBar.DateDropdownPress").as_ptr());
            layout_date_input(hwnd, data as i32);
            return result;
        }
        if m == WM_NCDESTROY {
            RemovePropW(hwnd, wide(DATE_EDIT).as_ptr());
        }
        let result = themed_control(hwnd, m, w, l, id, data, 2);
        if m == WM_SIZE {
            layout_date_input(hwnd, data as i32);
            theme_time_children(hwnd);
        }
        if m == WM_PARENTNOTIFY && w as u16 == WM_CREATE as u16 {
            theme_time_children(hwnd);
        }
        result
    }
}
