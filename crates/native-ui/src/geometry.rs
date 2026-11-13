//! One owner for window clipping/hit regions, derived from actual native geometry.
use std::{cell::Cell, mem::zeroed, rc::Rc};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{HiDpi::*, Shell::*, WindowsAndMessaging::*},
};
const ID: usize = 0x554247;
const STATE: &str = "UltraBar.Geometry.State.v1";
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    width: i32,
    height: i32,
    dpi: u32,
    corner: i32,
}
struct Rounded {
    corner: Cell<i32>,
    applied: Cell<Option<Key>>,
    applying: Cell<bool>,
}
unsafe fn retain(data: usize) -> Rc<Rounded> {
    unsafe {
        let pointer = data as *const Rounded;
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    }
}
unsafe fn shape(key: Key) -> HRGN {
    unsafe {
        let diameter = (key.corner * key.dpi as i32 / 96).clamp(0, key.width.min(key.height));
        if diameter <= 2 {
            CreateRectRgn(0, 0, key.width, key.height)
        } else {
            CreateRoundRectRgn(0, 0, key.width + 1, key.height + 1, diameter, diameter)
        }
    }
}
unsafe fn synchronize(hwnd: HWND, state: &Rounded, verify: bool) {
    unsafe {
        if state.applying.get() {
            return;
        }
        let mut rect: RECT = zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }
        let key = Key {
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
            dpi: GetDpiForWindow(hwnd).max(96),
            corner: state.corner.get(),
        };
        if key.width <= 0 || key.height <= 0 || (!verify && state.applied.get() == Some(key)) {
            return;
        }
        state.applying.set(true);
        let next = shape(key);
        if !next.is_null() {
            let previous = CreateRectRgn(0, 0, 0, 0);
            let same = !previous.is_null()
                && GetWindowRgn(hwnd, previous) != 0
                && EqualRgn(previous, next) != 0;
            if !previous.is_null() {
                crate::fonts::delete_object(previous);
            }
            let applied = if same {
                crate::fonts::delete_object(next);
                true
            } else if SetWindowRgn(hwnd, next, 1) != 0 {
                true
            } else {
                crate::fonts::delete_object(next);
                false
            };
            if applied {
                state.applied.set(Some(key));
            }
        }
        state.applying.set(false);
    }
}
unsafe extern "system" fn proc(
    hwnd: HWND,
    message: u32,
    w: usize,
    l: isize,
    id: usize,
    data: usize,
) -> isize {
    unsafe {
        let state = retain(data);
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(proc), id);
            RemovePropW(hwnd, crate::wide(STATE).as_ptr());
            drop(Rc::from_raw(data as *const Rounded));
            return DefSubclassProc(hwnd, message, w, l);
        }
        let result = DefSubclassProc(hwnd, message, w, l);
        if (matches!(
            message,
            WM_SIZE
                | WM_WINDOWPOSCHANGED
                | WM_SETFONT
                | WM_STYLECHANGED
                | WM_THEMECHANGED
                | WM_DPICHANGED
                | WM_DPICHANGED_AFTERPARENT
        ) || (message == WM_SHOWWINDOW && w != 0))
            && GetPropW(hwnd, crate::wide(STATE).as_ptr()) as usize == data
        {
            synchronize(hwnd, &state, message != WM_SIZE);
        }
        result
    }
}
/// Attach on the window's UI thread. `corner` is the logical ellipse diameter.
/// No dimensions are accepted: painting and hit testing follow the native window bounds.
/// # Safety
/// `hwnd` must be a live window on the calling UI thread.
pub unsafe fn attach_rounded(hwnd: HWND, corner: i32) {
    unsafe {
        if hwnd.is_null() {
            return;
        }
        let corner = corner.clamp(0, 256);
        let existing = GetPropW(hwnd, crate::wide(STATE).as_ptr()) as usize;
        if existing != 0 {
            let state = retain(existing);
            if state.corner.replace(corner) != corner {
                state.applied.set(None);
            }
            synchronize(hwnd, &state, true);
            return;
        }
        let state = Rc::new(Rounded {
            corner: Cell::new(corner),
            applied: Cell::new(None),
            applying: Cell::new(false),
        });
        let owned = Rc::into_raw(state.clone());
        if SetWindowSubclass(hwnd, Some(proc), ID, owned as usize) == 0 {
            drop(Rc::from_raw(owned));
            return;
        }
        if SetPropW(hwnd, crate::wide(STATE).as_ptr(), owned as HANDLE) == 0 {
            RemoveWindowSubclass(hwnd, Some(proc), ID);
            drop(Rc::from_raw(owned));
            return;
        }
        synchronize(hwnd, &state, true);
    }
}
/// # Safety
/// Read on the attached window's UI thread.
pub unsafe fn corner_pixels(hwnd: HWND) -> i32 {
    unsafe {
        let data = GetPropW(hwnd, crate::wide(STATE).as_ptr()) as usize;
        let corner = if data != 0 {
            retain(data).corner.get()
        } else {
            10
        };
        let mut bounds: RECT = zeroed();
        GetWindowRect(hwnd, &mut bounds);
        (corner * GetDpiForWindow(hwnd).max(96) as i32 / 96).clamp(
            0,
            (bounds.right - bounds.left)
                .min(bounds.bottom - bounds.top)
                .max(0),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;
    use windows_sys::Win32::UI::Controls::*;
    unsafe fn verify(hwnd: HWND) {
        unsafe {
            let mut window: RECT = zeroed();
            GetWindowRect(hwnd, &mut window);
            let (width, height) = (window.right - window.left, window.bottom - window.top);
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_ne!(GetWindowRgn(hwnd, region), 0);
            for x in [width / 10, width / 2, width * 9 / 10, width - 2]
                .into_iter()
                .filter(|x| *x >= 0 && *x < width)
            {
                assert_ne!(
                    PtInRegion(region, x, height / 2),
                    0,
                    "dead hit region at x={x}, width={width}"
                );
            }
            let mut bounds: RECT = zeroed();
            GetRgnBox(region, &mut bounds);
            assert_eq!(bounds.right, width);
            assert_eq!(bounds.bottom, height);
            crate::fonts::delete_object(region);
        }
    }
    #[test]
    fn actual_regions_follow_control_resize_font_visibility_and_external_region_changes() {
        unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_DATE_CLASSES,
            });
            let owner = CreateWindowExW(
                WS_EX_NOACTIVATE,
                crate::wide("STATIC").as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                -32000,
                -32000,
                800,
                600,
                null_mut(),
                null_mut(),
                null_mut(),
                std::ptr::null(),
            );
            for (class, style) in [
                ("EDIT", 0),
                ("COMBOBOX", CBS_DROPDOWNLIST as u32),
                ("SysDateTimePick32", 0),
            ] {
                let child = CreateWindowExW(
                    0,
                    crate::wide(class).as_ptr(),
                    std::ptr::null(),
                    WS_CHILD | style,
                    0,
                    0,
                    200,
                    28,
                    owner,
                    null_mut(),
                    null_mut(),
                    std::ptr::null(),
                );
                assert!(!child.is_null());
                attach_rounded(child, 10);
                for width in [1, 480, 120, 640, 200] {
                    MoveWindow(
                        child,
                        10,
                        30,
                        width,
                        if class == "COMBOBOX" { 220 } else { 32 },
                        0,
                    );
                    verify(child);
                    ShowWindow(child, SW_HIDE);
                    ShowWindow(child, SW_SHOWNA);
                    verify(child);
                }
                let wrong = CreateRectRgn(0, 0, 40, 20);
                SetWindowRgn(child, wrong, 0);
                verify(child);
                for corner in [20, 0, 10] {
                    attach_rounded(child, corner);
                    verify(child);
                }
                SendMessageW(
                    child,
                    WM_SETFONT,
                    GetStockObject(DEFAULT_GUI_FONT) as usize,
                    0,
                );
                verify(child);
                let font = crate::fonts::create_font(
                    -22,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    0,
                    0,
                    CLEARTYPE_NATURAL_QUALITY as u32,
                    0,
                    crate::wide("Segoe UI").as_ptr(),
                );
                SendMessageW(child, WM_SETFONT, font as usize, 0);
                verify(child);
                DestroyWindow(child);
                crate::fonts::delete_object(font);
            }
            DestroyWindow(owner);
        }
    }
    #[test]
    fn scaling_and_corner_configuration_preserve_the_whole_visible_hit_area() {
        unsafe {
            for dpi in [96, 120, 144, 192] {
                for corner in [0, 8, 10, 20, 80] {
                    let key = Key {
                        width: 480 * dpi as i32 / 96,
                        height: 28 * dpi as i32 / 96,
                        dpi,
                        corner,
                    };
                    let region = shape(key);
                    for x in [1, key.width / 2, key.width - 2] {
                        assert_ne!(PtInRegion(region, x, key.height / 2), 0);
                    }
                    let mut bounds: RECT = zeroed();
                    GetRgnBox(region, &mut bounds);
                    assert_eq!(bounds.right, key.width);
                    assert_eq!(bounds.bottom, key.height);
                    crate::fonts::delete_object(region);
                }
            }
        }
    }
}
