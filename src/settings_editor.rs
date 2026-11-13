use crate::visual::{Surface, text::draw_text, theme, wide};
use std::{
    collections::{HashMap, VecDeque},
    mem::zeroed,
    ptr::{null, null_mut},
};
use ultra_bar_native_ui::{
    form::{Field, Kind},
    settings_bridge::Schema,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Input::KeyboardAndMouse::EnableWindow, Shell::*, WindowsAndMessaging::*},
};
#[derive(serde::Deserialize)]
pub struct PluginPage {
    pub id: String,
    pub name: String,
    pub schema: Option<Schema>,
}
struct Draft {
    fields: Vec<Field>,
    revision: u64,
    dirty: bool,
    saving: bool,
    status: String,
    scroll: usize,
    pending_action: Option<u64>,
}
#[derive(Default)]
pub struct Editor {
    active: Option<String>,
    drafts: HashMap<String, Draft>,
    latest: HashMap<String, Schema>,
    controls: Vec<HWND>,
    loading: bool,
    pending: VecDeque<(String, u64, Vec<String>)>,
    actions: VecDeque<(String, ultra_bar_native_ui::settings_bridge::SaveRequest)>,
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pages(revision: u64, ack: Option<u64>, error: &str) -> Vec<PluginPage> {
        vec![
            PluginPage {
                id: "com.example.a".into(),
                name: "A".into(),
                schema: Some(Schema {
                    revision,
                    ack,
                    action_result: None,
                    error: error.into(),
                    fields: vec![Field::text("规则", "原值")],
                }),
            },
            PluginPage {
                id: "com.example.b".into(),
                name: "B".into(),
                schema: Some(Schema {
                    revision: 1,
                    ack: None,
                    action_result: None,
                    error: String::new(),
                    fields: vec![Field::text("路径", "B")],
                }),
            },
        ]
    }
    #[test]
    fn native_edits_survive_navigation_and_restart_is_not_mistaken_for_save_ack() {
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                700,
                540,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            assert!(!hwnd.is_null());
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let mut editor = Editor::default();
            editor.update(hwnd, &pages(1, None, ""), font, 96);
            editor.select(hwnd, Some("com.example.a".into()), font, 96);
            let original_control = editor.controls[0];
            editor.update(hwnd, &pages(1, None, ""), font, 96);
            assert_eq!(
                editor.controls[0], original_control,
                "后台状态刷新不应重建正在输入的控件"
            );
            SetWindowTextW(editor.controls[0], wide("未保存的中文正则").as_ptr());
            editor.changed();
            editor.select(hwnd, Some("com.example.b".into()), font, 96);
            editor.select(hwnd, Some("com.example.a".into()), font, 96);
            assert!(editor.click(hwnd, 635, 490, font, font, 96));
            let (id, revision, values) = editor.take_save().unwrap();
            assert_eq!(id, "com.example.a");
            assert_eq!(revision, 1);
            assert_eq!(values[0], "未保存的中文正则");
            editor.update(hwnd, &pages(2, None, ""), font, 96);
            let draft = &editor.drafts["com.example.a"];
            assert!(draft.dirty);
            assert!(!draft.saving);
            assert_eq!(draft.fields[0].value, "未保存的中文正则");
            assert!(editor.click(hwnd, 635, 490, font, font, 96));
            editor.take_save();
            editor.update(hwnd, &pages(3, Some(1), "规则无效"), font, 96);
            assert_eq!(
                editor.drafts["com.example.a"].fields[0].value,
                "未保存的中文正则"
            );
            assert!(editor.has_unsaved());
            assert!(editor.click(hwnd, 635, 490, font, font, 96));
            assert_eq!(editor.take_save().unwrap().1, 3);
            editor.update(hwnd, &pages(4, Some(3), ""), font, 96);
            assert!(!editor.has_unsaved());
            DestroyWindow(hwnd);
        }
    }
    #[test]
    fn plugin_choices_use_native_selection_and_preserve_explicit_values() {
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                700,
                540,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let mut p = pages(1, None, "");
            p[0].schema.as_mut().unwrap().fields = vec![
                Field::choice("每周第一天", 0, &["星期一", "星期日"]),
                Field::choice("展示", 0, &["仅时间", "并排", "轮播"]),
                Field::choice("开启", 1, &["关闭", "开启"]),
            ];
            let mut editor = Editor::default();
            editor.update(hwnd, &p, font, 96);
            editor.select(hwnd, Some("com.example.a".into()), font, 96);
            assert!(!editor.controls[0].is_null() && !editor.controls[1].is_null());
            assert!(editor.controls[2].is_null());
            assert_eq!(SendMessageW(editor.controls[0], CB_GETCOUNT, 0, 0), 2);
            SendMessageW(editor.controls[0], WM_MOUSEWHEEL, 120usize << 16, 0);
            assert_eq!(
                SendMessageW(editor.controls[0], CB_GETCURSEL, 0, 0),
                0,
                "关闭的下拉框不应被滚轮盲切"
            );
            SendMessageW(editor.controls[0], CB_SETCURSEL, 1, 0);
            editor.choice_changed(editor.controls[0]);
            SendMessageW(editor.controls[1], CB_SETCURSEL, 2, 0);
            editor.choice_changed(editor.controls[1]);
            editor.click(hwnd, 635, 490, font, font, 96);
            assert_eq!(editor.take_save().unwrap().2, vec!["1", "2", "1"]);
            DestroyWindow(hwnd);
        }
    }
    #[test]
    fn field_action_tests_the_draft_without_saving_or_erasing_secrets() {
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                700,
                540,
                null_mut(),
                null_mut(),
                null_mut(),
                null(),
            );
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let mut p = pages(1, None, "");
            p[0].schema.as_mut().unwrap().fields = vec![
                Field::text("Host", "old").with_action("test_api", "测试连接", "\u{e768}"),
                Field::secret("Key"),
            ];
            let mut editor = Editor::default();
            editor.update(hwnd, &p, font, 96);
            editor.select(hwnd, Some("com.example.a".into()), font, 96);
            SetWindowTextW(editor.controls[0], wide("draft.qweatherapi.com").as_ptr());
            SetWindowTextW(editor.controls[1], wide("test-secret-memory-only").as_ptr());
            editor.changed();
            assert!(editor.click(hwnd, 645, 136, font, font, 96));
            let (_, request) = editor.take_action().unwrap();
            assert_eq!(request.action.as_deref(), Some("test_api"));
            assert_eq!(
                request.values,
                vec!["draft.qweatherapi.com", "test-secret-memory-only"]
            );
            assert!(editor.take_save().is_none());
            p[0].schema.as_mut().unwrap().action_result =
                Some(ultra_bar_native_ui::settings_bridge::ActionResult {
                    request_id: request.request_id,
                    ok: true,
                    message: "连接成功".into(),
                });
            editor.update(hwnd, &p, font, 96);
            let d = &editor.drafts["com.example.a"];
            assert!(d.dirty && d.pending_action.is_none());
            assert_eq!(d.fields[0].value, "draft.qweatherapi.com");
            assert_eq!(d.fields[1].value, "test-secret-memory-only");
            DestroyWindow(hwnd);
        }
    }
}
impl Editor {
    unsafe fn collect(&mut self) {
        unsafe {
            if let Some(draft) = self.active.as_ref().and_then(|id| self.drafts.get_mut(id)) {
                for (i, control) in self
                    .controls
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| !h.is_null())
                {
                    let mut text =
                        vec![0u16; (GetWindowTextLengthW(*control).max(0) as usize).min(65536) + 1];
                    let n = GetWindowTextW(*control, text.as_mut_ptr(), text.len() as _);
                    if let Some(field) = draft.fields.get_mut(i) {
                        field.value = if matches!(field.kind, Kind::Choice(_)) {
                            SendMessageW(*control, CB_GETCURSEL, 0, 0)
                                .max(0)
                                .to_string()
                        } else {
                            String::from_utf16_lossy(&text[..n.max(0) as usize])
                        };
                    }
                }
            }
        }
    }
    pub unsafe fn select(&mut self, hwnd: HWND, id: Option<String>, font: HFONT, dpi: i32) {
        unsafe {
            self.collect();
            self.active = id;
            self.rebuild(hwnd, font, dpi);
        }
    }
    pub unsafe fn update(&mut self, hwnd: HWND, pages: &[PluginPage], font: HFONT, dpi: i32) {
        unsafe {
            self.collect();
            if self
                .active
                .as_ref()
                .is_none_or(|id| pages.iter().any(|p| &p.id == id))
                && self.latest.len() == pages.iter().filter(|p| p.schema.is_some()).count()
                && pages
                    .iter()
                    .all(|page| match (&page.schema, self.latest.get(&page.id)) {
                        (Some(next), Some(old)) => {
                            next.revision == old.revision
                                && next.ack == old.ack
                                && next.error == old.error
                                && next.action_result == old.action_result
                        }
                        (None, None) => true,
                        _ => false,
                    })
            {
                return;
            }
            self.drafts
                .retain(|id, _| pages.iter().any(|p| &p.id == id));
            self.latest.clear();
            for page in pages {
                if let Some(schema) = &page.schema {
                    self.latest.insert(page.id.clone(), schema.clone());
                    if let Some(draft) = self.drafts.get_mut(&page.id) {
                        if let Some(result) = &schema.action_result
                            && draft.pending_action == Some(result.request_id)
                        {
                            draft.pending_action = None;
                            draft.status = result.message.clone();
                        }
                        let interrupted =
                            draft.revision != schema.revision && draft.pending_action.is_some();
                        if interrupted {
                            draft.pending_action = None;
                        }
                        if draft.revision != schema.revision && (draft.saving || !draft.dirty) {
                            if draft.saving && schema.ack != Some(draft.revision) {
                                draft.saving = false;
                                draft.dirty = true;
                                draft.status = "插件设置已更新，请重新载入后重试".into();
                                continue;
                            }
                            if schema.error.is_empty() {
                                draft.fields = schema.fields.clone();
                                draft.dirty = false;
                                draft.status = if draft.saving {
                                    "已保存".into()
                                } else {
                                    String::new()
                                };
                            } else {
                                draft.status = schema.error.clone();
                            }
                            draft.revision = schema.revision;
                            draft.saving = false;
                        }
                        if interrupted {
                            draft.status = "插件设置已更新，请重新测试".into();
                        }
                    } else {
                        self.drafts.insert(
                            page.id.clone(),
                            Draft {
                                fields: schema.fields.clone(),
                                revision: schema.revision,
                                dirty: false,
                                saving: false,
                                status: schema.error.clone(),
                                scroll: 0,
                                pending_action: None,
                            },
                        );
                    }
                }
            }
            if self
                .active
                .as_ref()
                .is_some_and(|id| !pages.iter().any(|p| &p.id == id))
            {
                self.active = None;
            }
            self.rebuild(hwnd, font, dpi);
        }
    }
    unsafe fn rebuild(&mut self, hwnd: HWND, font: HFONT, dpi: i32) {
        unsafe {
            self.loading = true;
            for control in self.controls.drain(..).filter(|h| !h.is_null()) {
                DestroyWindow(control);
            }
            if let Some(draft) = self.active.as_ref().and_then(|id| self.drafts.get(id)) {
                for field in &draft.fields {
                    let control = if matches!(field.kind, Kind::Text | Kind::Secret) {
                        let h = CreateWindowExW(
                            0,
                            wide("EDIT").as_ptr(),
                            wide(&field.value).as_ptr(),
                            WS_CHILD
                                | WS_TABSTOP
                                | 0x80
                                | if matches!(field.kind, Kind::Secret) {
                                    0x20
                                } else {
                                    0
                                },
                            0,
                            0,
                            1,
                            1,
                            hwnd,
                            null_mut(),
                            null_mut(),
                            null(),
                        );
                        ultra_bar_native_ui::input::attach(h);
                        SendMessageW(h, WM_SETFONT, font as usize, 0);
                        SendMessageW(h, 0x00c5, 65536, 0);
                        h
                    } else if let Kind::Choice(choices) = &field.kind
                        && ultra_bar_native_ui::form::switch_state(field).is_none()
                    {
                        let combo = crate::editors::controls::create(
                            hwnd,
                            "COMBOBOX",
                            "",
                            0,
                            WS_TABSTOP | CBS_DROPDOWNLIST as u32,
                            (0, 0, 460, 26),
                            (font, dpi),
                        );
                        for choice in choices {
                            SendMessageW(combo, CB_ADDSTRING, 0, wide(choice).as_ptr() as isize);
                        }
                        SendMessageW(combo, CB_SETCURSEL, field.value.parse().unwrap_or(0), 0);
                        SendMessageW(
                            combo,
                            CB_SETITEMHEIGHT,
                            usize::MAX,
                            (20 * dpi / 96) as isize,
                        );
                        combo
                    } else {
                        null_mut()
                    };
                    self.controls.push(control);
                }
            }
            self.loading = false;
            self.position(dpi);
            InvalidateRect(hwnd, null(), 0);
        }
    }
    unsafe fn position(&self, dpi: i32) {
        unsafe {
            if let Some(draft) = self.active.as_ref().and_then(|id| self.drafts.get(id)) {
                for (i, h) in self
                    .controls
                    .iter()
                    .enumerate()
                    .filter(|(_, h)| !h.is_null())
                {
                    if (draft.scroll..draft.scroll + 6).contains(&i) {
                        MoveWindow(
                            *h,
                            200 * dpi / 96,
                            (124 + (i - draft.scroll) as i32 * 58) * dpi / 96,
                            (if draft.fields[i].action.is_some() {
                                420
                            } else {
                                460
                            }) * dpi
                                / 96,
                            (if matches!(draft.fields[i].kind, Kind::Choice(_)) {
                                26 + 24 * 8
                            } else {
                                26
                            }) * dpi
                                / 96,
                            1,
                        );
                        EnableWindow(
                            *h,
                            i32::from(!draft.saving && draft.pending_action.is_none()),
                        );
                        ShowWindow(*h, SW_SHOWNA);
                    } else {
                        ShowWindow(*h, SW_HIDE);
                    }
                }
            }
        }
    }
    pub fn changed(&mut self) {
        if !self.loading
            && let Some(d) = self.active.as_ref().and_then(|id| self.drafts.get_mut(id))
        {
            d.dirty = true;
            d.status.clear();
        }
    }
    pub unsafe fn choice_changed(&mut self, control: HWND) {
        unsafe {
            if !self.loading && self.controls.contains(&control) {
                self.collect();
                self.changed();
            }
        }
    }
    pub fn has_unsaved(&self) -> bool {
        self.drafts
            .values()
            .any(|d| d.dirty || d.saving || d.pending_action.is_some())
    }
    pub fn has_form(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|id| self.drafts.contains_key(id))
    }
    pub unsafe fn scroll(&mut self, hwnd: HWND, down: bool, dpi: i32) {
        unsafe {
            if let Some(d) = self.active.as_ref().and_then(|id| self.drafts.get_mut(id)) {
                d.scroll = if down {
                    (d.scroll + 1).min(d.fields.len().saturating_sub(6))
                } else {
                    d.scroll.saturating_sub(1)
                };
                self.position(dpi);
                InvalidateRect(hwnd, null(), 0);
            }
        }
    }
    pub fn take_save(&mut self) -> Option<(String, u64, Vec<String>)> {
        self.pending.pop_front()
    }
    pub fn take_action(
        &mut self,
    ) -> Option<(String, ultra_bar_native_ui::settings_bridge::SaveRequest)> {
        self.actions.pop_front()
    }
    pub fn error(&mut self, id: &str, error: String) {
        if let Some(d) = self.drafts.get_mut(id) {
            d.saving = false;
            d.pending_action = None;
            d.status = error;
        }
    }
    pub unsafe fn click(
        &mut self,
        hwnd: HWND,
        x: i32,
        y: i32,
        font: HFONT,
        icons: HFONT,
        dpi: i32,
    ) -> bool {
        unsafe {
            let _ = icons;
            if x < 184 {
                return false;
            }
            self.collect();
            let Some(id) = self.active.clone() else {
                return false;
            };
            if y >= 478 && (545..590).contains(&x) {
                if self.drafts.get(&id).is_some_and(|d| d.saving) {
                    return false;
                }
                if let Some(schema) = self.latest.get(&id) {
                    self.drafts.insert(
                        id.clone(),
                        Draft {
                            fields: schema.fields.clone(),
                            revision: schema.revision,
                            dirty: false,
                            saving: false,
                            status: String::new(),
                            scroll: 0,
                            pending_action: None,
                        },
                    );
                    self.rebuild(hwnd, font, dpi);
                }
                return false;
            }
            let Some(d) = self.drafts.get_mut(&id) else {
                return false;
            };
            if !d.saving
                && d.pending_action.is_none()
                && (100..448).contains(&y)
                && (23..49).contains(&((y - 100) % 58))
                && (628..668).contains(&x)
            {
                let index = d.scroll + ((y - 100) / 58) as usize;
                if let Some(action) = d.fields.get(index).and_then(|f| f.action.as_ref()) {
                    let request_id = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64;
                    self.actions.push_back((
                        id.clone(),
                        ultra_bar_native_ui::settings_bridge::SaveRequest {
                            revision: d.revision,
                            values: d.fields.iter().map(|f| f.value.clone()).collect(),
                            action: Some(action.id.clone()),
                            request_id,
                        },
                    ));
                    d.pending_action = Some(request_id);
                    d.status = "正在测试连接…".into();
                    for control in self.controls.iter().filter(|h| !h.is_null()) {
                        EnableWindow(*control, 0);
                    }
                    InvalidateRect(hwnd, null(), 0);
                    return true;
                }
            }
            if y >= 478 && x >= 610 && !d.saving && d.pending_action.is_none() {
                self.pending.push_back((
                    id,
                    d.revision,
                    d.fields.iter().map(|f| f.value.clone()).collect(),
                ));
                d.saving = true;
                d.status = "保存中…".into();
                for h in self.controls.iter().filter(|h| !h.is_null()) {
                    EnableWindow(*h, 0);
                }
                InvalidateRect(hwnd, null(), 0);
                return true;
            }
            if !d.saving
                && d.pending_action.is_none()
                && (100..448).contains(&y)
                && let Some(field) = d.fields.get_mut(d.scroll + ((y - 100) / 58) as usize)
            {
                match &field.kind {
                    Kind::Choice(_) => {
                        if ultra_bar_native_ui::form::switch_state(field).is_some() {
                            field.value = if field.value == "1" { "0" } else { "1" }.into();
                            d.dirty = true;
                            d.status.clear();
                        } else if let Some(control) =
                            self.controls.get(d.scroll + ((y - 100) / 58) as usize)
                        {
                            SendMessageW(*control, CB_SHOWDROPDOWN, 1, 0);
                        }
                    }
                    Kind::Link if field.value.starts_with("https://") => {
                        ShellExecuteW(
                            hwnd,
                            wide("open").as_ptr(),
                            wide(&field.value).as_ptr(),
                            null(),
                            null(),
                            SW_SHOWNORMAL,
                        );
                    }
                    _ => {}
                }
            }
            InvalidateRect(hwnd, null(), 0);
            false
        }
    }
    pub fn tips(&self, dpi: i32) -> Vec<crate::visual::Tip> {
        let u = |n| n * dpi / 96;
        let Some(d) = self.active.as_ref().and_then(|id| self.drafts.get(id)) else {
            return vec![];
        };
        let mut tips = Vec::new();
        for (row, field) in d.fields.iter().skip(d.scroll).take(6).enumerate() {
            if let Some(action) = &field.action {
                let y = 100 + row as i32 * 58;
                tips.push(crate::visual::Tip {
                    bounds: (u(628), u(y + 23), u(668), u(y + 49)),
                    text: action.tooltip.clone(),
                });
            }
        }
        if !d.status.is_empty() {
            tips.push(crate::visual::Tip {
                bounds: (u(194), u(478), u(535), u(522)),
                text: d.status.clone(),
            });
        }
        tips
    }
    pub unsafe fn draw(&self, surface: &Surface, dpi: i32, font: HFONT, icons: HFONT) {
        unsafe {
            let dc = surface.dc;
            let u = |n| n * dpi / 96;
            let p = theme::palette();
            SelectObject(dc, font);
            let text = |value: &str, x, y, width, height, color| {
                SetTextColor(dc, color);
                let mut r = RECT {
                    left: u(x),
                    top: u(y),
                    right: u(x + width),
                    bottom: u(y + height),
                };
                draw_text(
                    dc,
                    wide(value).as_ptr(),
                    -1,
                    &mut r,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
            };
            let Some(d) = self.active.as_ref().and_then(|id| self.drafts.get(id)) else {
                text("插件尚未提供设置", 200, 104, 460, 36, p.muted);
                return;
            };
            for (i, field) in d.fields.iter().skip(d.scroll).take(6).enumerate() {
                let y = 100 + i as i32 * 58;
                let brush = SelectObject(dc, theme::field_brush());
                let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                RoundRect(dc, u(184), u(y), u(676), u(y + 52), u(10), u(10));
                SelectObject(dc, brush);
                SelectObject(dc, pen);
                if let Some(checked) = ultra_bar_native_ui::form::switch_state(field) {
                    text(&field.label, 200, y + 10, 400, 32, p.text);
                    ultra_bar_native_ui::form::draw_switch(
                        dc,
                        RECT {
                            left: u(618),
                            top: u(y + 14),
                            right: u(660),
                            bottom: u(y + 38),
                        },
                        checked,
                        !d.saving,
                    );
                    continue;
                }
                text(&field.label, 200, y + 2, 460, 20, p.muted);
                if let Some(action) = &field.action {
                    SelectObject(dc, icons);
                    text(
                        &action.icon,
                        632,
                        y + 23,
                        28,
                        26,
                        if d.pending_action.is_some() {
                            p.muted
                        } else {
                            p.text
                        },
                    );
                    SelectObject(dc, font);
                }
                match &field.kind {
                    Kind::Choice(_) => {}
                    Kind::Link => text("打开配置指南 ↗", 200, y + 23, 460, 26, p.text),
                    _ => {}
                }
            }
            text(
                if !d.status.is_empty() {
                    &d.status
                } else if d.dirty {
                    "未保存"
                } else {
                    ""
                },
                194,
                480,
                340,
                40,
                p.muted,
            );
            if d.fields.len() > 6 {
                text(
                    &format!(
                        "{}–{} / {}",
                        d.scroll + 1,
                        (d.scroll + 6).min(d.fields.len()),
                        d.fields.len()
                    ),
                    570,
                    452,
                    100,
                    22,
                    p.muted,
                );
            }
            SelectObject(dc, icons);
            text("\u{e72c}", 555, 486, 30, 30, p.muted);
            text("\u{e74e}", 630, 486, 30, 30, p.text);
            SelectObject(dc, font);
        }
    }
    pub unsafe fn capture_controls(&self, dc: HDC) {
        unsafe {
            for h in self.controls.iter().filter(|h| {
                !h.is_null() && GetWindowLongPtrW(**h, GWL_STYLE) as u32 & WS_VISIBLE != 0
            }) {
                let mut r: RECT = zeroed();
                GetWindowRect(*h, &mut r);
                let mut p = POINT {
                    x: r.left,
                    y: r.top,
                };
                ScreenToClient(GetParent(*h), &mut p);
                let saved = SaveDC(dc);
                SetViewportOrgEx(dc, p.x, p.y, null_mut());
                SendMessageW(
                    *h,
                    WM_PRINTCLIENT,
                    dc as usize,
                    (PRF_CLIENT | PRF_ERASEBKGND) as isize,
                );
                RestoreDC(dc, saved);
            }
        }
    }
}
