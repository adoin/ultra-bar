use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Preference {
    pub id: String,
    pub executable: String,
    pub uid: u32,
    pub title: String,
    pub collapsed: bool,
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub entries: Vec<Preference>,
    pub import_prompt_shown: bool,
}

pub fn identity(path: &str, uid: u32) -> String {
    format!("{}#{uid}", path.replace('/', "\\").to_lowercase())
}

impl Preferences {
    fn path() -> Result<PathBuf> {
        Ok(crate::data_paths::root()?.join("tray.json"))
    }
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let value = Self::parse(&std::fs::read(path)?)?;
        Ok(value)
    }
    fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes)?;
        let mut ids = std::collections::HashSet::new();
        for entry in &value.entries {
            if entry.id != identity(&entry.executable, entry.uid) || !ids.insert(entry.id.clone()) {
                bail!("Invalid or duplicate tray identity");
            }
        }
        Ok(value)
    }
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        self.save_at(&path)
    }
    fn save_at(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().context("invalid settings directory")?)?;
        let pending = path.with_extension("json.new");
        std::fs::write(&pending, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(pending, path)?;
        Ok(())
    }
    pub fn merge(&mut self, incoming: impl IntoIterator<Item = Preference>) -> usize {
        let mut added = 0;
        for entry in incoming {
            if !self.entries.iter().any(|previous| previous.id == entry.id) {
                self.entries.push(entry);
                added += 1;
            }
        }
        added
    }
    #[cfg(test)]
    pub fn collapsed(&self, id: &str) -> bool {
        self.collapsed_or(id, true)
    }
    pub fn collapsed_or(&self, id: &str, native: bool) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map_or(native, |entry| entry.collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_icons_use_native_placement_and_local_choice_overrides_both_native_states() {
        let mut preferences = Preferences::default();
        assert!(!preferences.collapsed_or("new", false));
        assert!(preferences.collapsed_or("new", true));
        preferences.entries.push(item("C:/App.exe", 1, false));
        let id = preferences.entries[0].id.clone();
        assert!(!preferences.collapsed_or(&id, true));
        preferences.entries[0].collapsed = true;
        assert!(preferences.collapsed_or(&id, false));
        let restored: Preferences =
            serde_json::from_slice(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(restored.collapsed_or(&id, false));
    }
    #[test]
    fn atomic_save_replaces_previous_preferences_without_leaving_pending_file() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from("D:/tmp/ultra-bar-tray-tests")
            .join(format!("{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("tray.json");
        let mut preferences = Preferences {
            entries: vec![item("C:/App.exe", 1, false)],
            ..Default::default()
        };
        preferences.save_at(&path).unwrap();
        preferences.entries[0].collapsed = true;
        preferences.save_at(&path).unwrap();
        let restored = Preferences::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert!(restored.entries[0].collapsed);
        assert!(!path.with_extension("json.new").exists());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    fn item(path: &str, uid: u32, collapsed: bool) -> Preference {
        Preference {
            id: identity(path, uid),
            executable: path.into(),
            uid,
            title: "App".into(),
            collapsed,
        }
    }
    #[test]
    fn merging_import_keeps_user_choices_and_distinguishes_multiple_icons() {
        let mut preferences = Preferences {
            entries: vec![item("C:/App.exe", 1, true)],
            ..Default::default()
        };
        assert_eq!(
            preferences.merge([item("C:/App.exe", 1, false), item("C:/App.exe", 2, false)]),
            1
        );
        assert!(preferences.collapsed(&identity("C:/App.exe", 1)));
        assert!(!preferences.collapsed(&identity("C:/App.exe", 2)));
        assert_eq!(identity("C:/App.exe", 1), identity("c:\\app.EXE", 1));
    }
    #[test]
    fn duplicates_and_corrupt_imports_are_rejected() {
        let entry = item("C:/App.exe", 1, false);
        let bytes = serde_json::to_vec(&Preferences {
            entries: vec![entry.clone(), entry],
            ..Default::default()
        })
        .unwrap();
        assert!(Preferences::parse(&bytes).is_err());
        assert!(Preferences::parse(b"invalid").is_err());
        assert!(Preferences::default().collapsed("unknown"));
    }
    #[test]
    fn skipping_initial_import_survives_restart_and_manual_import() {
        let mut preferences = Preferences {
            import_prompt_shown: true,
            ..Default::default()
        };
        let restored = Preferences::parse(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(restored.import_prompt_shown && restored.entries.is_empty());
        preferences.merge([item("C:/App.exe", 1, false)]);
        let restored = Preferences::parse(&serde_json::to_vec(&preferences).unwrap()).unwrap();
        assert!(restored.import_prompt_shown && restored.entries.len() == 1);
    }
}
