//! Read-only clock summaries from background calendar panels. Directory events wake the UI.
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
};
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Summary {
    version: u32,
    display: usize,
    lunar: String,
    weather: String,
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
        let phase = crate::clock_feed::now() / 6;
        let rotate = phase != self.phase
            && self
                .summaries
                .values()
                .any(|s| s.display == 2 && !s.weather.is_empty());
        self.phase = phase;
        changed || rotate
    }
    pub fn render(&self, id: &str, template: &str, time: &str, pattern: &str) -> (String, String) {
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
                2 if self.phase % 2 == 1 => weather.clone(),
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
        feed.phase = 1;
        assert_eq!(
            feed.render("calendar", "{time}", "fallback", "HH:mm").0,
            "晴 16°C"
        );
        feed.phase = 0;
        assert!(
            feed.render("calendar", "{time}", "fallback", "HH:mm")
                .0
                .ends_with("正月初一")
        );
        feed.summaries.get_mut("calendar").unwrap().updated = 0;
        assert_eq!(
            feed.render("calendar", "{time}", "fallback", "HH:mm").0,
            "fallback"
        );
    }
}
