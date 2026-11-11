use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub const ITEM_GAP: i32 = 2;
pub const TRAY_PADDING: i32 = 3;
pub fn tray_pixels(content: i32, dpi: i32) -> i32 {
    content + 2 * (TRAY_PADDING * dpi / 96) + ITEM_GAP
}

/// Item boundaries exclude the gap; content padding belongs inside these edges.
pub fn item_edges(left: i32, pitch: i32) -> (i32, i32) {
    (left + ITEM_GAP / 2, left + pitch - ITEM_GAP / 2)
}

#[derive(Clone, Copy, Default, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskbarSize {
    #[default]
    Normal,
    Small,
}
impl TaskbarSize {
    pub fn height(self) -> i32 {
        if self == Self::Small { 30 } else { 40 }
    }
    pub fn icon(self) -> i32 {
        if self == Self::Small { 16 } else { 32 }
    }
    pub fn padding(self) -> i32 {
        if self == Self::Small { 6 } else { 8 }
    }
    /// Content is already in physical pixels. Padding scales; the external gap does not.
    pub fn item_pixels(self, content: i32, dpi: i32) -> i32 {
        content + 2 * (self.padding() * dpi / 96) + ITEM_GAP
    }
    pub fn slot_pixels(self, dpi: i32) -> i32 {
        self.item_pixels(self.icon() * dpi / 96, dpi)
    }
    pub fn start_pixels(self, dpi: i32) -> i32 {
        self.item_pixels(self.logo() * dpi / 96, dpi)
    }
    pub fn logo(self) -> i32 {
        if self == Self::Small { 16 } else { 20 }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub accent_color: String,
    pub icons_only: bool,
    pub size: TaskbarSize,
    pub clock_format: String,
    pub clock_extension: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            accent_color: crate::visual::theme::hex(crate::visual::theme::DEFAULT_ACCENT),
            icons_only: false,
            size: TaskbarSize::Normal,
            clock_format: crate::clock::DEFAULT_FORMAT.into(),
            clock_extension: String::new(),
        }
    }
}

impl Settings {
    fn path() -> Result<PathBuf> {
        Ok(crate::data_paths::root()?.join("settings.json"))
    }
    pub fn load() -> Self {
        let read = || -> Result<Self> {
            let path = Self::path()?;
            if !path.exists() {
                return Ok(Self::default());
            }
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        };
        read().unwrap_or_else(|error| {
            eprintln!("[settings] {error:#}");
            Self::default()
        })
    }
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().context("invalid settings path")?)?;
        let pending = path.with_extension("json.new");
        std::fs::write(&pending, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(pending, path).context("Cannot save taskbar settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_gap_is_two_pixels_without_consuming_content_padding() {
        for size in [TaskbarSize::Normal, TaskbarSize::Small] {
            for dpi in [96, 120, 144, 192] {
                let icon = size.icon() * dpi / 96;
                let pitch = size.slot_pixels(dpi);
                let (left, right) = item_edges(0, pitch);
                let (next_left, _) = item_edges(pitch, pitch);
                assert_eq!(next_left - right, 2);
                assert_eq!(right - left - icon, 2 * (size.padding() * dpi / 96));
                assert!(right - left - icon >= 12);
                assert!(tray_pixels(16 * dpi / 96, dpi) < size.item_pixels(16 * dpi / 96, dpi));
            }
        }
    }
    #[test]
    fn preference_round_trips_and_defaults_to_adaptive() {
        assert!(!serde_json::from_str::<Settings>("{}").unwrap().icons_only);
        let saved = serde_json::to_vec(&Settings {
            icons_only: true,
            ..Default::default()
        })
        .unwrap();
        assert!(
            serde_json::from_slice::<Settings>(&saved)
                .unwrap()
                .icons_only
        );
    }
    #[test]
    fn old_settings_keep_normal_size_and_small_preserves_icon_preference() {
        let mut settings: Settings = serde_json::from_str(r#"{"icons_only":true}"#).unwrap();
        assert_eq!(settings.size, TaskbarSize::Normal);
        assert_eq!(settings.clock_format, crate::clock::DEFAULT_FORMAT);
        assert_eq!(settings.accent_color, "#69B4FF");
        settings.accent_color = "#C458F5".into();
        assert!(settings.clock_extension.is_empty());
        settings.size = TaskbarSize::Small;
        let loaded: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(loaded.size, TaskbarSize::Small);
        assert_eq!(loaded.accent_color, "#C458F5");
        assert!(loaded.icons_only);
        assert!(loaded.size.height() < TaskbarSize::Normal.height());
        assert_eq!(loaded.size.icon(), 16);
    }
}
