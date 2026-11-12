//! Pointer-driven tray hover relay. No polling or animation timers.
use super::{Command, HoverEvent};
pub const HOVER_TIMER: usize = 8;
use std::sync::mpsc::SyncSender;
use windows_sys::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::ClientToScreen,
    UI::Input::KeyboardAndMouse::*,
};
#[derive(Default)]
pub struct Hover {
    key: Option<String>,
    point: (i32, i32),
    entered: bool,
    opened: bool,
    owner: usize,
}
impl Hover {
    /// # Safety
    /// Window and client point must belong to the calling UI thread.
    pub unsafe fn motion(
        &mut self,
        hwnd: HWND,
        key: Option<String>,
        x: i32,
        y: i32,
        sender: &SyncSender<Command>,
    ) {
        unsafe {
            self.owner = hwnd as usize;
            let changed = self.key != key;
            if changed {
                eprintln!("[tray-hover] target {:?} -> {:?}", self.key, key);
                self.leave(sender);
                self.key = key;
                if self.key.is_some() && !hwnd.is_null() {
                    let mut delay = 400u32;
                    windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
                        windows_sys::Win32::UI::WindowsAndMessaging::SPI_GETMOUSEHOVERTIME,
                        0,
                        (&mut delay as *mut u32).cast(),
                        0,
                    );
                    windows_sys::Win32::UI::WindowsAndMessaging::SetTimer(
                        hwnd,
                        HOVER_TIMER,
                        delay.max(100),
                        None,
                    );
                }
            }
            if !self.entered {
                let mut point = POINT { x, y };
                ClientToScreen(hwnd, &mut point);
                self.point = (point.x, point.y);
            }
            if let Some(key) = &self.key {
                // A legacy app may rebuild/reposition its preview for every WM_MOUSEMOVE.
                // Enter once per icon, keeping the same anchor until the pointer leaves.
                if !self.entered
                    && sender
                        .try_send(Command::Hover(key.clone(), HoverEvent::Move, self.point))
                        .is_ok()
                {
                    self.entered = true;
                }
            }
            self.track(hwnd);
        }
    }
    pub fn matches(&self, key: &str) -> bool {
        self.key.as_deref() == Some(key)
    }
    /// # Safety
    /// Window is on this UI thread. There is one tracking request, shared by hover and leave.
    pub unsafe fn track(&self, hwnd: HWND) {
        unsafe {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as _,
                dwFlags: TME_LEAVE
                    | if self.key.is_some() && !self.opened {
                        TME_HOVER
                    } else {
                        0
                    },
                hwndTrack: hwnd,
                dwHoverTime: u32::MAX,
            };
            TrackMouseEvent(&mut track);
        }
    }
    pub fn open(&mut self, sender: &SyncSender<Command>) {
        if self.owner != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(
                    self.owner as _,
                    HOVER_TIMER,
                );
            }
        }
        if let Some(key) = &self.key {
            if !self.entered {
                if sender
                    .try_send(Command::Hover(key.clone(), HoverEvent::Move, self.point))
                    .is_err()
                {
                    return;
                }
                self.entered = true;
            }
            if !self.opened
                && sender
                    .try_send(Command::Hover(key.clone(), HoverEvent::Open, self.point))
                    .is_ok()
            {
                eprintln!("[tray-hover] open {key}");
                self.opened = true;
            }
        }
    }
    pub fn leave(&mut self, sender: &SyncSender<Command>) {
        if self.owner != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::KillTimer(
                    self.owner as _,
                    HOVER_TIMER,
                );
            }
        }
        if let Some(key) = self.key.take() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let mut p = POINT { x: 0, y: 0 };
                GetCursorPos(&mut p);
                let w = WindowFromPoint(p);
                let mut class = [0u16; 128];
                let n = GetClassNameW(w, class.as_mut_ptr(), 128);
                eprintln!(
                    "[tray-hover] leave {key} cursor={},{} under={:#x} class={}",
                    p.x,
                    p.y,
                    w as usize,
                    String::from_utf16_lossy(&class[..n.max(0) as usize])
                );
            }
            let _ = sender.try_send(Command::Hover(key, HoverEvent::Close, self.point));
        }
        self.entered = false;
        self.opened = false;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_icon_motion_enters_once_with_a_fixed_anchor_and_exit_is_paired() {
        unsafe {
            let (tx, rx) = std::sync::mpsc::sync_channel(8);
            let mut hover = Hover::default();
            hover.motion(std::ptr::null_mut(), Some("a".into()), 1, 2, &tx);
            hover.motion(std::ptr::null_mut(), Some("a".into()), 2, 2, &tx);
            for x in 3..1000 {
                hover.motion(std::ptr::null_mut(), Some("a".into()), x, 2, &tx);
            }
            assert_eq!(hover.point, (1, 2));
            hover.open(&tx);
            hover.open(&tx);
            hover.motion(std::ptr::null_mut(), Some("b".into()), 3, 2, &tx);
            hover.leave(&tx);
            let mut events = vec![];
            while let Ok(Command::Hover(key, event, _)) = rx.try_recv() {
                events.push((key, event));
            }
            assert_eq!(
                events,
                vec![
                    ("a".into(), HoverEvent::Move),
                    ("a".into(), HoverEvent::Open),
                    ("a".into(), HoverEvent::Close),
                    ("b".into(), HoverEvent::Move),
                    ("b".into(), HoverEvent::Close)
                ]
            );
        }
    }
}
