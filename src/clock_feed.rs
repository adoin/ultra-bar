//! Read-only clock summaries from background calendar panels. Directory events wake the UI.
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
const SLIDE_DURATION: Duration = Duration::from_millis(240);
pub unsafe fn draw_slide(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    rect: &windows_sys::Win32::Foundation::RECT,
    current: &str,
    previous: &str,
    phase: f64,
) {
    use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};
    unsafe {
        let saved = SaveDC(dc);
        IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
        let travel = rect.bottom - rect.top;
        let shift = (travel as f64 * (1. - phase.clamp(0., 1.))).round() as i32;
        let flags = DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS | DT_NOPREFIX;
        let mut incoming = RECT {
            top: rect.top + shift,
            bottom: rect.bottom + shift,
            ..*rect
        };
        crate::visual::text::draw_text(
            dc,
            crate::visual::wide(current).as_ptr(),
            -1,
            &mut incoming,
            flags,
        );
        incoming.top -= travel;
        incoming.bottom -= travel;
        crate::visual::text::draw_text(
            dc,
            crate::visual::wide(previous).as_ptr(),
            -1,
            &mut incoming,
            flags,
        );
        RestoreDC(dc, saved);
    }
}
pub unsafe fn smoke(directory: &std::path::Path) -> anyhow::Result<()> {
    use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};
    unsafe {
        std::fs::create_dir_all(directory)?;
        for dpi in [96, 144, 192] {
            let unit = |x| x * dpi / 96;
            let surface = crate::visual::Surface::new(unit(900), unit(64))
                .ok_or_else(|| anyhow::anyhow!("Cannot create clock preview"))?;
            let bg = 0x00201612;
            let brush = CreateSolidBrush(bg);
            let area = RECT {
                left: 0,
                top: 0,
                right: unit(900),
                bottom: unit(64),
            };
            FillRect(surface.dc, &area, brush);
            crate::visual::fonts::delete_object(brush);
            SetBkMode(surface.dc, TRANSPARENT as _);
            SetTextColor(surface.dc, 0x00ffffff);
            let font = crate::visual::fonts::create_font(
                -unit(13),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_DEFAULT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                CLEARTYPE_NATURAL_QUALITY as _,
                DEFAULT_PITCH as _,
                crate::visual::wide("Segoe UI").as_ptr(),
            );
            let old_font = SelectObject(surface.dc, font);
            for (index, progress) in [0., 0.25, 0.5, 0.75, 1.].iter().enumerate() {
                let rect = RECT {
                    left: unit(index as i32 * 180 + 8),
                    top: unit(22),
                    right: unit((index as i32 + 1) * 180 - 8),
                    bottom: unit(44),
                };
                draw_slide(
                    surface.dc,
                    &rect,
                    "福州 晴 24°C",
                    "11-16 09:30 周一",
                    *progress,
                );
            }
            GdiFlush();
            // Transition glyphs must not leak into the surrounding taskbar.
            for y in (0..unit(20)).chain(unit(46)..unit(64)) {
                for x in 0..unit(900) {
                    anyhow::ensure!(
                        GetPixel(surface.dc, x, y) == bg,
                        "Clock text escaped its clip"
                    );
                }
            }
            surface.save_bmp(&directory.join(format!("clock-slide-{dpi}.bmp")))?;
            SelectObject(surface.dc, old_font);
            crate::visual::fonts::delete_object(font);
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Motion {
    provider: String,
    page: usize,
    current: String,
    previous: Option<String>,
    started: Option<Instant>,
}
impl Motion {
    pub fn update(
        &mut self,
        provider: &str,
        page: usize,
        text: &str,
        enabled: bool,
        now: Instant,
    ) -> bool {
        if !enabled || !self.animating(now) {
            self.previous = None;
            self.started = None;
        }
        if provider != self.provider || page != self.page {
            self.previous = (enabled
                && provider == self.provider
                && text != self.current
                && !self.current.is_empty())
            .then(|| self.current.clone());
            self.started = self.previous.as_ref().map(|_| now);
            self.provider = provider.into();
            self.page = page;
        }
        if text != self.current {
            self.current = text.into();
        }
        self.animating(now)
    }
    pub fn animating(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|start| now.saturating_duration_since(start) < SLIDE_DURATION)
    }
    pub fn frame(&self, now: Instant) -> Option<(&str, f64)> {
        if !self.animating(now) {
            return None;
        }
        let t = now.saturating_duration_since(self.started?).as_secs_f64()
            / SLIDE_DURATION.as_secs_f64();
        Some((self.previous.as_deref()?, 1. - (1. - t).powi(3)))
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Summary {
    version: u32,
    display: usize,
    rotation_seconds: u32,
    clock_seconds: u32,
    weather_seconds: u32,
    lunar: String,
    weather: String,
    humidity_percent: Option<u8>,
    tooltip: String,
    clock_format: String,
    utc_offset_seconds: i32,
    updated: i64,
}
pub struct Feed {
    root: PathBuf,
    watcher: Option<RecommendedWatcher>,
    receive: Receiver<()>,
    summaries: HashMap<String, Summary>,
    phase: u64,
    known: Vec<String>,
}
impl Feed {
    pub fn new(hwnd: isize, message: u32) -> Self {
        let root = crate::data_paths::root()
            .unwrap_or_default()
            .join("plugins");
        let _ = std::fs::create_dir_all(&root);
        let (send, receive) = mpsc::sync_channel(1);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|e| {
                !matches!(e.kind, notify::EventKind::Access(_))
                    && e.paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == "clock.json"))
            }) {
                let _ = send.try_send(());
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                        hwnd as _, message, 0, 0,
                    );
                }
            }
        })
        .ok()
        .and_then(|mut w| w.watch(&root, RecursiveMode::Recursive).ok().map(|_| w));
        Self {
            root,
            watcher,
            receive,
            summaries: HashMap::new(),
            phase: 0,
            known: vec![],
        }
    }
    fn reload(&mut self, widgets: &[crate::plugins::Widget]) {
        self.summaries.clear();
        for widget in widgets
            .iter()
            .filter(|w| w.slot == crate::plugins::Slot::Clock)
        {
            let path = self.root.join(&widget.id).join("clock.json");
            if let Ok(metadata) = std::fs::metadata(&path)
                && metadata.len() <= 16384
                && let Ok(bytes) = std::fs::read(path)
                && let Ok(data) = serde_json::from_slice::<Summary>(&bytes)
                && data.version == 1
                && data.weather.chars().count() <= 80
                && data.lunar.chars().count() <= 40
                && data.tooltip.chars().count() <= 1024
                && data.clock_format.chars().count() <= 128
                && !data.clock_format.chars().any(char::is_control)
                && data.utc_offset_seconds.unsigned_abs() <= 86400
            {
                self.summaries.insert(widget.id.clone(), data);
            }
        }
    }
    pub fn poll(&mut self, widgets: &[crate::plugins::Widget]) -> bool {
        let mut changed = false;
        while self.receive.try_recv().is_ok() {
            changed = true;
        }
        let ids: Vec<_> = widgets
            .iter()
            .filter(|w| w.slot == crate::plugins::Slot::Clock)
            .map(|w| w.id.clone())
            .collect();
        if changed || ids != self.known {
            self.reload(widgets);
            self.known = ids;
        }
        let phase = crate::clock_feed::now();
        let rotate = self.summaries.values().any(|s| {
            s.display == 2
                && !s.weather.is_empty()
                && rotation_page(s, phase) != rotation_page(s, self.phase)
        });
        self.phase = phase;
        changed || rotate
    }
    pub fn render(&self, id: &str, template: &str, time: &str, pattern: &str) -> (String, String) {
        self.render_phase(id, template, time, pattern, self.page(id) as u64)
    }
    pub fn weather_detail(&self, id: &str, template: &str) -> Option<String> {
        let summary = self.summaries.get(id)?;
        if self.page(id) != 1 {
            return None;
        }
        let humidity = summary.humidity_percent.filter(|percent| *percent <= 100)?;
        Some(template.replace("{time}", &format!("{} 湿度{humidity}%", summary.weather)))
    }
    pub fn page(&self, id: &str) -> usize {
        self.summaries
            .get(id)
            .filter(|s| {
                s.display == 2 && !s.weather.is_empty() && (now() as i64 - s.updated).abs() <= 180
            })
            .map_or(0, |s| rotation_page(s, self.phase))
    }
    /// Reserve the widest page before rotating, including the template and lunar date.
    pub fn width_texts(&self, id: &str, template: &str, time: &str, pattern: &str) -> [String; 2] {
        [0, 1].map(|phase| self.render_phase(id, template, time, pattern, phase).0)
    }
    fn render_phase(
        &self,
        id: &str,
        template: &str,
        time: &str,
        pattern: &str,
        phase: u64,
    ) -> (String, String) {
        let Some(summary) = self
            .summaries
            .get(id)
            .filter(|s| (now() as i64 - s.updated).abs() <= 180)
        else {
            return (template.replace("{time}", time), String::new());
        };
        let pattern = if summary.clock_format.is_empty() {
            pattern
        } else {
            &summary.clock_format
        };
        let time =
            regional_time(pattern, summary.utc_offset_seconds).unwrap_or_else(|| time.into());
        let time = if summary.lunar.is_empty() {
            time
        } else {
            format!("{time} {}", summary.lunar)
        };
        let weather = &summary.weather;
        let text = if weather.is_empty() {
            time
        } else {
            match summary.display {
                1 => format!("{time}  {weather}"),
                2 if phase % 2 == 1 => weather.clone(),
                _ => time,
            }
        };
        let mut tip = summary
            .tooltip
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if tip.is_empty() {
            tip = regional_time("YYYY-MM-DD dddd HH:mm", summary.utc_offset_seconds)
                .unwrap_or_else(|| text.clone());
            if !summary.lunar.trim().is_empty() {
                tip.push_str(&format!("\n{}", summary.lunar.trim()));
            }
        }
        (template.replace("{time}", &text), tip)
    }
}
fn rotation_page(summary: &Summary, seconds: u64) -> usize {
    fn duration(value: u32, fallback: u64) -> u64 {
        if (2..=3600).contains(&value) {
            u64::from(value)
        } else {
            fallback
        }
    }
    let legacy = duration(summary.rotation_seconds, 6);
    let clock = duration(summary.clock_seconds, legacy);
    let weather = duration(summary.weather_seconds, legacy);
    usize::from(seconds % (clock + weather) >= clock)
}
impl Drop for Feed {
    fn drop(&mut self) {
        self.watcher.take();
    }
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn regional_time(pattern: &str, offset: i32) -> Option<String> {
    unsafe {
        use windows_sys::Win32::{
            Foundation::*,
            System::{SystemInformation::GetSystemTimeAsFileTime, Time::FileTimeToSystemTime},
        };
        let mut ft: FILETIME = std::mem::zeroed();
        GetSystemTimeAsFileTime(&mut ft);
        let ticks = ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) as i128
            + offset as i128 * 10_000_000;
        let ticks = u64::try_from(ticks).ok()?;
        let ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut time: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&ft, &mut time) == 0 {
            return None;
        }
        crate::clock::format(pattern, &time).ok()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slide_frames_are_clipped_at_normal_and_high_dpi() {
        let directory = std::env::var_os("ULTRA_BAR_CLOCK_SLIDE_CAPTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(format!(
                    "D:/tmp/ultra-bar-clock-slide-tests-{}",
                    std::process::id()
                ))
            });
        unsafe {
            smoke(&directory).unwrap();
        }
    }
    #[test]
    fn slide_only_runs_for_page_changes_and_stops_after_its_deadline() {
        let now = Instant::now();
        let mut motion = Motion::default();
        assert!(!motion.update("calendar", 0, "09:00", true, now));
        assert!(!motion.update("calendar", 0, "09:01", true, now));
        assert!(motion.update("calendar", 1, "晴 16°C", true, now));
        assert_eq!(motion.frame(now), Some(("09:01", 0.)));
        let (_, progress) = motion.frame(now + Duration::from_millis(120)).unwrap();
        assert!(progress > 0.5 && progress < 1.);
        assert!(!motion.animating(now + SLIDE_DURATION));
        assert!(motion.frame(now + SLIDE_DURATION).is_none());
        assert!(!motion.update("another", 0, "09:01", true, now + SLIDE_DURATION));
        assert!(!motion.update("another", 1, "weather", false, now + SLIDE_DURATION));
    }
    #[test]
    fn rotation_supports_separate_durations_and_legacy_feeds() {
        for (value, expected) in [(0, 6), (1, 6), (2, 2), (12, 12), (3600, 3600), (3601, 6)] {
            assert_eq!(
                rotation_page(
                    &Summary {
                        rotation_seconds: value,
                        ..Default::default()
                    },
                    expected
                ),
                1
            );
        }
        let summary = Summary {
            clock_seconds: 12,
            weather_seconds: 4,
            ..Default::default()
        };
        for (seconds, page) in [(0, 0), (11, 0), (12, 1), (15, 1), (16, 0), (28, 1)] {
            assert_eq!(rotation_page(&summary, seconds), page);
        }
    }
    #[test]
    fn display_modes_keep_lunar_and_stale_panels_fall_back_to_system_time() {
        let (_, receive) = mpsc::sync_channel(1);
        let mut feed = Feed {
            root: PathBuf::new(),
            watcher: None,
            receive,
            summaries: HashMap::new(),
            phase: 0,
            known: vec![],
        };
        feed.summaries.insert(
            "calendar".into(),
            Summary {
                version: 1,
                display: 1,
                lunar: "正月初一".into(),
                weather: "晴 16°C".into(),
                humidity_percent: Some(75),
                updated: now() as i64,
                ..Default::default()
            },
        );
        feed.summaries.get_mut("calendar").unwrap().tooltip = "\n \n\t".into();
        let (text, tip) = feed.render("calendar", "{time}", "fallback", "HH:mm");
        assert!(text.ends_with("正月初一  晴 16°C"));
        assert!(tip.contains("星期"));
        assert!(!tip.trim().is_empty());
        feed.summaries.get_mut("calendar").unwrap().display = 2;
        let pages = feed.width_texts("calendar", "[{time}]", "fallback", "HH:mm");
        assert!(pages[0].ends_with("正月初一]"));
        assert_eq!(pages[1], "[晴 16°C]");
        feed.phase = 6;
        assert_eq!(
            feed.weather_detail("calendar", "[{time}]").as_deref(),
            Some("[晴 16°C 湿度75%]")
        );
        assert_eq!(
            pages,
            feed.width_texts("calendar", "[{time}]", "fallback", "HH:mm")
        );
        assert_eq!(
            feed.render("calendar", "{time}", "fallback", "HH:mm").0,
            "晴 16°C"
        );
        feed.phase = 0;
        assert!(feed.weather_detail("calendar", "{time}").is_none());
        assert!(
            feed.render("calendar", "{time}", "fallback", "HH:mm")
                .0
                .ends_with("正月初一")
        );
        feed.summaries.get_mut("calendar").unwrap().rotation_seconds = 12;
        feed.phase = 6;
        assert_eq!(feed.page("calendar"), 0);
        feed.phase = 12;
        assert_eq!(feed.page("calendar"), 1);
        assert_eq!(
            feed.render("calendar", "{time}", "fallback", "HH:mm").0,
            "晴 16°C"
        );
        feed.summaries.get_mut("calendar").unwrap().updated = 0;
        assert_eq!(
            feed.width_texts("calendar", "{time}", "fallback", "HH:mm"),
            ["fallback", "fallback"]
        );
        assert_eq!(
            feed.render("calendar", "{time}", "fallback", "HH:mm").0,
            "fallback"
        );
    }
}
