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

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveStyle {
    Inset,
    #[default]
    #[serde(alias = "accent_inset", alias = "solid")]
    Underline,
    TopDot,
}
impl ActiveStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inset => "纯凹陷",
            Self::Underline => "底部横条",
            Self::TopDot => "顶部粗点",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub accent_color: String,
    pub icons_only: bool,
    pub size: TaskbarSize,
    pub active_style: ActiveStyle,
    pub clock_format: String,
    pub clock_extension: String,
    pub font_family: String,
    pub startup_enabled: bool,
    pub hide_fullscreen: bool,
    pub auto_check_updates: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            accent_color: crate::visual::theme::hex(crate::visual::theme::DEFAULT_ACCENT),
            icons_only: false,
            size: TaskbarSize::Normal,
            active_style: ActiveStyle::default(),
            clock_format: crate::clock::DEFAULT_FORMAT.into(),
            clock_extension: String::new(),
            font_family: "Segoe UI".into(),
            startup_enabled: true,
            hide_fullscreen: true,
            auto_check_updates: true,
        }
    }
}

impl Settings {
    pub(crate) fn path() -> Result<PathBuf> {
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
        let previous = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        std::fs::rename(pending, &path).context("Cannot save taskbar settings")?;
        ultra_bar_native_ui::diagnostics::event(
            "settings",
            "taskbar.saved",
            serde_json::json!({"before":previous,"after":self,"path":path}),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_preferences_keep_old_configuration_and_preserve_explicit_opt_out() {
        let defaults: super::Settings = serde_json::from_str("{}").unwrap();
        assert!(defaults.startup_enabled && defaults.hide_fullscreen);
        assert_eq!(defaults.font_family, "Segoe UI");
        let settings: super::Settings = serde_json::from_str(
            r#"{"startup_enabled":false,"hide_fullscreen":false,"font_family":"Arial"}"#,
        )
        .unwrap();
        let roundtrip: super::Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(!roundtrip.startup_enabled && !roundtrip.hide_fullscreen);
        assert_eq!(roundtrip.font_family, "Arial");
    }
    #[test]
    fn older_active_styles_migrate_to_the_inset_marker_without_losing_preferences() {
        for name in ["accent_inset", "solid"] {
            let settings: super::Settings = serde_json::from_value(
                serde_json::json!({"active_style":name,"icons_only":true,"accent_color":"#B8591A"}),
            )
            .unwrap();
            assert_eq!(settings.active_style, super::ActiveStyle::Underline);
            assert!(settings.icons_only);
            assert_eq!(settings.accent_color, "#B8591A");
        }
        assert_eq!(
            super::Settings::default().active_style,
            super::ActiveStyle::Underline
        );
    }
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
