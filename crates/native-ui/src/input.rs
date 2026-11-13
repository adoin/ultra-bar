//! Standard editing shortcuts for native Win32 EDIT controls, including IME inputs.
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
/// # Safety
/// Attach on the edit control's UI thread.
pub unsafe fn attach(edit: HWND) {
    unsafe {
        SetWindowSubclass(edit, Some(proc), 0x554241, 0);
        SetWindowSubclass(GetParent(edit), Some(parent_color), 0x554243, 0);
        let empty = crate::wide("");
        SetWindowTheme(edit, empty.as_ptr(), empty.as_ptr());
        SetWindowLongPtrW(
            edit,
            GWL_STYLE,
            GetWindowLongPtrW(edit, GWL_STYLE) & !((WS_BORDER | WS_VSCROLL) as isize),
        );
        SetWindowLongPtrW(
            edit,
            GWL_EXSTYLE,
            GetWindowLongPtrW(edit, GWL_EXSTYLE) & !(WS_EX_CLIENTEDGE as isize),
        );
        let margin = (8 * GetDpiForWindow(edit).max(96) / 96) as usize;
        SendMessageW(
            edit,
            EM_SETMARGINS,
            (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
            (margin | margin << 16) as isize,
        );
        crate::geometry::attach_rounded(edit, 10);
        frame_changed(edit);
    }
}
unsafe extern "system" fn parent_color(
    hwnd: HWND,
    message: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(parent_color), id);
        }
        if matches!(message, WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC) && l != 0 {
            let child = l as HWND;
            let mut name = [0u16; 16];
            let len = GetClassNameW(child, name.as_mut_ptr(), name.len() as i32);
            if String::from_utf16_lossy(&name[..len.max(0) as usize]).eq_ignore_ascii_case("edit") {
                let dc = w as HDC;
                SetBkMode(dc, OPAQUE as i32);
                SetBkColor(dc, crate::theme::palette().field);
                SetTextColor(
                    dc,
                    if IsWindowEnabled(child) != 0 {
                        crate::theme::palette().text
                    } else {
                        crate::theme::palette().muted
                    },
                );
                return crate::theme::field_brush() as isize;
            }
        }
        DefSubclassProc(hwnd, message, w, l)
    }
}
/// # Safety
/// Use for the inner text editor of a host-owned date/time or other composite control.
pub unsafe fn attach_composite(edit: HWND) {
    unsafe {
        attach(edit);
        SetPropW(
            edit,
            crate::wide("UltraBar.Edit.Composite").as_ptr(),
            1usize as HANDLE,
        );
        RedrawWindow(
            edit,
            std::ptr::null(),
            std::ptr::null_mut(),
            RDW_INVALIDATE | RDW_FRAME,
        );
    }
}
unsafe fn frame_changed(hwnd: HWND) {
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}
fn vertical_padding(height: i32, font_height: i32, dpi: u32, multiline: bool) -> (i32, i32) {
    if multiline {
        let padding = (4 * dpi / 96) as i32;
        return (padding, padding);
    }
    let spare = (height - font_height - 2).max(0);
    (spare / 2, spare - spare / 2)
}
unsafe fn font_height(hwnd: HWND) -> i32 {
    unsafe {
        let dc = GetDC(hwnd);
        if dc.is_null() {
            return 16;
        }
        let font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HGDIOBJ;
        let old = SelectObject(
            dc,
            if font.is_null() {
                GetStockObject(DEFAULT_GUI_FONT)
            } else {
                font
            },
        );
        let mut metrics: TEXTMETRICW = std::mem::zeroed();
        GetTextMetricsW(dc, &mut metrics);
        SelectObject(dc, old);
        ReleaseDC(hwnd, dc);
        metrics.tmHeight.max(1)
    }
}

