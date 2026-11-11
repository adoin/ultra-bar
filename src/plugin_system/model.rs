use anyhow::{Result, bail, ensure};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, path::PathBuf};

pub const OFFICIAL_ID: &str = "github:1452116417";
pub const OFFICIAL_REPO: &str = "adoin/ultra-bar-plugins";
pub const EXTENSION: &str = "ubplugin";
pub const PLATFORM: &str = "windows-x86_64";
pub const INVENTORY_MESSAGE: &str = "UltraBar.PluginInventoryChanged.v1";

pub fn canonical_id(id: &str) -> &str {
    if id == "schedule-calendar" {
        "com.emssion.schedule-calendar"
    } else {
        id
    }
}
pub fn valid_id(id: &str) -> bool {
    let parts: Vec<_> = id.split('.').collect();
    id.len() <= 128
        && parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_lowercase()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}
pub fn precedence(candidate: &str, current: &str) -> Result<Ordering> {
    Ok(Version::parse(candidate)?.cmp_precedence(&Version::parse(current)?))
}
pub fn compatible(requirement: &str) -> Result<bool> {
    Ok(VersionReq::parse(requirement)?.matches(&Version::parse(env!("CARGO_PKG_VERSION"))?))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub full_name: String,
    pub name: String,
}
impl Source {
    pub fn official() -> Self {
        Self {
            id: OFFICIAL_ID.into(),
            full_name: OFFICIAL_REPO.into(),
            name: "官方插件库".into(),
        }
    }
    pub fn official_source(&self) -> bool {
        self.id == OFFICIAL_ID
    }
}
pub fn repository_name(input: &str) -> Result<String> {
    let input = input.trim().trim_end_matches('/').trim_end_matches(".git");
    let input = input
        .strip_prefix("https://github.com/")
        .or_else(|| input.strip_prefix("http://github.com/"))
        .or_else(|| input.strip_prefix("git@github.com:"))
        .unwrap_or(input);
    let parts: Vec<_> = input.split('/').collect();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| !part.is_empty()
                && part.len() <= 100
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)))
            && parts.iter().all(|part| *part != "." && *part != ".."),
        "请输入 GitHub owner/repository 或仓库 URL"
    );
    Ok(input.to_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub platform: String,
    pub host: String,
    pub asset: String,
    pub sha256: String,
    #[serde(default)]
    pub description: String,
}
impl CatalogEntry {
    pub fn validate(&self) -> Result<()> {
        ensure!(valid_id(&self.id), "无效的插件 ID");
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 256
                && !self.name.chars().any(char::is_control)
                && self.version.len() <= 128
                && self.host.len() <= 256
                && self.description.len() <= 8192,
            "发布目录元数据无效"
        );
        Version::parse(&self.version)?;
        ensure!(self.platform == PLATFORM, "不支持的平台");
        ensure!(compatible(&self.host)?, "插件不兼容当前 Ultra Bar 版本");
        ensure!(
            self.sha256.len() == 64 && self.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "插件摘要无效"
        );
        ensure!(
            !self.asset.contains(['/', '\\']) && self.asset.ends_with(".ubplugin"),
            "无效的插件产物名"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema_version: u32,
    pub plugins: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub entry: CatalogEntry,
    pub source: Source,
    pub asset_id: u64,
    pub download_url: String,
    pub size: u64,
    pub release_tag: String,
}
pub fn eligible_update(installed: &Installed, candidate: &Candidate) -> bool {
    installed.id == candidate.entry.id
        && installed
            .source
            .as_ref()
            .is_none_or(|source| source.id == candidate.source.id)
        && precedence(&candidate.entry.version, &installed.version).is_ok_and(|order| order.is_gt())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub generation: String,
    pub source: Option<Source>,
    pub installed_at: u64,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct Registry {
    pub plugins: Vec<Installed>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sources {
    pub repositories: Vec<Source>,
}
impl Default for Sources {
    fn default() -> Self {
        Self {
            repositories: vec![Source::official()],
        }
    }
}

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
}
impl Paths {
    pub fn user() -> Result<Self> {
        Ok(Self {
            root: crate::data_paths::root()?.join("plugin-system"),
        })
    }
    pub fn registry(&self) -> Result<Registry> {
        self.read("installed.json")
    }
    pub fn sources(&self) -> Result<Sources> {
        let mut sources: Sources = self.read("repositories.json")?;
        if !sources
            .repositories
            .iter()
            .any(|source| source.official_source())
        {
            sources.repositories.insert(0, Source::official());
        }
        Ok(sources)
    }
    fn read<T: serde::de::DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        let path = self.root.join(name);
        if !path.exists() {
            return Ok(T::default());
        }
        ensure!(
            std::fs::metadata(&path)?.len() <= 4 * 1024 * 1024,
            "插件状态文件过大"
        );
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        ensure!(
            matches!(name, "installed.json" | "repositories.json"),
            "无效的插件状态文件"
        );
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(name);
        let pending = path.with_extension("json.new");
        std::fs::write(&pending, serde_json::to_vec_pretty(value)?)?;
        std::fs::rename(pending, path)?;
        Ok(())
    }
    pub fn directory(&self, installed: &Installed) -> Result<PathBuf> {
        ensure!(valid_id(&installed.id), "已安装插件 ID 无效");
        Version::parse(&installed.version)?;
        ensure!(
            installed.digest.len() == 64 && installed.digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "已安装插件摘要无效"
        );
        ensure!(
            !installed.generation.is_empty()
                && installed.generation.len() <= 64
                && installed.generation.bytes().all(|c| c.is_ascii_hexdigit()),
            "安装实例无效"
        );
        Ok(self
            .root
            .join("store")
            .join(&installed.id)
            .join(&installed.version)
            .join(format!("{}-{}", installed.digest, installed.generation)))
    }
    pub fn active_directories(&self) -> Result<Vec<PathBuf>> {
        self.registry()?
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled)
            .map(|plugin| self.directory(plugin))
            .collect()
    }
}

