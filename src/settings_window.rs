use crate::visual::{Surface, Tip, Tooltips, text::draw_text, theme, wide};
use std::{
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

pub fn message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(wide("UltraBar.SettingsAction.v1").as_ptr())
    })
}
pub struct Row {
    pub command: usize,
    pub title: String,
    pub detail: String,
    pub plugin_id: Option<String>,
}
pub struct Snapshot {
    pub pages: Vec<Vec<Row>>,
    pub plugins: Vec<crate::settings_editor::PluginPage>,
}
struct State {
    owner: HWND,
    data: Snapshot,
    tab: usize,
    scroll: usize,
    dpi: i32,
    font: HFONT,
    heading: HFONT,
    icons: HFONT,
    capture: Option<std::path::PathBuf>,
    editor: crate::settings_editor::Editor,
    nav_scroll: usize,
    tips: Option<Tooltips>,
}
pub struct Window {
    hwnd: HWND,
    state: Box<State>,
}
impl Window {
    pub unsafe fn new(owner: HWND, data: Snapshot) -> Option<Self> {
        unsafe {
            let module = GetModuleHandleW(null());
            let class = wide("UltraBar.Settings");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: module,
                lpszClassName: class.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                ..zeroed()
            });
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let font = |face: &str, height: i32, weight: i32| {
                ultra_bar_native_ui::fonts::create_font(
                    -height * dpi / 96,
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
                    wide(face).as_ptr(),
                )
            };
            let mut state = Box::new(State {
                owner,
                data,
                tab: 0,
                scroll: 0,
                dpi,
                font: font("Segoe UI", 13, 400),
                heading: font("Segoe UI", 22, 600),
                icons: font("Segoe MDL2 Assets", 18, 400),
                capture: None,
                editor: Default::default(),
                nav_scroll: 0,
                tips: None,
            });
            let mut work: MONITORINFO = zeroed();
            work.cbSize = size_of::<MONITORINFO>() as _;
            GetMonitorInfoW(
                MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST),
                &mut work,
            );
            let (w, h) = (700 * dpi / 96, 540 * dpi / 96);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.as_ptr(),
                wide("Ultra Bar 设置").as_ptr(),
                WS_POPUP | WS_CLIPCHILDREN,
                work.rcWork.left + (work.rcWork.right - work.rcWork.left - w) / 2,
                work.rcWork.top + (work.rcWork.bottom - work.rcWork.top - h) / 2,
                w,
                h,
                owner,
                null_mut(),
                module,
                (&mut *state as *mut State).cast(),
            );
            if hwnd.is_null() {
                ultra_bar_native_ui::fonts::delete_object(state.font);
                ultra_bar_native_ui::fonts::delete_object(state.heading);
                ultra_bar_native_ui::fonts::delete_object(state.icons);
                return None;
            }
            let glass = crate::visual::enable_glass(hwnd);
            ultra_bar_native_ui::diagnostics::event(
                "render",
                "settings.created",
                serde_json::json!({"hwnd":hwnd as usize,"dpi":dpi,"glass":glass,"alpha":if glass{244}else{255},"plugin_pages":state.data.plugins.len()}),
            );
            SetLayeredWindowAttributes(hwnd, 0, if glass { 244 } else { 255 }, LWA_ALPHA);
            SetWindowRgn(
                hwnd,
                CreateRoundRectRgn(0, 0, w, h, 20 * dpi / 96, 20 * dpi / 96),
                1,
            );
            state
                .editor
                .update(hwnd, &state.data.plugins, state.font, state.dpi);
            state.tips = Some(Tooltips::new(hwnd));
            sync_tips(&mut state);
            Some(Self { hwnd, state })
        }
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    pub fn plugin_id(&self, command: usize) -> Option<&str> {
        self.state
            .data
            .pages
            .iter()
            .flatten()
            .find(|row| row.command == command)
            .and_then(|row| row.plugin_id.as_deref())
    }
    pub fn take_save(&mut self) -> Option<(String, u64, Vec<String>)> {
        self.state.editor.take_save()
    }
    pub fn has_unsaved(&self) -> bool {
        self.state.editor.has_unsaved()
    }
    pub unsafe fn save_error(&mut self, id: &str, error: String) {
        unsafe {
            self.state.editor.error(id, error);
            InvalidateRect(self.hwnd, null(), 0);
        }
    }
    pub unsafe fn capture(&mut self, directory: &std::path::Path) {
        unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            for tab in 0..4 + self.state.data.plugins.len() {
                self.state.tab = tab;
                let id = self
                    .state
                    .data
                    .plugins
                    .get(tab.wrapping_sub(4))
                    .map(|p| p.id.clone());
                self.state
                    .editor
                    .select(self.hwnd, id, self.state.font, self.state.dpi);
                self.state.capture = Some(directory.join(format!("settings-{tab}.bmp")));
                SendMessageW(self.hwnd, WM_PAINT, 0, 0);
            }
            self.state.capture = None;
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
    pub unsafe fn update(&mut self, data: Snapshot) {
        unsafe {
            let selected = self
                .state
                .data
                .plugins
                .get(self.state.tab.wrapping_sub(4))
                .map(|p| p.id.clone());
            self.state
                .editor
                .update(self.hwnd, &data.plugins, self.state.font, self.state.dpi);
            if let Some(id) = selected {
                self.state.tab = data
                    .plugins
                    .iter()
                    .position(|p| p.id == id)
                    .map_or(3, |i| 4 + i);
            }
            self.state.nav_scroll = self
                .state
                .nav_scroll
                .min(data.plugins.len().saturating_sub(5));
            self.state.data = data;
            sync_tips(&mut self.state);
            InvalidateRect(self.hwnd, null(), 0);
        }
    }
    pub unsafe fn show_plugin(&mut self, id: &str) {
        unsafe {
            if let Some(index) = self.state.data.plugins.iter().position(|p| p.id == id) {
                self.state.tab = 4 + index;
                self.state.nav_scroll = index.saturating_sub(4);
                self.state.editor.select(
                    self.hwnd,
                    Some(id.into()),
                    self.state.font,
                    self.state.dpi,
                );
                InvalidateRect(self.hwnd, null(), 0);
            }
            self.show();
        }
    }
    pub unsafe fn show(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
            ultra_bar_native_ui::fonts::delete_object(self.state.font);
            ultra_bar_native_ui::fonts::delete_object(self.state.heading);
            ultra_bar_native_ui::fonts::delete_object(self.state.icons);
        }
    }
}
const TABS: [&str; 4] = ["外观", "时间", "托盘", "插件"];
unsafe fn sync_tips(s: &mut State) {
    unsafe {
        let u = |n| n * s.dpi / 96;
        let mut tips = vec![Tip {
            bounds: (u(646), 0, u(700), u(58)),
            text: "关闭".into(),
        }];
        if s.tab >= 4 {
            tips.extend([
                Tip {
                    bounds: (u(545), u(478), u(590), u(524)),
                    text: "重新载入，放弃未保存的修改".into(),
                },
                Tip {
                    bounds: (u(610), u(478), u(676), u(524)),
                    text: "保存".into(),
                },
            ]);
        }
        for (i, plugin) in s.data.plugins.iter().enumerate().skip(s.nav_scroll).take(5) {
            let y = 306 + (i - s.nav_scroll) as i32 * 42;
            tips.push(Tip {
                bounds: (u(12), u(y), u(160), u(y + 38)),
                text: plugin.name.clone(),
            });
        }
        if let Some(control) = &mut s.tips {
            control.sync(tips);
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
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, w, l);
        }
        let s = &mut *ptr;
        let unit = |v| v * s.dpi / 96;
        match msg {
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_KEYDOWN if w == 0x1b => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_NCHITTEST => {
                let mut p = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                ScreenToClient(hwnd, &mut p);
                if p.y < unit(62) && p.x < unit(646) {
                    HTCAPTION as _
                } else {
                    HTCLIENT as _
                }
            }
            WM_MOUSEWHEEL => {
                let mut p = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                ScreenToClient(hwnd, &mut p);
                if p.x < unit(164) {
                    s.nav_scroll = if ((w >> 16) as i16) < 0 {
                        (s.nav_scroll + 1).min(s.data.plugins.len().saturating_sub(5))
                    } else {
                        s.nav_scroll.saturating_sub(1)
                    };
                    sync_tips(s);
                    InvalidateRect(hwnd, null(), 0);
                    return 0;
                }
                if s.tab >= 4 {
                    s.editor.scroll(hwnd, ((w >> 16) as i16) < 0, s.dpi);
                    return 0;
                }
                let count = s.data.pages.get(s.tab).map_or(0, Vec::len);
                if ((w >> 16) as i16) < 0 {
                    s.scroll = (s.scroll + 1).min(count.saturating_sub(6));
                } else {
                    s.scroll = s.scroll.saturating_sub(1);
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONUP => {
                let x = l as i16 as i32 * 96 / s.dpi;
                let y = (l >> 16) as i16 as i32 * 96 / s.dpi;
                if y < 58 && x > 646 {
                    ShowWindow(hwnd, SW_HIDE);
                } else if x < 164 && (88..280).contains(&y) {
                    s.tab = ((y - 88) / 48) as usize;
                    s.editor.select(hwnd, None, s.font, s.dpi);
                    s.scroll = 0;
                    InvalidateRect(hwnd, null(), 0);
                } else if x < 164 && (306..516).contains(&y) {
                    let index = s.nav_scroll + ((y - 306) / 42) as usize;
                    if let Some(plugin) = s.data.plugins.get(index) {
                        s.tab = 4 + index;
                        s.editor
                            .select(hwnd, Some(plugin.id.clone()), s.font, s.dpi);
                    }
                } else if s.tab >= 4 {
                    if s.editor.click(hwnd, x, y, s.font, s.icons, s.dpi) {
                        PostMessageW(s.owner, message(), 90, 0);
                    }
                } else if x >= 184
                    && (100..496).contains(&y)
                    && let Some(row) = s
                        .data
                        .pages
                        .get(s.tab)
                        .and_then(|rows| rows.get(s.scroll + ((y - 100) / 66) as usize))
                {
                    PostMessageW(s.owner, message(), row.command, 0);
                }
                sync_tips(s);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == EN_CHANGE => {
                s.editor.changed();
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_CTLCOLOREDIT => {
                SetBkColor(w as HDC, theme::palette().field);
                SetTextColor(w as HDC, theme::palette().text);
                theme::field_brush() as isize
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let mut ps = zeroed();
                BeginPaint(hwnd, &mut ps);
                let mut area: RECT = zeroed();
                GetClientRect(hwnd, &mut area);
                if let Some(surface) = Surface::new(area.right, area.bottom) {
                    let dc = surface.dc;
                    let palette = theme::palette();
                    FillRect(dc, &area, theme::background_brush());
                    SetBkMode(dc, TRANSPARENT as _);
                    let old = SelectObject(dc, s.font);
                    let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                    let text = |value: &str, x, y, width, height, color| {
                        SetTextColor(dc, color);
                        let mut r = RECT {
                            left: unit(x),
                            top: unit(y),
                            right: unit(x + width),
                            bottom: unit(y + height),
                        };
                        draw_text(
                            dc,
                            wide(value).as_ptr(),
                            -1,
                            &mut r,
                            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                        );
                    };
                    SelectObject(dc, s.heading);
                    text("设置", 24, 16, 150, 40, palette.text);
                    let title = if s.tab < 4 {
                        TABS[s.tab]
                    } else {
                        s.data
                            .plugins
                            .get(s.tab - 4)
                            .map_or("插件设置", |p| p.name.as_str())
                    };
                    text(title, 194, 33, 450, 40, palette.text);
                    SelectObject(dc, s.font);
                    for (i, tab) in TABS.iter().enumerate() {
                        if i == s.tab {
                            let brush = CreateSolidBrush(palette.field);
                            let b = SelectObject(dc, brush);
                            RoundRect(
                                dc,
                                unit(12),
                                unit(88 + i as i32 * 48),
                                unit(160),
                                unit(130 + i as i32 * 48),
                                unit(12),
                                unit(12),
                            );
                            SelectObject(dc, b);
                            ultra_bar_native_ui::fonts::delete_object(brush);
                        }
                        text(
                            tab,
                            30,
                            88 + i as i32 * 48,
                            120,
                            42,
                            if i == s.tab {
                                palette.text
                            } else {
                                palette.muted
                            },
                        );
                    }
                    if !s.data.plugins.is_empty() {
                        text("插件设置", 24, 282, 130, 20, palette.muted);
                        for (i, plugin) in
                            s.data.plugins.iter().enumerate().skip(s.nav_scroll).take(5)
                        {
                            let y = 306 + (i - s.nav_scroll) as i32 * 42;
                            if s.tab == 4 + i {
                                let brush = SelectObject(dc, theme::field_brush());
                                RoundRect(
                                    dc,
                                    unit(12),
                                    unit(y),
                                    unit(160),
                                    unit(y + 38),
                                    unit(12),
                                    unit(12),
                                );
                                SelectObject(dc, brush);
                            }
                            text(
                                &plugin.name,
                                24,
                                y,
                                132,
                                38,
                                if s.tab == 4 + i {
                                    palette.text
                                } else {
                                    palette.muted
                                },
                            );
                        }
                    }
                    if s.tab >= 4 {
                        s.editor.draw(&surface, s.dpi, s.font, s.icons);
                    }
                    if let Some(rows) = s.data.pages.get(s.tab) {
                        for (i, row) in rows.iter().skip(s.scroll).take(6).enumerate() {
                            let y = 100 + i as i32 * 66;
                            let b = SelectObject(dc, theme::field_brush());
                            RoundRect(
                                dc,
                                unit(184),
                                unit(y),
                                unit(676),
                                unit(y + 58),
                                unit(12),
                                unit(12),
                            );
                            SelectObject(dc, b);
                            text(&row.title, 200, y + 5, 420, 25, palette.text);
                            text(&row.detail, 200, y + 29, 420, 20, palette.muted);
                            if matches!(row.command, 41 | 42) {
                                ultra_bar_native_ui::form::draw_switch(
                                    dc,
                                    RECT {
                                        left: unit(620),
                                        top: unit(y + 18),
                                        right: unit(660),
                                        bottom: unit(y + 40),
                                    },
                                    row.detail == "开启",
                                    true,
                                );
                            } else {
                                SelectObject(dc, s.icons);
                                text("\u{e76c}", 644, y + 14, 24, 28, palette.muted);
                            }
                            SelectObject(dc, s.font);
                        }
                    }
                    SelectObject(dc, s.icons);
                    text("\u{e711}", 656, 18, 24, 30, palette.text);
                    SelectObject(dc, old);
                    SelectObject(dc, pen);
                    if let Some(path) = &s.capture {
                        s.editor.capture_controls(dc);
                        let _ = surface.save_bmp(path);
                    }
                    surface.blit_opaque(ps.hdc);
                }
                EndPaint(hwnd, &ps);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