unsafe fn paint_frame(hwnd: HWND) {
    unsafe {
        let mut window: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut window);
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let mut origin = POINT { x: 0, y: 0 };
        ClientToScreen(hwnd, &mut origin);
        let width = window.right - window.left;
        let height = window.bottom - window.top;
        let dc = GetWindowDC(hwnd);
        if dc.is_null() {
            return;
        }
        let saved = SaveDC(dc);
        ExcludeClipRect(
            dc,
            origin.x - window.left,
            origin.y - window.top,
            origin.x - window.left + client.right,
            origin.y - window.top + client.bottom,
        );
        if let Some(surface) = crate::Surface::new(width, height) {
            let area = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            FillRect(surface.dc, &area, crate::theme::field_brush());
            let pen = CreatePen(
                PS_SOLID,
                1,
                if GetFocus() == hwnd {
                    crate::theme::palette().active_border
                } else {
                    crate::theme::palette().border
                },
            );
            let old_pen = SelectObject(surface.dc, pen);
            let old_brush = SelectObject(surface.dc, GetStockObject(NULL_BRUSH));
            let radius = crate::geometry::corner_pixels(hwnd);
            if GetPropW(hwnd, crate::wide("UltraBar.Edit.Composite").as_ptr()).is_null() {
                RoundRect(surface.dc, 0, 0, width, height, radius, radius);
            }
            SelectObject(surface.dc, old_brush);
            SelectObject(surface.dc, old_pen);
            crate::fonts::delete_object(pen);
            if GetWindowLongPtrW(hwnd, GWL_STYLE) & ES_MULTILINE as isize != 0 {
                let total = SendMessageW(hwnd, EM_GETLINECOUNT, 0, 0).max(1) as i32;
                let visible = (client.bottom / font_height(hwnd).max(1)).max(1);
                if total > visible {
                    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                    let track = (height - 8 * dpi / 96).max(1);
                    let thumb = (track * visible / total).max(12 * dpi / 96).min(track);
                    let first = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, 0, 0) as i32;
                    let top = 4 * dpi / 96
                        + (track - thumb) * first.min(total - visible) / (total - visible);
                    let brush = CreateSolidBrush(crate::theme::palette().muted);
                    let old = SelectObject(surface.dc, brush);
                    let pen = SelectObject(surface.dc, GetStockObject(NULL_PEN));
                    RoundRect(
                        surface.dc,
                        width - 4 * dpi / 96,
                        top,
                        width - 2 * dpi / 96,
                        top + thumb,
                        2 * dpi / 96,
                        2 * dpi / 96,
                    );
                    SelectObject(surface.dc, pen);
                    SelectObject(surface.dc, old);
                    crate::fonts::delete_object(brush);
                }
            }
            surface.blit_opaque(dc);
        }
        RestoreDC(dc, saved);
        ReleaseDC(hwnd, dc);
    }
}
unsafe fn print_control(hwnd: HWND, target: HDC) {
    unsafe {
        let mut window: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut window);
        let width = window.right - window.left;
        let height = window.bottom - window.top;
        if let Some(surface) = crate::Surface::new(width, height) {
            let rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            FillRect(surface.dc, &rect, crate::theme::field_brush());
            let pen = CreatePen(PS_SOLID, 1, crate::theme::palette().border);
            let old = SelectObject(surface.dc, pen);
            let brush = SelectObject(surface.dc, GetStockObject(NULL_BRUSH));
            let r = crate::geometry::corner_pixels(hwnd);
            if GetPropW(hwnd, crate::wide("UltraBar.Edit.Composite").as_ptr()).is_null() {
                RoundRect(surface.dc, 0, 0, width, height, r, r);
            }
            SelectObject(surface.dc, brush);
            SelectObject(surface.dc, old);
            crate::fonts::delete_object(pen);
            let mut origin = POINT { x: 0, y: 0 };
            ClientToScreen(hwnd, &mut origin);
            let saved = SaveDC(surface.dc);
            SetViewportOrgEx(
                surface.dc,
                origin.x - window.left,
                origin.y - window.top,
                std::ptr::null_mut(),
            );
            DefSubclassProc(
                hwnd,
                WM_PRINTCLIENT,
                surface.dc as usize,
                (PRF_CLIENT | PRF_ERASEBKGND) as isize,
            );
            RestoreDC(surface.dc, saved);
            let saved = SaveDC(target);
            BeginPath(target);
            RoundRect(target, 0, 0, width, height, r, r);
            EndPath(target);
            SelectClipPath(target, RGN_AND);
            surface.blit_opaque(target);
            RestoreDC(target, saved);
        }
    }
}
unsafe fn paint_client(hwnd: HWND, field: bool) {
    unsafe {
        let mut ps = std::mem::zeroed();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut area: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut area);
        if let Some(surface) = crate::Surface::new(area.right, area.bottom) {
            FillRect(
                surface.dc,
                &area,
                if field {
                    crate::theme::field_brush()
                } else {
                    crate::theme::background_brush()
                },
            );
            DefSubclassProc(
                hwnd,
                WM_PRINTCLIENT,
                surface.dc as usize,
                (PRF_CLIENT | PRF_ERASEBKGND) as isize,
            );
            surface.blit_opaque(dc);
        }
        EndPaint(hwnd, &ps);
    }
}
/// # Safety
/// Attach on the native STATIC label's UI thread.
pub unsafe fn attach_label(hwnd: HWND) {
    unsafe {
        SetWindowSubclass(hwnd, Some(label_proc), 0x554242, 0);
    }
}
unsafe extern "system" fn label_proc(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if m == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(label_proc), id);
        }
        if m == WM_ERASEBKGND {
            return 1;
        }
        if m == WM_PAINT {
            paint_client(hwnd, false);
            return 0;
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn proc(
    hwnd: HWND,
    message: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(proc), id);
            RemovePropW(hwnd, crate::wide("UltraBar.Edit.Wheel.v1").as_ptr());
            RemovePropW(hwnd, crate::wide("UltraBar.Edit.Composite").as_ptr());
        }
        if message == WM_NCCALCSIZE {
            let result = DefSubclassProc(hwnd, message, w, l);
            let rect = if w != 0 {
                &mut (*(l as *mut NCCALCSIZE_PARAMS)).rgrc[0]
            } else {
                &mut *(l as *mut RECT)
            };
            let multiline = GetWindowLongPtrW(hwnd, GWL_STYLE) & ES_MULTILINE as isize != 0;
            let (top, bottom) = vertical_padding(
                rect.bottom - rect.top,
                font_height(hwnd),
                GetDpiForWindow(hwnd).max(96),
                multiline,
            );
            rect.top += top;
            rect.bottom = (rect.bottom - bottom).max(rect.top + 1);
            rect.left += 1;
            rect.right = (rect.right
                - if multiline {
                    (5 * GetDpiForWindow(hwnd).max(96) / 96) as i32
                } else {
                    1
                })
            .max(rect.left + 1);
            return result;
        }
        if message == WM_MOUSEWHEEL
            && GetWindowLongPtrW(hwnd, GWL_STYLE) & ES_MULTILINE as isize != 0
        {
            let key = crate::wide("UltraBar.Edit.Wheel.v1");
            let delta =
                (w >> 16) as u16 as i16 as i32 + GetPropW(hwnd, key.as_ptr()) as isize as i32;
            SetPropW(hwnd, key.as_ptr(), (delta % 120) as isize as HANDLE);
            if delta / 120 != 0 {
                SendMessageW(hwnd, EM_LINESCROLL, 0, (-3 * (delta / 120)) as isize);
                paint_frame(hwnd);
            }
            return 0;
        }
        if message == WM_SETFONT {
            let result = DefSubclassProc(hwnd, message, w, l);
            frame_changed(hwnd);
            return result;
        }
        if message == WM_SIZE {
            let result = DefSubclassProc(hwnd, message, w, l);

            return result;
        }
        if message == WM_PRINT {
            print_control(hwnd, w as HDC);
            return 0;
        }
        if message == WM_NCPAINT {
            paint_frame(hwnd);
            return 0;
        }
        if message == WM_ERASEBKGND {
            return 1;
        }
        if message == WM_PAINT {
            paint_client(hwnd, true);
            paint_frame(hwnd);
            return 0;
        }
        if matches!(message, WM_SETFOCUS | WM_KILLFOCUS) {
            let result = DefSubclassProc(hwnd, message, w, l);
            paint_frame(hwnd);
            return result;
        }
        if message == WM_KEYDOWN
            && w == b'A' as usize
            && GetKeyState(VK_CONTROL as i32) < 0
            && GetKeyState(VK_MENU as i32) >= 0
        {
            SendMessageW(hwnd, EM_SETSEL, 0, -1);
            crate::diagnostics::event(
                "input",
                "select_all",
                serde_json::json!({"hwnd":hwnd as usize,"control_id":GetDlgCtrlID(hwnd),"chars":GetWindowTextLengthW(hwnd)}),
            );
            return 0;
        }
        if message == WM_CHAR && w == 1 {
            return 0;
        }
        DefSubclassProc(hwnd, message, w, l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_line_is_centered_and_multiline_keeps_top_padding() {
        for dpi in [96, 144, 192] {
            let height = 28 * dpi as i32 / 96;
            let font = 16 * dpi as i32 / 96;
            let (top, bottom) = vertical_padding(height, font, dpi, false);
            assert!((top - bottom).abs() <= 1);
            assert!(top > 0);
            assert_eq!(height - top - bottom, font + 2);
            assert_eq!(
                vertical_padding(100, font, dpi, true),
                (4 * dpi as i32 / 96, 4 * dpi as i32 / 96)
            );
        }
    }
}