pub fn safe_relative(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name.len() <= 2048
            && !name.starts_with('/')
            && !name.contains(['\\', ':', '\0']),
        "无效的包内路径"
    );
    let mut result = PathBuf::new();
    for part in name.trim_end_matches('/').split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' ']),
            "包内路径越界或不兼容 Windows"
        );
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            bail!("包内路径包含 Windows 保留名称");
        }
        ensure!(
            !part
                .chars()
                .any(|c| c.is_control() || "<>\"|?*".contains(c)),
            "包内路径包含无效字符"
        );
        result.push(part);
    }
    ensure!(
        !result.is_absolute()
            && result
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "包内路径越界"
    );
    Ok(result)
}

pub fn notify_inventory() {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let window = FindWindowW(
            crate::visual::wide("UltraBar.Window").as_ptr(),
            std::ptr::null(),
        );
        if !window.is_null() {
            PostMessageW(
                window,
                RegisterWindowMessageW(crate::visual::wide(INVENTORY_MESSAGE).as_ptr()),
                0,
                0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_updates_keep_repository_identity_unless_user_switches_source() {
        let official = Source::official();
        let mut installed = Installed {
            id: "com.example.clock".into(),
            name: "Clock".into(),
            version: "1.0.0".into(),
            digest: "a".repeat(64),
            generation: "1".into(),
            source: Some(official.clone()),
            installed_at: 0,
            enabled: true,
        };
        let mut candidate = Candidate {
            entry: CatalogEntry {
                id: installed.id.clone(),
                name: "Clock".into(),
                version: "1.1.0".into(),
                platform: PLATFORM.into(),
                host: "*".into(),
                asset: "clock.ubplugin".into(),
                sha256: "b".repeat(64),
                description: String::new(),
            },
            source: official,
            asset_id: 1,
            download_url: String::new(),
            size: 0,
            release_tag: String::new(),
        };
        assert!(eligible_update(&installed, &candidate));
        candidate.source.id = "github:999".into();
        assert!(!eligible_update(&installed, &candidate));
        installed.source = None;
        assert!(eligible_update(&installed, &candidate));
        candidate.entry.id = "com.example.other".into();
        assert!(!eligible_update(&installed, &candidate));
        candidate.entry.id = installed.id.clone();
        candidate.entry.version = "0.9.0".into();
        assert!(!eligible_update(&installed, &candidate));
    }
    use std::path::Path;
    #[test]
    fn identities_and_semver_are_not_lexicographic() {
        assert!(valid_id("com.emssion.schedule-calendar"));
        assert!(!valid_id("../calendar"));
        assert_eq!(precedence("1.10.0", "1.9.0").unwrap(), Ordering::Greater);
        assert_eq!(
            precedence("1.0.0+new", "1.0.0+old").unwrap(),
            Ordering::Equal
        );
        assert_eq!(precedence("1.0.0-beta.2", "1.0.0").unwrap(), Ordering::Less);
    }
    #[test]
    fn rejects_windows_and_zip_traversal_names() {
        for name in [
            "../plugin.json",
            "C:/temp/x",
            "foo\\bar",
            "/absolute",
            "AUX.txt",
            "path/file.",
            "path/evil:stream",
            "a//b",
        ] {
            assert!(safe_relative(name).is_err(), "{name}");
        }
        assert_eq!(
            safe_relative("bin/panel.exe").unwrap(),
            Path::new("bin/panel.exe")
        );
    }
    #[test]
    fn sources_parse_and_official_identity_are_stable() {
        assert_eq!(
            repository_name("https://github.com/adoin/ultra-bar-plugins.git").unwrap(),
            OFFICIAL_REPO
        );
        assert!(repository_name("https://evil.test/a/b").is_err());
        assert_eq!(Source::official().id, OFFICIAL_ID);
    }
}
