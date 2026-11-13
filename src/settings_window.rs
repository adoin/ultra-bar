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
    pub clock_provider: Option<String>,
    pub updates: std::collections::HashMap<String, PluginUpdate>,
}
pub struct PluginUpdate {
    pub version: String,
    pub source: String,
    pub busy: bool,
}
struct State {
    owner: HWND,
    data: Snapshot,
    tab: usize,
    scroll: usize,
    dpi: i32,
    font: HFONT,
    plugin_font: HFONT,
    heading: HFONT,
    icons: HFONT,
    capture: Option<std::path::PathBuf>,
    editor: crate::settings_editor::Editor,
    nav_scroll: usize,
    tips: Option<Tooltips>,
    pending_update: Option<String>,
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
                plugin_font: font("Segoe UI", 12, 600),
                heading: font("Segoe UI", 22, 600),
                icons: font("Segoe MDL2 Assets", 18, 400),
                capture: None,
                editor: Default::default(),
                nav_scroll: 0,
                tips: None,
                pending_update: None,
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
                ultra_bar_native_ui::fonts::delete_object(state.plugin_font);
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
            ultra_bar_native_ui::geometry::attach_rounded(hwnd, 20);
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
    pub fn take_update(&mut self) -> Option<String> {
        self.state.pending_update.take()
    }
    pub fn take_action(
        &mut self,
    ) -> Option<(String, ultra_bar_native_ui::settings_bridge::SaveRequest)> {
        self.state.editor.take_action()
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
            for tab in 0..TABS.len() + self.state.data.plugins.len() {
                self.state.tab = tab;
                let id = self
                    .state
                    .data
                    .plugins
                    .get(tab.wrapping_sub(TABS.len()))
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
                .get(self.state.tab.wrapping_sub(TABS.len()))
                .map(|p| p.id.clone());
            self.state
                .editor
                .update(self.hwnd, &data.plugins, self.state.font, self.state.dpi);
            if let Some(id) = selected {
                self.state.tab = data
                    .plugins
                    .iter()
                    .position(|p| p.id == id)
                    .map_or(TABS.len() - 1, |i| TABS.len() + i);
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
                self.state.tab = TABS.len() + index;
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
            ultra_bar_native_ui::fonts::delete_object(self.state.plugin_font);
            ultra_bar_native_ui::fonts::delete_object(self.state.heading);
            ultra_bar_native_ui::fonts::delete_object(self.state.icons);
        }
    }
}
const TABS: [&str; 5] = ["常规", "外观", "时间", "托盘", "插件"];
fn choices(command: usize) -> &'static [(usize, &'static str)] {
    match command {
        2 | 3 => &[(3, "仅图标"), (2, "根据空间自动展示")],
        7 | 8 => &[(7, "正常"), (8, "小")],
        17 => &[(50, "纯凹陷"), (51, "底部横条"), (52, "顶部粗点")],
        _ => &[],
    }
}
unsafe fn sync_tips(s: &mut State) {
    unsafe {
        let u = |n| n * s.dpi / 96;
        let mut tips = vec![Tip {
            bounds: (u(646), 0, u(700), u(58)),
            text: "关闭".into(),
        }];
        if s.tab >= TABS.len() && s.editor.has_form() {
            tips.extend(s.editor.tips(s.dpi));
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
        if let Some(rows) = s.data.pages.get(s.tab) {
            for (i, row) in rows.iter().skip(s.scroll).take(6).enumerate() {
                if matches!(row.command, 70 | 71) {
                    let y = 100 + i as i32 * 66;
                    tips.push(Tip {
                        bounds: (u(184), u(y), u(676), u(y + 58)),
                        text: format!("{}\n{}", row.title, row.detail),
                    });
                }
            }
        }
        for (i, plugin) in s.data.plugins.iter().enumerate().skip(s.nav_scroll).take(5) {
            let y = 304 + (i - s.nav_scroll) as i32 * 42;
            tips.push(Tip {
                bounds: (
                    u(12),
                    u(y),
                    u(if s.data.updates.contains_key(&plugin.id) {
                        124
                    } else {
                        160
                    }),
                    u(y + 38),
                ),
                text: plugin.name.clone(),
            });
            if let Some(update) = s.data.updates.get(&plugin.id) {
                tips.push(Tip {
                    bounds: (u(124), u(y + 4), u(160), u(y + 34)),
                    text: format!(
                        "{}\n更新至 {}\n来源：{}",
                        plugin.id, update.version, update.source
                    ),
                });
            }
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
                if s.tab >= TABS.len() {
                    s.editor.scroll(hwnd, ((w >> 16) as i16) < 0, s.dpi);
                    sync_tips(s);
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
                } else if x < 164 && (88..288).contains(&y) {
                    s.tab = ((y - 88) / 40) as usize;
                    s.editor.select(hwnd, None, s.font, s.dpi);
                    s.scroll = 0;
                    InvalidateRect(hwnd, null(), 0);
                } else if x < 164 && (304..514).contains(&y) {
                    let index = s.nav_scroll + ((y - 304) / 42) as usize;
                    if let Some(plugin) = s.data.plugins.get(index) {
                        if (124..160).contains(&x)
                            && (4..34).contains(&((y - 304) % 42))
                            && s.data
                                .updates
                                .get(&plugin.id)
                                .is_some_and(|update| !update.busy)
                        {
                            s.pending_update = Some(plugin.id.clone());
                            PostMessageW(s.owner, message(), 73, 0);
                        } else {
                            s.tab = TABS.len() + index;
                            s.editor
                                .select(hwnd, Some(plugin.id.clone()), s.font, s.dpi);
                        }
                    }
                } else if s.tab >= TABS.len() {
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
                    let options = choices(row.command);
                    if options.is_empty() {
                        PostMessageW(s.owner, message(), row.command, 0);
                    } else if (27..58).contains(&((y - 100) % 66)) && (200..660).contains(&x) {
                        let index =
                            ((x - 200) as usize * options.len() / 460).min(options.len() - 1);
                        PostMessageW(s.owner, message(), options[index].0, 0);
                    }
                }
                sync_tips(s);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == EN_CHANGE => {
                s.editor.changed();
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == CBN_SELCHANGE => {
                s.editor.choice_changed(l as HWND);
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_COMMAND if (w >> 16) as u32 == CBN_DROPDOWN => {
                crate::editors::controls::prepare_combo_popup(l as HWND);
                0
            }
            WM_DRAWITEM if l != 0 => {
                crate::editors::controls::draw_combo_item(
                    &*(l as *const windows_sys::Win32::UI::Controls::DRAWITEMSTRUCT),
                );
                1
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
                    let title = if s.tab < TABS.len() {
                        TABS[s.tab]
                    } else {
                        s.data
                            .plugins
                            .get(s.tab - TABS.len())
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
                                unit(88 + i as i32 * 40),
                                unit(160),
                                unit(126 + i as i32 * 40),
                                unit(12),
                                unit(12),
                            );
                            SelectObject(dc, b);
                            ultra_bar_native_ui::fonts::delete_object(brush);
                        }
                        text(
                            tab,
                            30,
                            88 + i as i32 * 40,
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
                        SelectObject(dc, s.plugin_font);
                        for (i, plugin) in
                            s.data.plugins.iter().enumerate().skip(s.nav_scroll).take(5)
                        {
                            let y = 304 + (i - s.nav_scroll) as i32 * 42;
                            if s.tab == TABS.len() + i {
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
                                if s.data.updates.contains_key(&plugin.id) {
                                    96
                                } else {
                                    132
                                },
                                38,
                                if s.tab == TABS.len() + i {
                                    palette.text
                                } else {
                                    palette.muted
                                },
                            );
                            if let Some(update) = s.data.updates.get(&plugin.id) {
                                let brush = SelectObject(dc, theme::field_brush());
                                RoundRect(
                                    dc,
                                    unit(128),
                                    unit(y + 5),
                                    unit(156),
                                    unit(y + 33),
                                    unit(8),
                                    unit(8),
                                );
                                SelectObject(dc, brush);
                                SelectObject(dc, s.icons);
                                text(
                                    "\u{e896}",
                                    130,
                                    y + 5,
                                    24,
                                    28,
                                    if update.busy {
                                        palette.muted
                                    } else {
                                        palette.active_border
                                    },
                                );
                                SelectObject(dc, s.plugin_font);
                            }
                        }
                        SelectObject(dc, s.font);
                    }
                    if s.tab >= TABS.len() {
                        if let Some(page) = s.data.plugins.get(s.tab - TABS.len())
                            && page.schema.is_none()
                            && let Some(update) = s.data.updates.get(&page.id)
                        {
                            text(
                                &format!("可更新至 {}", update.version),
                                200,
                                110,
                                460,
                                28,
                                palette.text,
                            );
                            text(&page.id, 200, 150, 460, 28, palette.muted);
                            text(
                                &format!("来源：{}", update.source),
                                200,
                                190,
                                460,
                                28,
                                palette.muted,
                            );
                        } else {
                            s.editor.draw(&surface, s.dpi, s.font, s.icons);
                        }
                    }
                    if s.tab == 2
                        && let Some(provider) = &s.data.clock_provider
                    {
                        text(
                            &format!("时间区域由 {provider} 管理"),
                            200,
                            112,
                            460,
                            28,
                            palette.text,
                        );
                        text(
                            "请在左侧对应插件的设置中调整。",
                            200,
                            148,
                            460,
                            28,
                            palette.muted,
                        );
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
                            let options = choices(row.command);
                            if !options.is_empty() {
                                let width = 460 / options.len() as i32;
                                for (index, (_, label)) in options.iter().enumerate() {
                                    let x = 200 + index as i32 * width;
                                    let selected = row.detail == *label;
                                    let ring = CreatePen(
                                        PS_SOLID,
                                        unit(1).max(1),
                                        if selected {
                                            palette.active_border
                                        } else {
                                            palette.muted
                                        },
                                    );
                                    let old_pen = SelectObject(dc, ring);
                                    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                                    Ellipse(dc, unit(x), unit(y + 33), unit(x + 14), unit(y + 47));
                                    if selected {
                                        let dot = CreateSolidBrush(palette.active_border);
                                        SelectObject(dc, dot);
                                        Ellipse(
                                            dc,
                                            unit(x + 4),
                                            unit(y + 37),
                                            unit(x + 10),
                                            unit(y + 43),
                                        );
                                        SelectObject(dc, old_brush);
                                        ultra_bar_native_ui::fonts::delete_object(dot);
                                    } else {
                                        SelectObject(dc, old_brush);
                                    }
                                    SelectObject(dc, old_pen);
                                    ultra_bar_native_ui::fonts::delete_object(ring);
                                    text(
                                        label,
                                        x + 22,
                                        y + 29,
                                        width - 24,
                                        22,
                                        if selected {
                                            palette.text
                                        } else {
                                            palette.muted
                                        },
                                    );
                                }
                            } else {
                                text(&row.detail, 200, y + 29, 420, 20, palette.muted);
                            }
                            if matches!(row.command, 41 | 42 | 72) {
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
                            } else if options.is_empty() {
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
