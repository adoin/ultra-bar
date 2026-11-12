//! Native drag image follows mouse events; there is no animation timer.
use windows_sys::Win32::{
    Foundation::*,
    UI::{Controls::*, WindowsAndMessaging::*},
};
pub struct Ghost {
    image: HIMAGELIST,
}
impl Ghost {
    pub unsafe fn new(icon: HICON, dpi: i32) -> Option<Self> {
        unsafe {
            let size = 16 * dpi / 96;
            let image = ImageList_Create(size, size, ILC_COLOR32 | ILC_MASK, 1, 1);
            if image == 0 {
                return None;
            }
            if ImageList_ReplaceIcon(image, -1, icon) < 0
                || ImageList_BeginDrag(image, 0, size / 2, size / 2) == 0
            {
                ImageList_Destroy(image);
                return None;
            }
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            if ImageList_DragEnter(GetDesktopWindow(), point.x, point.y) == 0 {
                ImageList_EndDrag();
                ImageList_Destroy(image);
                return None;
            }
            Some(Self { image })
        }
    }
    pub unsafe fn follow(&self) {
        unsafe {
            let mut p = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut p) != 0 {
                ImageList_DragMove(p.x, p.y);
            }
        }
    }
}
impl Drop for Ghost {
    fn drop(&mut self) {
        unsafe {
            ImageList_DragLeave(GetDesktopWindow());
            ImageList_EndDrag();
            ImageList_Destroy(self.image);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;
    #[test]
    fn native_ghost_preserves_mouse_capture_and_releases_the_drag_image() {
        unsafe {
            let capture = windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture();
            let icon = LoadIconW(null_mut(), IDI_APPLICATION);
            let ghost = Ghost::new(icon, 96).expect("native drag image");
            ImageList_DragShowNolock(0);
            assert_ne!(ImageList_GetDragImage(null_mut(), null_mut()), 0);
            assert_eq!(
                windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture(),
                capture
            );
            drop(ghost);
            assert_eq!(ImageList_GetDragImage(null_mut(), null_mut()), 0);
        }
    }
}
