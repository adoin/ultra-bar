//! Event-driven native statistics dashboard shared by agent providers.
use chrono::Datelike;
use std::collections::{BTreeMap, HashSet};
use ultra_bar_native_ui::{Surface, text::draw_text, theme, wide};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    pub total: u64,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
}
impl Tokens {
    pub fn add(&mut self, other: &Self) {
        self.total = self.total.saturating_add(other.total);
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Bucket {
    pub day: i32,
    pub date: String,
    pub project: String,
    pub model: String,
    pub session: String,
    pub tokens: Tokens,
    pub calls: u64,
}
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ResetCredit {
    pub title: String,
    pub status: String,
    pub granted_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub expiry_known: bool,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub quota: String,
    #[serde(default)]
    pub resets: Option<u64>,
    #[serde(default)]
    pub reset_credits: Option<Vec<ResetCredit>>,
    pub source: String,
    pub available: bool,
    pub breakdown: bool,
    pub buckets: Vec<Bucket>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversations: Option<Conversations>,
    #[serde(default)]
    pub mode: StatisticsMode,
}
#[derive(Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatisticsMode {
    #[default]
    Tokens,
    Conversations,
    Blank,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConversationDay {
    pub day: i32,
    pub date: String,
    pub session: String,
    pub sent: u64,
    pub received: u64,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Conversations {
    pub rows: Vec<ConversationDay>,
    pub partial: bool,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Dataset {
    pub today: i32,
    pub agents: Vec<Agent>,
    pub loading: bool,
}
#[derive(Default)]
pub struct View {
    pub data: Dataset,
    agent: usize,
    tab: usize,
    days: i32,
    scroll: usize,
    reset_details: bool,
}
#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn conversation_views_offer_only_relevant_tabs_and_missing_sources_stay_blank() {
        let mut view = View::default();
        view.replace(Dataset {
            agents: vec![Agent {
                mode: StatisticsMode::Conversations,
                conversations: Some(Conversations::default()),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(view.tabs(), ["概览", "最近几天"]);
        view.click(130, 74);
        assert_eq!(view.tab, 1);
        view.click(330, 74);
        assert_eq!(view.tab, 1);
        view.replace(Dataset {
            agents: vec![Agent {
                mode: StatisticsMode::Blank,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(view.tabs().is_empty());
        assert!(!view.click(130, 74));
        assert_eq!(view.tab, 0);
    }
    #[test]
    fn reset_details_open_from_count_and_return_without_switching_agents() {
        let mut view = View::default();
        view.replace(Dataset {
            agents: vec![Agent {
                id: "Codex".into(),
                resets: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(view.click(580, 170));
        assert!(view.reset_details);
        assert!(view.click(70, 138));
        assert!(!view.reset_details);
        view.click(580, 170);
        view.click(130, 74);
        assert!(!view.reset_details);
        assert_eq!(view.tab, 1);
    }
    #[test]
    fn navigation_rows_stay_clickable_after_removing_the_title_bar() {
        let mut view = View::default();
        view.replace(Dataset {
            agents: vec![
                Agent {
                    id: "a".into(),
                    ..Default::default()
                },
                Agent {
                    id: "b".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        assert!(!view.draggable(32, 24));
        assert!(view.draggable(400, 24));
        assert!(view.click(168, 24));
        assert_eq!(view.agent, 1);
        assert!(view.click(320, 74));
        assert_eq!(view.tab, 3);
        assert!(view.click(620, 74));
        assert_eq!(view.days, 14);
    }
}
pub fn number(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1e9)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1e6)
    } else if value >= 1000 {
        format!("{:.1}K", value as f64 / 1000.)
    } else {
        value.to_string()
    }
}
pub fn smoke(directory: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 11)
        .unwrap()
        .num_days_from_ce();
    let mut agents = vec![];
    for name in ["Codex", "Claude", "Grok Bot"] {
        let mut agent = Agent {
            id: name.into(),
            name: name.into(),
            quota: if name == "Grok Bot" {
                "93.3%".into()
            } else {
                "25%".into()
            },
            resets: (name == "Codex").then_some(2),
            reset_credits: (name == "Codex").then(|| {
                vec![
                    ResetCredit {
                        title: "额度重置".into(),
                        status: "available".into(),
                        expires_at: Some(1795003200),
                        expiry_known: true,
                        ..Default::default()
                    },
                    ResetCredit {
                        title: "额度重置".into(),
                        status: "available".into(),
                        expires_at: None,
                        expiry_known: true,
                        ..Default::default()
                    },
                ]
            }),
            available: name != "Grok Bot",
            breakdown: name != "Grok Bot",
            source: "本地使用记录 · 最近 30 天".into(),
            ..Default::default()
        };
        if agent.available {
            for n in 0..14 {
                agent.buckets.push(Bucket {
                    day: today - n,
                    date: chrono::NaiveDate::from_num_days_from_ce_opt(today - n)
                        .unwrap()
                        .to_string(),
                    project: format!(
                        "D:\\workspace\\{}",
                        if n % 2 == 0 {
                            "ultra-bar"
                        } else {
                            "Schedule-Manager"
                        }
                    ),
                    model: "gpt-6.1-sol".into(),
                    session: format!("session-{n}"),
                    tokens: Tokens {
                        input: 1_200_000 + n as u64 * 20_000,
                        cached: 700_000,
                        output: 180_000,
                        reasoning: 90_000,
                        total: 1_380_000 + n as u64 * 20_000,
                    },
                    calls: 12 + n as u64,
                });
            }
        }
        agents.push(agent);
    }
    let dataset = std::fs::read(directory.join("statistics-data.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Dataset>(&b).ok())
        .unwrap_or(Dataset {
            today,
            agents,
            loading: false,
        });
    unsafe {
        for dpi in [96, 144] {
            let font = ultra_bar_native_ui::fonts::create_font(
                -14 * dpi / 96,
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
                wide("Segoe UI").as_ptr(),
            );
            let icons = ultra_bar_native_ui::fonts::create_font(
                -18 * dpi / 96,
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
                wide("Segoe MDL2 Assets").as_ptr(),
            );
            let mut view = View::default();
            view.replace(dataset.clone());
            for agent in 0..dataset.agents.len().min(8) {
                view.agent = agent;
                for tab in 0..6 {
                    view.tab = if tab == 5 { 0 } else { tab };
                    view.reset_details = tab == 5;
                    let bounds = RECT {
                        left: 0,
                        top: 0,
                        right: 680 * dpi / 96,
                        bottom: 620 * dpi / 96,
                    };
                    if let Some(surface) = Surface::new(bounds.right, bounds.bottom) {
                        view.paint(&surface, font, icons, dpi, &bounds);
                        surface.save_bmp(
                            &directory.join(format!("statistics-{dpi}-{agent}-{tab}.bmp")),
                        )?;
                    }
                }
            }
            ultra_bar_native_ui::fonts::delete_object(font);
            ultra_bar_native_ui::fonts::delete_object(icons);
        }
    }
    Ok(())
}
impl View {
    fn tabs(&self) -> &'static [&'static str] {
        match self
            .data
            .agents
            .get(self.agent)
            .map(|a| a.mode)
            .unwrap_or_default()
        {
            StatisticsMode::Tokens => &["概览", "项目", "最近几天", "Token 构成", "模型"],
            StatisticsMode::Conversations => &["概览", "最近几天"],
            StatisticsMode::Blank => &[],
        }
    }
    pub fn replace(&mut self, data: Dataset) {
        let id = self.data.agents.get(self.agent).map(|a| a.id.clone());
        self.agent = id
            .and_then(|id| data.agents.iter().position(|a| a.id == id))
            .unwrap_or(0);
        self.data = data;
        self.tab = self.tab.min(self.tabs().len().saturating_sub(1));
        if self.days == 0 {
            self.days = 7;
        }
    }
    pub fn click(&mut self, x: i32, y: i32) -> bool {
        if (12..46).contains(&y) && self.data.agents.len() > 1 {
            let i = ((x - 24) / 132).max(0) as usize;
            if x >= 24 && i < self.data.agents.len() {
                self.agent = i;
                self.tab = 0;
                self.reset_details = false;
                self.scroll = 0;
                return true;
            }
        }
        if self.tab == 0
            && self
                .data
                .agents
                .get(self.agent)
                .is_some_and(|a| a.resets.is_some())
        {
            if self.reset_details && (24..160).contains(&x) && (120..158).contains(&y) {
                self.reset_details = false;
                self.scroll = 0;
                return true;
            }
            let at = 120 - self.scroll as i32 * 32;
            if !self.reset_details && (532..660).contains(&x) && (at.max(120)..at + 88).contains(&y)
            {
                self.reset_details = true;
                self.scroll = 0;
                return true;
            }
        }
        let top = 60;
        if (top..top + 34).contains(&y) {
            if self.tabs().is_empty() {
                return false;
            }
            if x >= 506 {
                self.days = match self.days {
                    7 => 14,
                    14 => 30,
                    _ => 7,
                };
            } else if x >= 24 {
                let i = ((x - 24) / 92) as usize;
                if i < self.tabs().len() {
                    self.tab = i;
                    self.reset_details = false;
                }
            }
            self.scroll = 0;
            return true;
        }
        false
    }
    pub fn wheel(&mut self, down: bool) {
        self.scroll = if down {
            self.scroll.saturating_add(1).min(4096)
        } else {
            self.scroll.saturating_sub(1)
        };
    }
    pub fn draggable(&self, x: i32, y: i32) -> bool {
        y < 54
            && ((0..16).contains(&x) || (x >= 24 + self.data.agents.len() as i32 * 132 && x < 590))
    }
    /// # Safety
    /// Surface and fonts belong to the current UI thread.
    pub unsafe fn paint(
        &mut self,
        surface: &Surface,
        font: HFONT,
        icons: HFONT,
        dpi: i32,
        bounds: &RECT,
    ) {
        unsafe {
            let unit = |n: i32| n * dpi / 96;
            let dc = surface.dc;
            let p = theme::palette();
            let height = bounds.bottom * 96 / dpi;
            let bg = theme::colorref(p.background);
            FillRect(dc, bounds, theme::background_brush());
            surface.effect(
                0,
                0,
                bounds.right,
                bounds.bottom,
                &vec![0xee000000 | bg; (bounds.right * bounds.bottom) as usize],
            );
            let create = |size, weight| {
                ultra_bar_native_ui::fonts::create_font(
                    -unit(size),
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
                    wide("Segoe UI").as_ptr(),
                )
            };
            let small = create(11, 400);
            let medium = create(15, 600);
            let value_font = create(27, 600);
            let text = |value: &str, x, y, w, h, color, face, align| {
                let old = SelectObject(dc, face);
                SetTextColor(dc, color);
                SetBkMode(dc, TRANSPARENT as _);
                let mut r = RECT {
                    left: unit(x),
                    top: unit(y),
                    right: unit(x + w),
                    bottom: unit(y + h),
                };
                draw_text(
                    dc,
                    wide(value).as_ptr(),
                    -1,
                    &mut r,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX | align,
                );
                SelectObject(dc, old);
            };
            let blend = |a: u32, b: u32, alpha: f64| {
                theme::colorref(theme::blend(theme::colorref(a), theme::colorref(b), alpha))
            };
            let faint = blend(p.muted, p.background, 0.15);
            let track = blend(p.field, p.background, 0.48);
            let round = |x, y, w, h, r, color| {
                if w <= 0 || h <= 0 {
                    return;
                }
                let brush = CreateSolidBrush(color);
                let old = SelectObject(dc, brush);
                let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                RoundRect(
                    dc,
                    unit(x),
                    unit(y),
                    unit(x + w),
                    unit(y + h),
                    unit(r * 2),
                    unit(r * 2),
                );
                SelectObject(dc, old);
                SelectObject(dc, pen);
                ultra_bar_native_ui::fonts::delete_object(brush);
            };
            let line = |x, y, w, color| {
                let brush = CreateSolidBrush(color);
                FillRect(
                    dc,
                    &RECT {
                        left: unit(x),
                        top: unit(y),
                        right: unit(x + w),
                        bottom: unit(y) + 1,
                    },
                    brush,
                );
                ultra_bar_native_ui::fonts::delete_object(brush);
            };
            // Agent navigation replaces the former title bar. Only compact tools remain on the right.
            for (i, agent) in self.data.agents.iter().enumerate() {
                let x = 24 + i as i32 * 132;
                if self.data.agents.len() > 1 && self.agent == i {
                    round(
                        x - 8,
                        12,
                        124,
                        34,
                        10,
                        blend(p.selection, p.background, 0.55),
                    );
                }
                text(
                    &agent.name,
                    x + 3,
                    12,
                    108,
                    34,
                    if self.agent == i { p.text } else { p.muted },
                    if self.agent == i { medium } else { font },
                    0,
                );
            }
            text("\u{e72c}", 602, 14, 24, 28, p.muted, icons, DT_CENTER);
            text("\u{e711}", 644, 14, 20, 28, p.muted, icons, DT_CENTER);
            line(24, 54, 632, faint);
            for (i, label) in self.tabs().iter().enumerate() {
                let x = 24 + i as i32 * 92;
                text(
                    label,
                    x,
                    64,
                    84,
                    28,
                    if self.tab == i { p.text } else { p.muted },
                    font,
                    0,
                );
                if self.tab == i {
                    round(
                        x,
                        99,
                        if *label == "Token 构成" {
                            68
                        } else if label.chars().count() >= 4 {
                            56
                        } else {
                            28
                        },
                        2,
                        1,
                        p.active_border,
                    );
                }
            }
            if !self.tabs().is_empty() {
                text(
                    &format!("最近 {} 天  ›", self.days),
                    512,
                    64,
                    144,
                    28,
                    p.muted,
                    font,
                    DT_RIGHT,
                );
            }
            let content_top = 120;
            if matches!(self.tab, 0 | 3) && !self.reset_details {
                self.scroll = self
                    .scroll
                    .min(((content_top + 440 - (height - 44)).max(0) + 31) as usize / 32);
            }
            let y = content_top
                - if matches!(self.tab, 0 | 3) && !self.reset_details {
                    self.scroll as i32 * 32
                } else {
                    0
                };
            let saved = SaveDC(dc);
            IntersectClipRect(
                dc,
                unit(18),
                unit(content_top),
                bounds.right - unit(18),
                bounds.bottom - unit(46),
            );
            if let Some(agent) = self.data.agents.get(self.agent) {
                if agent.mode != StatisticsMode::Tokens {
                    if let Some(data) = &agent.conversations
                        && agent.mode == StatisticsMode::Conversations
                    {
                        let rows: Vec<_> = data
                            .rows
                            .iter()
                            .filter(|r| {
                                r.day > self.data.today - self.days && r.day <= self.data.today
                            })
                            .collect();
                        let mut daily = BTreeMap::<i32, (u64, u64)>::new();
                        let mut sessions = HashSet::new();
                        let (mut sent, mut received) = (0, 0);
                        for row in &rows {
                            sent += row.sent;
                            received += row.received;
                            sessions.insert(&row.session);
                            let day = daily.entry(row.day).or_default();
                            day.0 += row.sent;
                            day.1 += row.received;
                        }
                        if self.tab == 0 {
                            self.scroll = 0;
                            round(24, 120, 632, 88, 16, blend(p.field, p.background, 0.30));
                            for (i, (name, value)) in [
                                ("会话", sessions.len() as u64),
                                ("发送消息", sent),
                                ("收到回复", received),
                                ("活跃天数", daily.len() as u64),
                            ]
                            .iter()
                            .enumerate()
                            {
                                let x = 40 + i as i32 * 156;
                                text(name, x, 132, 136, 22, p.muted, small, 0);
                                text(&value.to_string(), x, 158, 136, 34, p.text, value_font, 0);
                            }
                            text("每日消息", 24, 232, 200, 26, p.text, medium, 0);
                            let max =
                                daily.values().map(|(a, b)| a + b).max().unwrap_or(0).max(1) as f64;
                            let peak = (max / 3.).ceil().max(1.) * 3.;
                            let plot_height = (height - 390).clamp(80, 180);
                            for n in 0..=3 {
                                let gy = 276 + plot_height - plot_height * n / 3;
                                line(78, gy, 578, faint);
                                text(
                                    &format!("{:.0}", peak * n as f64 / 3.),
                                    24,
                                    gy - 10,
                                    42,
                                    20,
                                    p.disabled,
                                    small,
                                    DT_RIGHT,
                                );
                            }
                            let pitch = 578. / self.days as f64;
                            let width = (pitch * 0.34).round().clamp(4., 18.) as i32;
                            for n in 0..self.days {
                                let day = self.data.today - self.days + 1 + n;
                                let (out, incoming) = daily.get(&day).copied().unwrap_or_default();
                                let x = 78 + ((n as f64 + 0.5) * pitch).round() as i32;
                                let out_h = (out as f64 / peak * plot_height as f64).round() as i32;
                                let in_h =
                                    (incoming as f64 / peak * plot_height as f64).round() as i32;
                                if out_h > 0 {
                                    round(
                                        x - width / 2,
                                        276 + plot_height - out_h,
                                        width,
                                        out_h,
                                        2,
                                        p.active_border,
                                    );
                                }
                                if in_h > 0 {
                                    round(
                                        x - width / 2,
                                        276 + plot_height - out_h - in_h,
                                        width,
                                        in_h,
                                        2,
                                        blend(p.active_border, p.text, 0.45),
                                    );
                                }
                                if (self.days <= 7 || n % 7 == 0 || n == self.days - 1)
                                    && let Some(date) =
                                        chrono::NaiveDate::from_num_days_from_ce_opt(day)
                                {
                                    text(
                                        &date.format("%m-%d").to_string(),
                                        x - 30,
                                        284 + plot_height,
                                        60,
                                        20,
                                        p.disabled,
                                        small,
                                        DT_CENTER,
                                    );
                                }
                            }
                            round(78, 318 + plot_height, 6, 6, 2, p.active_border);
                            text(
                                "发送消息",
                                94,
                                310 + plot_height,
                                120,
                                22,
                                p.muted,
                                small,
                                0,
                            );
                            round(
                                256,
                                318 + plot_height,
                                6,
                                6,
                                2,
                                blend(p.active_border, p.text, 0.45),
                            );
                            text(
                                "收到回复",
                                272,
                                310 + plot_height,
                                120,
                                22,
                                p.muted,
                                small,
                                0,
                            );
                        } else {
                            for (name, x) in [("日期", 24), ("发送消息", 270), ("收到回复", 470)]
                            {
                                text(name, x, 120, 180, 24, p.muted, small, 0);
                            }
                            let visible = ((height - 220) / 36).max(1) as usize;
                            self.scroll = self
                                .scroll
                                .min((self.days as usize).saturating_sub(visible));
                            for n in (0..self.days as usize).skip(self.scroll).take(visible) {
                                let day = self.data.today - n as i32;
                                let (out, incoming) = daily.get(&day).copied().unwrap_or_default();
                                let at = 156 + (n - self.scroll) as i32 * 36;
                                if let Some(date) =
                                    chrono::NaiveDate::from_num_days_from_ce_opt(day)
                                {
                                    text(&date.to_string(), 24, at, 220, 28, p.text, font, 0);
                                }
                                text(&out.to_string(), 270, at, 180, 28, p.active_border, font, 0);
                                text(&incoming.to_string(), 470, at, 180, 28, p.text, font, 0);
                                line(24, at + 32, 632, faint);
                            }
                        }
                        RestoreDC(dc, saved);
                        text(
                            &agent.source,
                            24,
                            height - 34,
                            632,
                            22,
                            p.disabled,
                            small,
                            0,
                        );
                    } else {
                        RestoreDC(dc, saved);
                    }
                    ultra_bar_native_ui::fonts::delete_object(small);
                    ultra_bar_native_ui::fonts::delete_object(medium);
                    ultra_bar_native_ui::fonts::delete_object(value_font);
                    return;
                }
                let rows: Vec<_> = agent
                    .buckets
                    .iter()
                    .filter(|b| b.day > self.data.today - self.days && b.day <= self.data.today)
                    .collect();
                let mut total = Tokens::default();
                let mut calls = 0;
                let mut sessions = HashSet::new();
                for row in &rows {
                    total.add(&row.tokens);
                    calls += row.calls;
                    sessions.insert(&row.session);
                }
                let colors = [
                    p.active_border,
                    theme::colorref(
                        theme::Hsl {
                            h: theme::Hsl::from_rgb(theme::accent_rgb()).h + 42.,
                            s: 0.58,
                            l: 0.66,
                        }
                        .rgb(),
                    ),
                    theme::colorref(
                        theme::Hsl {
                            h: theme::Hsl::from_rgb(theme::accent_rgb()).h + 160.,
                            s: 0.55,
                            l: 0.66,
                        }
                        .rgb(),
                    ),
                ];
                let composition = |at: i32| {
                    let parts = [
                        total.input.saturating_sub(total.cached),
                        total.cached.min(total.input),
                        total.output,
                    ];
                    let sum = parts.iter().sum::<u64>();
                    round(24, at, 632, 8, 4, track);
                    let clip = CreateRoundRectRgn(
                        unit(24),
                        unit(at),
                        unit(656),
                        unit(at + 8),
                        unit(8),
                        unit(8),
                    );
                    let saved = SaveDC(dc);
                    ExtSelectClipRgn(dc, clip, RGN_AND);
                    ultra_bar_native_ui::fonts::delete_object(clip);
                    let mut used = 0u64;
                    for (i, part) in parts.iter().enumerate() {
                        let left = 24
                            + if sum > 0 {
                                (used as f64 / sum as f64 * 632.).round() as i32
                            } else {
                                0
                            };
                        used += part;
                        let right = 24
                            + if sum > 0 {
                                (used as f64 / sum as f64 * 632.).round() as i32
                            } else {
                                0
                            };
                        if right > left {
                            let brush = CreateSolidBrush(colors[i]);
                            FillRect(
                                dc,
                                &RECT {
                                    left: unit(left),
                                    top: unit(at),
                                    right: unit(right),
                                    bottom: unit(at + 8),
                                },
                                brush,
                            );
                            ultra_bar_native_ui::fonts::delete_object(brush);
                        }
                    }
                    RestoreDC(dc, saved);
                    for (i, (name, part)) in ["未缓存输入", "缓存输入", "输出"]
                        .iter()
                        .zip(parts)
                        .enumerate()
                    {
                        let x = 24 + i as i32 * 214;
                        round(x, at + 28, 5, 5, 2, colors[i]);
                        text(name, x + 13, at + 20, 120, 22, p.muted, small, 0);
                        text(&number(part), x, at + 45, 132, 27, p.text, medium, 0);
                        text(
                            &format!(
                                "{:.1}%",
                                if sum > 0 {
                                    part as f64 * 100. / sum as f64
                                } else {
                                    0.
                                }
                            ),
                            x + 124,
                            at + 45,
                            72,
                            27,
                            p.muted,
                            small,
                            DT_RIGHT,
                        );
                    }
                };
                let graph = |at: i32, h: i32| {
                    let mut days = BTreeMap::<i32, u64>::new();
                    for row in &rows {
                        *days.entry(row.day).or_default() += row.tokens.total;
                    }
                    let peak = days.values().copied().max().unwrap_or(0).max(1) as f64;
                    let rough = peak / 3.;
                    let power = 10f64.powf(rough.log10().floor());
                    let step = [1., 2., 5., 10.]
                        .into_iter()
                        .map(|v| v * power)
                        .find(|v| *v >= rough)
                        .unwrap_or(power * 10.);
                    let count = (peak / step).ceil().max(1.) as i32;
                    let maximum = step * count as f64;
                    let plot_left = 78;
                    let plot_width = 578;
                    let plot_height = h - 34;
                    for n in 0..=count {
                        let gy = at + plot_height
                            - (n as f64 / count as f64 * plot_height as f64).round() as i32;
                        line(plot_left, gy, plot_width, faint);
                        let value = (step * n as f64) as u64;
                        let label = if value >= 1_000_000_000 {
                            format!("{:.1}B", value as f64 / 1e9)
                        } else if value >= 1_000_000 {
                            format!("{:.0}M", value as f64 / 1e6)
                        } else if value >= 1000 {
                            format!("{:.0}K", value as f64 / 1e3)
                        } else {
                            value.to_string()
                        };
                        text(&label, 24, gy - 10, 42, 20, p.disabled, small, DT_RIGHT);
                    }
                    let pitch = plot_width as f64 / self.days as f64;
                    let bar_width = (pitch * 0.32).round().clamp(4., 18.) as i32;
                    for n in 0..self.days {
                        let day = self.data.today - self.days + 1 + n;
                        let value = days.get(&day).copied().unwrap_or(0);
                        let bh = (value as f64 / maximum * plot_height as f64).round() as i32;
                        let center = plot_left + ((n as f64 + 0.5) * pitch).round() as i32;
                        if bh > 0 {
                            round(
                                center - bar_width / 2,
                                at + plot_height - bh,
                                bar_width,
                                bh,
                                3,
                                p.active_border,
                            );
                        } else {
                            round(
                                center - 2,
                                at + plot_height - 2,
                                4,
                                2,
                                1,
                                blend(p.muted, p.background, 0.4),
                            );
                        }
                        if self.days <= 7 && value > 0 {
                            text(
                                &number(value),
                                center - 34,
                                at + plot_height - bh - 24,
                                68,
                                20,
                                p.muted,
                                small,
                                DT_CENTER,
                            );
                        }
                        if self.days <= 14 || n % 5 == 0 || n == self.days - 1 {
                            let label = chrono::NaiveDate::from_num_days_from_ce_opt(day)
                                .map(|d| d.format("%m-%d").to_string())
                                .unwrap_or_default();
                            text(
                                &label,
                                center - 28,
                                at + plot_height + 10,
                                56,
                                20,
                                p.muted,
                                small,
                                DT_CENTER,
                            );
                        }
                    }
                };
                if self.reset_details {
                    text("‹  概览", 24, 120, 132, 36, p.active_border, font, 0);
                    text(
                        &format!("可用重置 {} 次", agent.resets.unwrap_or(0)),
                        180,
                        120,
                        470,
                        36,
                        p.text,
                        medium,
                        DT_RIGHT,
                    );
                    let credits = agent.reset_credits.as_deref();
                    match credits {
                        None => text(
                            "当前仅返回可用次数，暂未提供过期明细",
                            24,
                            188,
                            620,
                            30,
                            p.muted,
                            font,
                            0,
                        ),
                        Some([]) => text(
                            if agent.resets == Some(0) {
                                "暂无可用重置"
                            } else {
                                "暂未返回可用重置明细"
                            },
                            24,
                            188,
                            620,
                            30,
                            p.muted,
                            font,
                            0,
                        ),
                        Some(credits) => {
                            let visible = ((height - 250) / 78).max(1) as usize;
                            self.scroll = self.scroll.min(credits.len().saturating_sub(visible));
                            if credits.len() as u64 != agent.resets.unwrap_or(0) {
                                text(
                                    &format!(
                                        "已返回 {} 条明细 · 共 {} 次可用重置",
                                        credits.len(),
                                        agent.resets.unwrap_or(0)
                                    ),
                                    24,
                                    166,
                                    620,
                                    24,
                                    p.muted,
                                    small,
                                    0,
                                );
                            }
                            for (i, credit) in
                                credits.iter().skip(self.scroll).take(visible).enumerate()
                            {
                                let at = 204 + i as i32 * 78;
                                round(20, at, 640, 68, 10, blend(p.field, p.background, 0.30));
                                text(&credit.title, 36, at + 8, 286, 24, p.text, font, 0);
                                let granted = credit
                                    .granted_at
                                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                                    .map(|at| {
                                        format!(
                                            "获得于 {}",
                                            at.with_timezone(&chrono::Local)
                                                .format("%Y-%m-%d %H:%M")
                                        )
                                    })
                                    .unwrap_or_default();
                                text(&granted, 36, at + 34, 286, 22, p.muted, small, 0);
                                let expiry = credit
                                    .expires_at
                                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                                    .map(|at| {
                                        at.with_timezone(&chrono::Local)
                                            .format("%Y-%m-%d %H:%M:%S")
                                            .to_string()
                                    })
                                    .unwrap_or_else(|| {
                                        if credit.expiry_known {
                                            "无过期时间".into()
                                        } else {
                                            "未提供过期时间".into()
                                        }
                                    });
                                text("过期时间", 330, at + 7, 312, 22, p.muted, small, DT_RIGHT);
                                text(&expiry, 330, at + 31, 312, 26, p.text, font, DT_RIGHT);
                            }
                        }
                    }
                } else if !agent.available {
                    if self.tab == 0 {
                        text("订阅余量", 24, y + 8, 300, 24, p.muted, font, 0);
                        text(
                            if agent.quota.is_empty() {
                                "—"
                            } else {
                                &agent.quota
                            },
                            24,
                            y + 38,
                            600,
                            52,
                            p.text,
                            value_font,
                            0,
                        );
                    }
                    text(
                        if self.data.loading {
                            "正在整理本地统计…"
                        } else {
                            "当前来源暂未提供项目与 Token 明细"
                        },
                        24,
                        y + 120,
                        620,
                        30,
                        p.muted,
                        font,
                        0,
                    );
                } else if rows.is_empty() {
                    text("所选时间段没有记录", 24, y + 18, 620, 32, p.muted, font, 0);
                } else {
                    match self.tab {
                        0 => {
                            round(20, y, 640, 88, 14, blend(p.field, p.background, 0.30));
                            let mut metrics = vec![
                                ("Token 用量", number(total.total)),
                                ("会话", sessions.len().to_string()),
                                ("请求", calls.to_string()),
                                (
                                    "订阅余量",
                                    if agent.quota.is_empty() {
                                        "—".into()
                                    } else {
                                        agent.quota.clone()
                                    },
                                ),
                            ];
                            if let Some(count) = agent.resets {
                                metrics.push(("重置次数 ›", format!("{count} 次")));
                            }
                            let pitch = if metrics.len() == 5 { 128 } else { 162 };
                            for (i, (name, value)) in metrics.iter().enumerate() {
                                let x = 36 + i as i32 * pitch;
                                text(name, x, y + 10, pitch - 12, 22, p.muted, small, 0);
                                text(
                                    value,
                                    x,
                                    y + 34,
                                    pitch - 12,
                                    40,
                                    if i >= 3 { p.active_border } else { p.text },
                                    value_font,
                                    0,
                                );
                            }
                            text("每日用量", 24, y + 108, 400, 24, p.text, medium, 0);
                            graph(y + 148, 170);
                            if agent.breakdown {
                                text("Token 构成", 24, y + 334, 400, 24, p.text, medium, 0);
                                composition(y + 372);
                            }
                        }
                        1 | 4 => {
                            let mut groups =
                                BTreeMap::<String, (Tokens, u64, HashSet<String>)>::new();
                            for row in &rows {
                                let key = if self.tab == 1 {
                                    &row.project
                                } else {
                                    &row.model
                                };
                                let item = groups
                                    .entry(if key.is_empty() {
                                        "未分类".into()
                                    } else {
                                        key.clone()
                                    })
                                    .or_default();
                                item.0.add(&row.tokens);
                                item.1 += row.calls;
                                item.2.insert(row.session.clone());
                            }
                            let mut groups: Vec<_> = groups.into_iter().collect();
                            groups.sort_by_key(|(_, v)| std::cmp::Reverse(v.0.total));
                            let visible = ((height - y - 48) / 72).max(1) as usize;
                            self.scroll = self.scroll.min(groups.len().saturating_sub(visible));
                            for (i, (name, (tokens, requests, sessions))) in
                                groups.iter().skip(self.scroll).take(visible).enumerate()
                            {
                                let at = y + i as i32 * 72;
                                let label = if self.tab == 1 {
                                    std::path::Path::new(name)
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or(name)
                                } else {
                                    name
                                };
                                text(label, 24, at, 400, 26, p.text, medium, 0);
                                text(
                                    &number(tokens.total),
                                    462,
                                    at,
                                    118,
                                    26,
                                    p.text,
                                    medium,
                                    DT_RIGHT,
                                );
                                text(
                                    &format!(
                                        "{:.1}%",
                                        tokens.total as f64 * 100. / total.total.max(1) as f64
                                    ),
                                    590,
                                    at,
                                    66,
                                    26,
                                    p.muted,
                                    small,
                                    DT_RIGHT,
                                );
                                text(
                                    &format!(
                                        "{name}  ·  {} 会话  ·  {requests} 请求",
                                        sessions.len()
                                    ),
                                    24,
                                    at + 29,
                                    632,
                                    22,
                                    p.muted,
                                    small,
                                    0,
                                );
                                round(24, at + 59, 632, 2, 1, track);
                                round(
                                    24,
                                    at + 59,
                                    (tokens.total as f64 / total.total.max(1) as f64 * 632.).round()
                                        as i32,
                                    2,
                                    1,
                                    blend(p.active_border, p.background, 0.7),
                                );
                            }
                        }
                        2 => {
                            graph(y + 8, 174);
                            let mut days = BTreeMap::<i32, (Tokens, u64)>::new();
                            for row in &rows {
                                let d = days.entry(row.day).or_default();
                                d.0.add(&row.tokens);
                                d.1 += row.calls;
                            }
                            let visible = ((height - y - 250) / 40).max(1) as usize;
                            self.scroll = self
                                .scroll
                                .min((self.days as usize).saturating_sub(visible));
                            text("日期", 24, y + 198, 200, 20, p.disabled, small, 0);
                            text("Token", 310, y + 198, 180, 20, p.disabled, small, DT_RIGHT);
                            text("请求", 540, y + 198, 116, 20, p.disabled, small, DT_RIGHT);
                            for (i, n) in
                                (self.scroll..self.days as usize).take(visible).enumerate()
                            {
                                let day = self.data.today - n as i32;
                                let at = y + 226 + i as i32 * 40;
                                let date = chrono::NaiveDate::from_num_days_from_ce_opt(day)
                                    .map(|d| d.to_string())
                                    .unwrap_or_default();
                                let value = days.get(&day);
                                text(&date, 24, at, 220, 32, p.text, font, 0);
                                text(
                                    &number(value.map(|v| v.0.total).unwrap_or(0)),
                                    310,
                                    at,
                                    180,
                                    32,
                                    p.text,
                                    font,
                                    DT_RIGHT,
                                );
                                text(
                                    &value.map(|v| v.1).unwrap_or(0).to_string(),
                                    540,
                                    at,
                                    116,
                                    32,
                                    p.muted,
                                    font,
                                    DT_RIGHT,
                                );
                                line(24, at + 36, 632, faint);
                            }
                        }
                        _ => {
                            if agent.breakdown {
                                composition(y + 12);
                                for (i, (label, value)) in [
                                    ("输入（含缓存）", number(total.input)),
                                    (
                                        "缓存命中率",
                                        if total.input > 0 {
                                            format!(
                                                "{:.1}%",
                                                total.cached as f64 * 100. / total.input as f64
                                            )
                                        } else {
                                            "—".into()
                                        },
                                    ),
                                    ("输出", number(total.output)),
                                    (
                                        "推理输出（包含在输出中）",
                                        if agent.id == "Codex" {
                                            number(total.reasoning)
                                        } else {
                                            "—".into()
                                        },
                                    ),
                                ]
                                .iter()
                                .enumerate()
                                {
                                    let at = y + 120 + i as i32 * 56;
                                    text(label, 24, at, 430, 42, p.muted, font, 0);
                                    text(value, 470, at, 186, 42, p.text, medium, DT_RIGHT);
                                    line(24, at + 49, 632, faint);
                                }
                                text(
                                    "缓存输入计入输入，推理输出计入输出；占比不重复相加",
                                    24,
                                    y + 365,
                                    632,
                                    26,
                                    p.disabled,
                                    small,
                                    0,
                                );
                            } else {
                                text(
                                    "当前来源未提供 Token 分类明细",
                                    24,
                                    y + 18,
                                    632,
                                    32,
                                    p.muted,
                                    font,
                                    0,
                                );
                            }
                        }
                    }
                }
                if self.tab == 0
                    && !self.reset_details
                    && (!agent.available || rows.is_empty())
                    && let Some(count) = agent.resets
                {
                    round(532, y, 128, 88, 12, blend(p.field, p.background, 0.30));
                    text("重置次数 ›", 548, y + 10, 100, 22, p.muted, small, 0);
                    text(
                        &format!("{count} 次"),
                        548,
                        y + 34,
                        100,
                        40,
                        p.active_border,
                        value_font,
                        0,
                    );
                }
                RestoreDC(dc, saved);
                text(
                    &agent.source,
                    24,
                    height - 34,
                    632,
                    22,
                    p.disabled,
                    small,
                    0,
                );
            } else {
                RestoreDC(dc, saved);
                text(
                    "正在读取统计",
                    24,
                    content_top + 12,
                    632,
                    32,
                    p.muted,
                    font,
                    0,
                );
            }
            ultra_bar_native_ui::fonts::delete_object(small);
            ultra_bar_native_ui::fonts::delete_object(medium);
            ultra_bar_native_ui::fonts::delete_object(value_font);
        }
    }
}
