//! Modal glass settings form with native IME edits and in-app choice controls.
use crate::{Surface, enable_glass, text::draw_text, theme, wide};
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Controls::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};
thread_local! {static CAPTURE:std::cell::RefCell<Option<(std::path::PathBuf,i32)>>=const{std::cell::RefCell::new(None)};}
/// Render the actual form offscreen without changing settings or sending global input.
pub fn capture(directory: &std::path::Path, dpi: i32, fields: Vec<Field>) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    CAPTURE.with(|c| *c.borrow_mut() = Some((directory.join(format!("settings-{dpi}.bmp")), dpi)));
    unsafe {
        edit(null_mut(), "地区与天气", fields);
    }
    CAPTURE.with(|c| *c.borrow_mut() = None);
    Ok(())
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Secret,
    Choice(Vec<String>),
    Link,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub kind: Kind,
    #[serde(skip)]
    pub validate: Option<Validator>,
}
pub type Validator = fn(&str) -> Result<(), String>;
pub fn switch_state(field: &Field) -> Option<bool> {
    let Kind::Choice(choices) = &field.kind else {
        return None;
    };
    if choices.len() != 2 {
        return None;
    }
    let binary = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "开启" | "启用" | "显示" | "是" | "on" | "true" => Some(true),
        "关闭" | "禁用" | "隐藏" | "否" | "off" | "false" => Some(false),
        _ => None,
    };
    let a = binary(&choices[0])?;
    let b = binary(&choices[1])?;
    if a == b {
        return None;
    }
    Some(if field.value == "1" { b } else { a })
}
#[cfg(test)]
mod switch_tests {
    use super::*;
    #[test]
    fn switches_follow_boolean_meanings_without_turning_two_way_selections_into_toggles() {
        assert_eq!(
            switch_state(&Field::choice("开关", 1, &["关闭", "开启"])),
            Some(true)
        );
        assert_eq!(
            switch_state(&Field::choice("开关", 0, &["关闭", "开启"])),
            Some(false)
        );
        assert_eq!(
            switch_state(&Field::choice("反序", 0, &["开启", "关闭"])),
            Some(true)
        );
        assert_eq!(
            switch_state(&Field::choice("农历", 0, &["隐藏", "显示"])),
            Some(false)
        );
        assert_eq!(
            switch_state(&Field::choice("星期", 0, &["星期一", "星期日"])),
            None
        );
        assert_eq!(
            switch_state(&Field::choice("单位", 0, &["摄氏度 °C", "华氏度 °F"])),
            None
        );
        assert_eq!(
            switch_state(&Field::choice(
                "模式",
                0,
                &["仅时间", "并排展示", "轮播展示"]
            )),
            None
        );
    }
}
/// # Safety
/// Draw on the UI thread using a valid target DC.
pub unsafe fn draw_switch(dc: HDC, bounds: RECT, checked: bool, enabled: bool) {
    unsafe {
        let w = bounds.right - bounds.left;
        let h = bounds.bottom - bounds.top;
        if h <= 0 || w < h {
            return;
        }
        let p = theme::palette();
        let base = theme::colorref(p.field);
        let track = theme::colorref(if checked && enabled {
            p.accent
        } else {
            p.disabled
        });
        let knob = theme::colorref(p.text);
        let blend = |a: u32, b: u32, t: f64| -> u32 {
            let c = |shift: u32| {
                ((((a >> shift) & 255u32) as f64 * (1. - t) + ((b >> shift) & 255u32) as f64 * t)
                    .round() as u32)
                    << shift
            };
            c(16) | c(8) | c(0)
        };
        let radius = h as f64 / 2.;
        let gap = (h as f64 / 12.).max(2.);
        let inner = radius - gap;
        let center = if checked {
            w as f64 - gap - inner
        } else {
            gap + inner
        };
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                let (x, y) = (x as f64 + 0.5, y as f64 + 0.5);
                let cap = (0.5
                    - ((x - x.clamp(radius, w as f64 - radius)).hypot(y - radius) - radius))
                    .clamp(0., 1.);
                let thumb = (0.5 - ((x - center).hypot(y - radius) - inner)).clamp(0., 1.);
                pixels.push(0xff000000 | blend(blend(base, track, cap), knob, thumb));
            }
        }
        let mut info: BITMAPINFO = zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as _;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        SetDIBitsToDevice(
            dc,
            bounds.left,
            bounds.top,
            w as _,
            h as _,
            0,
            0,
            0,
            h as _,
            pixels.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
        );
    }
}

impl Field {
    pub fn text(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            kind: Kind::Text,
            validate: None,
        }
    }
    pub fn secret(label: &str) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            kind: Kind::Secret,
            validate: None,
        }
    }
    pub fn choice(label: &str, index: usize, choices: &[&str]) -> Self {
        Self {
            label: label.into(),
            value: index.to_string(),
            kind: Kind::Choice(choices.iter().map(|s| s.to_string()).collect()),
            validate: None,
        }
    }
    pub fn link(label: &str, url: &str) -> Self {
        Self {
            label: label.into(),
            value: url.into(),
            kind: Kind::Link,
            validate: None,
        }
    }
}
impl Field {
    pub fn validated(mut self, validate: Validator) -> Self {
        self.validate = Some(validate);
        self
    }
}
struct State {
    capture: Option<std::path::PathBuf>,
    height: i32,
    scroll: i32,
    tips: Option<crate::Tooltips>,
    fields: Vec<Field>,
    controls: Vec<HWND>,
    font: HFONT,
    icons: HFONT,
    brush: HBRUSH,
    dpi: i32,
    done: bool,
    saved: bool,
    title: String,
    error: String,
}
/// # Safety
/// Owner must belong to this UI thread, or be null.
pub unsafe fn edit(owner: HWND, title: &str, fields: Vec<Field>) -> Option<Vec<String>> {
    unsafe {
        let mut dpi = if owner.is_null() {
            96
        } else {
            windows_sys::Win32::UI::HiDpi::GetDpiForWindow(owner).max(96) as i32
        };
        if let Some((_, value)) = CAPTURE.with(|c| c.borrow().clone()) {
            dpi = value;
        }
        let scale = |n: i32| n * dpi / 96;
        let module = GetModuleHandleW(null());
        let class = wide("UltraBar.SettingsForm");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let font = |name: &str, h| {
            crate::fonts::create_font(
                -scale(h),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                0,
                0,
                CLEARTYPE_NATURAL_QUALITY as _,
                0,
                wide(name).as_ptr(),
            )
        };
        let mut s = Box::new(State {
            capture: CAPTURE.with(|c| c.borrow().as_ref().map(|(p, _)| p.clone())),
            height: 0,
            scroll: 0,
            tips: None,
            fields,
            controls: vec![],
            font: font("Segoe UI", 13),
            icons: font("Segoe MDL2 Assets", 18),
            brush: CreateSolidBrush(theme::palette().field),
            dpi,
            done: false,
            saved: false,
            title: title.into(),
            error: String::new(),
        });
        let mut height = scale(100 + s.fields.len() as i32 * 53);
        let width = scale(490);
        let mut area: RECT = zeroed();
        GetWindowRect(owner, &mut area);
        let mut work: RECT = zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0);
        height = height.min(work.bottom - work.top - scale(16));
        s.height = height * 96 / dpi;
        let x = (area.right - width).clamp(work.left, (work.right - width).max(work.left));
        let y = (area.bottom - height).clamp(work.top, (work.bottom - height).max(work.top));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class.as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_CLIPCHILDREN,
            if s.capture.is_some() { -32000 } else { x },
            if s.capture.is_some() { -32000 } else { y },
            width,
            height,
            owner,
            null_mut(),
            module,
            (&mut *s as *mut State).cast(),
        );
        if hwnd.is_null() {
            crate::fonts::delete_object(s.font);
            crate::fonts::delete_object(s.icons);
            crate::fonts::delete_object(s.brush);
            return None;
        }
        let glass = enable_glass(hwnd);
        SetLayeredWindowAttributes(hwnd, 0, if glass { 242 } else { 255 }, LWA_ALPHA);
        SetWindowRgn(
            hwnd,
            CreateRoundRectRgn(0, 0, width, height, scale(18), scale(18)),
            1,
        );
        for (i, f) in s.fields.iter().enumerate() {
            let control = if matches!(f.kind, Kind::Text | Kind::Secret) {
                let edit = CreateWindowExW(
                    0,
                    wide("EDIT").as_ptr(),
                    wide(&f.value).as_ptr(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | ES_AUTOHSCROLL as u32
                        | if matches!(f.kind, Kind::Secret) {
                            ES_PASSWORD as u32
                        } else {
                            0
                        },
                    scale(18),
                    scale(67 + i as i32 * 53),
                    scale(454),
                    scale(25),
                    hwnd,
                    (100 + i) as _,
                    module,
                    null(),
                );
                SendMessageW(edit, WM_SETFONT, s.font as usize, 1);
                SendMessageW(edit, EM_SETLIMITTEXT, 4096, 0);
                crate::input::attach(edit);
                edit
            } else {
                null_mut()
            };
            s.controls.push(control);
        }
        if !owner.is_null() {
            EnableWindow(owner, 0);
        }
        s.tips = Some(crate::Tooltips::new(hwnd));
        if let Some(tips) = s.tips.as_mut() {
            tips.sync(vec![
                crate::Tip {
                    bounds: (scale(380), height - scale(45), scale(430), height),
                    text: "保存".into(),
                },
                crate::Tip {
                    bounds: (scale(430), height - scale(45), scale(490), height),
                    text: "取消".into(),
                },
            ]);
        }
        position_controls(&s);
        ShowWindow(
            hwnd,
            if s.capture.is_some() {
                SW_SHOWNOACTIVATE
            } else {
                SW_SHOW
            },
        );
        if s.capture.is_none() {
            SetForegroundWindow(hwnd);
        }
        if let Some(first) = s.controls.iter().find(|p| !p.is_null())
            && s.capture.is_none()
        {
            SetFocus(*first);
        }
        let mut message: MSG = zeroed();
        let mut quit = None;
        while !s.done {
            let status = GetMessageW(&mut message, null_mut(), 0, 0);
            if status <= 0 {
                if status == 0 {
                    quit = Some(message.wParam);
                }
                break;
            }
            if message.message == WM_KEYDOWN && message.wParam == 0x1b {
                s.done = true;
            } else if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let result = if s.saved {
            Some(
                s.fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let edit = s.controls[i];
                        if edit.is_null() {
                            f.value.clone()
                        } else {
                            let mut text = vec![0u16; 4097];
                            let n = GetWindowTextW(edit, text.as_mut_ptr(), text.len() as _);
                            String::from_utf16_lossy(&text[..n.max(0) as usize])
                        }
                    })
                    .collect(),
            )
        } else {
            None
        };
        s.tips.take();
        DestroyWindow(hwnd);
        if !owner.is_null() {
            EnableWindow(owner, 1);
            SetForegroundWindow(owner);
        }
        crate::fonts::delete_object(s.font);
        crate::fonts::delete_object(s.icons);
        crate::fonts::delete_object(s.brush);
        if let Some(code) = quit {
            PostQuitMessage(code as i32);
        }
        result
    }
}
unsafe fn position_controls(s: &State) {
    unsafe {
        for (i, control) in s.controls.iter().enumerate() {
            if control.is_null() {
                continue;
            }
            let y = 67 + i as i32 * 53 - s.scroll;
            let visible = y >= 42 && y + 25 <= s.height - 50;
            ShowWindow(*control, if visible { SW_SHOWNA } else { SW_HIDE });
            SetWindowPos(
                *control,
                null_mut(),
                18 * s.dpi / 96,
                y * s.dpi / 96,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if p.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let s = &mut *p;
        let scale = |n: i32| n * s.dpi / 96;
        match msg {
            WM_MOUSEWHEEL => {
                let delta = (w >> 16) as i16 as i32;
                let max = (100 + s.fields.len() as i32 * 53 - s.height).max(0);
                s.scroll = (s.scroll - delta.signum() * 53).clamp(0, max);
                position_controls(s);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == EN_SETFOCUS => {
                let i = (w & 0xffff).saturating_sub(100);
                if i < s.fields.len() {
                    let y = 67 + i as i32 * 53 - s.scroll;
                    if y < 67 {
                        s.scroll = (i as i32 * 53).max(0);
                    } else if y + 25 > s.height - 50 {
                        s.scroll += y + 25 - (s.height - 50);
                    }
                    position_controls(s);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                ScreenToClient(hwnd, &mut pt);
                if pt.y < scale(40) && pt.x < scale(440) {
                    HTCAPTION as _
                } else {
                    HTCLIENT as _
                }
            }
            WM_CLOSE => {
                s.done = true;
                0
            }
            WM_CTLCOLOREDIT => {
                SetBkColor(w as _, theme::palette().field);
                SetTextColor(w as _, theme::palette().text);
                s.brush as _
            }
            WM_LBUTTONUP => {
                let x = l as i16 as i32 * 96 / s.dpi;
                let y = (l >> 16) as i16 as i32 * 96 / s.dpi;
                let bottom = s.height;
                if (y < 40 && x > 440) || (y > bottom - 45 && x > 430) {
                    s.done = true;
                } else if y > bottom - 45 && x > 380 {
                    s.error.clear();
                    for (i, field) in s.fields.iter().enumerate() {
                        if let Some(validate) = field.validate {
                            let mut text = vec![0u16; 4097];
                            let n =
                                GetWindowTextW(s.controls[i], text.as_mut_ptr(), text.len() as _);
                            let value = if s.controls[i].is_null() {
                                field.value.clone()
                            } else {
                                String::from_utf16_lossy(&text[..n.max(0) as usize])
                            };
                            if let Err(error) = validate(&value) {
                                s.error = error;
                                SetFocus(s.controls[i]);
                                InvalidateRect(hwnd, null(), 0);
                                return 0;
                            }
                        }
                    }
                    s.saved = true;
                    s.done = true;
                } else if y >= 67 && y < bottom - 50 {
                    let i = ((y - 67 + s.scroll) / 53) as usize;
                    if let Some(f) = s.fields.get_mut(i) {
                        match &f.kind {
                            Kind::Choice(v) => {
                                f.value = ((f.value.parse::<usize>().unwrap_or(0) + 1)
                                    % v.len().max(1))
                                .to_string();
                                InvalidateRect(hwnd, null(), 0);
                            }
                            Kind::Link => {
                                ShellExecuteW(
                                    hwnd,
                                    wide("open").as_ptr(),
                                    wide(&f.value).as_ptr(),
                                    null(),
                                    null(),
                                    SW_SHOWNORMAL,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                0
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps = zeroed();
                BeginPaint(hwnd, &mut ps);
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                if let Some(surface) = Surface::new(rect.right, rect.bottom) {
                    let dc = surface.dc;
                    let p = theme::palette();
                    let brush = CreateSolidBrush(p.background);
                    FillRect(dc, &rect, brush);
                    crate::fonts::delete_object(brush);
                    SelectObject(dc, s.font);
                    SetBkMode(dc, TRANSPARENT as _);
                    let text = |value: &str, x, y, width, height, color| {
                        SetTextColor(dc, color);
                        let mut r = RECT {
                            left: scale(x),
                            top: scale(y),
                            right: scale(x + width),
                            bottom: scale(y + height),
                        };
                        draw_text(
                            dc,
                            wide(value).as_ptr(),
                            -1,
                            &mut r,
                            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );
                    };
                    text(&s.title, 18, 8, 410, 30, p.text);
                    let saved = SaveDC(dc);
                    IntersectClipRect(dc, scale(12), scale(42), scale(478), scale(s.height - 50));
                    for (i, f) in s.fields.iter().enumerate() {
                        let y = 45 + i as i32 * 53 - s.scroll;
                        if y + 47 < 42 || y > s.height - 50 {
                            continue;
                        }
                        let brush = CreateSolidBrush(p.field);
                        let old = SelectObject(dc, brush);
                        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                        RoundRect(
                            dc,
                            scale(18),
                            scale(if switch_state(f).is_some() { y } else { y + 22 }),
                            scale(472),
                            scale(y + 47),
                            scale(8),
                            scale(8),
                        );
                        SelectObject(dc, old);
                        SelectObject(dc, pen);
                        crate::fonts::delete_object(brush);
                        if let Some(checked) = switch_state(f) {
                            text(&f.label, 25, y + 9, 360, 30, p.text);
                            draw_switch(
                                dc,
                                RECT {
                                    left: scale(416),
                                    top: scale(y + 13),
                                    right: scale(458),
                                    bottom: scale(y + 37),
                                },
                                checked,
                                true,
                            );
                            continue;
                        }
                        text(&f.label, 18, y, 454, 21, p.muted);
                        match &f.kind {
                            Kind::Choice(v) => {
                                text(
                                    v.get(f.value.parse::<usize>().unwrap_or(0))
                                        .map(String::as_str)
                                        .unwrap_or("—"),
                                    25,
                                    y + 22,
                                    420,
                                    25,
                                    p.text,
                                );
                                text("›", 450, y + 22, 20, 25, p.muted);
                            }
                            Kind::Link => text("打开配置指南 ↗", 25, y + 22, 420, 25, p.text),
                            _ => {}
                        }
                    }
                    RestoreDC(dc, saved);
                    text(&s.error, 18, s.height - 40, 350, 30, p.text);
                    let content = 100 + s.fields.len() as i32 * 53;
                    if content > s.height {
                        let track = s.height - 112;
                        let thumb = (track * (s.height - 100) / (content - 100)).max(16);
                        let y = 52 + (track - thumb) * s.scroll / (content - s.height).max(1);
                        let brush = CreateSolidBrush(p.muted);
                        let r = RECT {
                            left: scale(482),
                            top: scale(y),
                            right: scale(484),
                            bottom: scale(y + thumb),
                        };
                        FillRect(dc, &r, brush);
                        crate::fonts::delete_object(brush);
                    }
                    SelectObject(dc, s.icons);
                    text("\u{e711}", 448, 8, 25, 30, p.muted);
                    let bottom = s.height;
                    text("\u{e73e}", 390, bottom - 40, 30, 30, p.text);
                    text("\u{e711}", 440, bottom - 40, 30, 30, p.muted);
                    surface.blit_opaque(ps.hdc);
                    if let Some(path) = s.capture.take() {
                        for control in &s.controls {
                            if control.is_null() || IsWindowVisible(*control) == 0 {
                                continue;
                            }
                            let mut r: RECT = zeroed();
                            GetWindowRect(*control, &mut r);
                            let mut p = POINT {
                                x: r.left,
                                y: r.top,
                            };
                            ScreenToClient(hwnd, &mut p);
                            let saved = SaveDC(dc);
                            SetViewportOrgEx(dc, p.x, p.y, null_mut());
                            SendMessageW(
                                *control,
                                WM_PRINTCLIENT,
                                dc as usize,
                                (PRF_CLIENT | PRF_ERASEBKGND) as isize,
                            );
                            RestoreDC(dc, saved);
                        }
                        let _ = surface.save_bmp(&path);
                        PostMessageW(hwnd, WM_CLOSE, 0, 0);
                    }
                }
                EndPaint(hwnd, &ps);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
