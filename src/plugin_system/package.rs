use super::model::*;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_TOTAL: u64 = 512 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileDigest {
    pub path: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub format: String,
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub platform: String,
    pub host: String,
    pub files: Vec<FileDigest>,
    #[serde(default)]
    pub build: Option<BuildInfo>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildInfo {
    pub profile: String,
    pub source_revision: String,
    pub sdk_revision: String,
}
pub struct InstallResult {
    pub plugin: Installed,
    pub previous: Option<String>,
}
impl InstallResult {
    pub fn summary(&self) -> String {
        match &self.previous {
            Some(previous) if previous == &self.plugin.version => {
                format!("{} {} 已重新安装", self.plugin.name, self.plugin.version)
            }
            Some(previous) => format!(
                "{} {} → {} 已安装",
                self.plugin.name, previous, self.plugin.version
            ),
            None => format!("{} {} 已安装", self.plugin.name, self.plugin.version),
        }
    }
}
fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub fn sha256(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

struct Staging {
    path: PathBuf,
    base: PathBuf,
}
impl Drop for Staging {
    fn drop(&mut self) {
        if let (Ok(path), Ok(base)) = (self.path.canonicalize(), self.base.canonicalize())
            && path.starts_with(&base)
            && path != base
        {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
struct Lock(windows_sys::Win32::Foundation::HANDLE);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
fn lock() -> Result<Lock> {
    unsafe {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        let handle = CreateMutexW(
            std::ptr::null(),
            0,
            crate::visual::wide("Local\\UltraBar.PluginInstall.v1").as_ptr(),
        );
        ensure!(!handle.is_null(), "无法锁定插件安装");
        let wait = WaitForSingleObject(handle, 5000);
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            CloseHandle(handle);
            anyhow::bail!("另一个插件安装正在进行");
        }
        Ok(Lock(handle))
    }
}

pub fn install_file(path: &Path, force: bool, source: Option<Source>) -> Result<InstallResult> {
    let result = install_at(&Paths::user()?, path, force, source, None)?;
    notify_inventory();
    Ok(result)
}

pub fn install_at(
    paths: &Paths,
    path: &Path,
    force: bool,
    source: Option<Source>,
    expected: Option<&CatalogEntry>,
) -> Result<InstallResult> {
    ultra_bar_native_ui::diagnostics::event(
        "plugins",
        "package.install_requested",
        serde_json::json!({"package":path,"force":force}),
    );
    let _lock = lock()?;
    ensure!(
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(EXTENSION)),
        "插件包后缀应为 .ubplugin"
    );
    ensure!(
        std::fs::metadata(path)?.len() <= MAX_ARCHIVE,
        "插件包超过 256 MiB"
    );
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let count = input.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
    }
    let digest = hex(hasher.finalize());
    if let Some(expected) = expected {
        expected.validate()?;
        ensure!(
            digest.eq_ignore_ascii_case(&expected.sha256),
            "下载包的 SHA-256 与目录不一致"
        );
    }
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    ensure!(archive.len() <= 2048, "插件包文件数量过多");
    let mut names = HashSet::new();
    let mut directories = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name();
        safe_relative(name)?;
        let normalized = name.trim_end_matches('/').to_lowercase();
        ensure!(names.insert(normalized.clone()), "包内存在重复路径");
        if file.is_dir() {
            directories.insert(normalized);
        }
        ensure!(
            file.unix_mode()
                .is_none_or(|mode| matches!(mode & 0xf000, 0 | 0x8000 | 0x4000)),
            "插件包不能包含链接或设备文件"
        );
        ensure!(file.is_dir() || file.size() <= MAX_FILE, "插件包单文件过大");
        total = total.checked_add(file.size()).context("插件包大小溢出")?;
        ensure!(total <= MAX_TOTAL, "插件包解压后超过 512 MiB");
    }
    let metadata = {
        let file = archive
            .by_name("package.json")
            .context("缺少 package.json")?;
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 1024 * 1024, "插件包清单过大");
        serde_json::from_slice::<Package>(&bytes)?
    };
    ensure!(
        metadata.format == "ultra-bar-plugin" && metadata.schema_version == 1,
        "不支持的插件包格式"
    );
    ensure!(
        valid_id(&metadata.id),
        "插件 ID 必须为反向域名，如 com.emssion.schedule-calendar"
    );
    ensure!(
        !metadata.name.trim().is_empty()
            && metadata.name.len() <= 256
            && !metadata.name.chars().any(char::is_control)
            && metadata.version.len() <= 128
            && metadata.host.len() <= 256,
        "插件包元数据无效"
    );
    semver::Version::parse(&metadata.version)?;
    ensure!(metadata.platform == PLATFORM, "插件平台不兼容");
    ensure!(compatible(&metadata.host)?, "插件要求不同的 Ultra Bar 版本");
    ensure!(
        !metadata.files.is_empty() && metadata.files.len() <= 2047,
        "插件包载荷清单无效"
    );
    if let Some(expected) = expected {
        ensure!(
            metadata.id == expected.id && metadata.version == expected.version,
            "插件包 ID 或版本与发布目录不一致"
        );
    }
    let mut registry = paths.registry()?;
    let previous = registry
        .plugins
        .iter()
        .find(|plugin| plugin.id == metadata.id)
        .cloned();
    if !force && let Some(previous) = &previous {
        ensure!(
            precedence(&metadata.version, &previous.version)?.is_gt(),
            "已安装相同或更新版本；本地强制安装可覆盖或降级"
        );
    }
    std::fs::create_dir_all(&paths.root)?;
    let base = paths.root.canonicalize()?;
    let nonce = format!(
        "{:x}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let staging_base = base.join(".staging");
    std::fs::create_dir_all(&staging_base)?;
    ensure!(
        staging_base.canonicalize()?.starts_with(&base),
        "安装暂存目录越界"
    );
    let stage_path = staging_base.join(format!("{}-{nonce}", std::process::id()));
    std::fs::create_dir(&stage_path)?;
    let stage = Staging {
        path: stage_path,
        base: staging_base,
    };
    let mut payload_names = HashSet::new();
    for entry in &metadata.files {
        let relative = safe_relative(&entry.path)?;
        ensure!(
            entry.sha256.len() == 64 && entry.sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "载荷摘要无效"
        );
        ensure!(
            payload_names.insert(entry.path.to_lowercase()),
            "载荷清单重复"
        );
        let zip_name = format!("plugin/{}", entry.path);
        let file = archive
            .by_name(&zip_name)
            .with_context(|| format!("缺少载荷 {}", entry.path))?;
        ensure!(!file.is_dir(), "载荷必须为普通文件");
        let declared = file.size();
        let mut bytes = Vec::new();
        file.take(declared + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == declared && bytes.len() as u64 <= MAX_FILE,
            "载荷大小不一致"
        );
        ensure!(
            sha256(&bytes).eq_ignore_ascii_case(&entry.sha256),
            "载荷 SHA-256 不一致：{}",
            entry.path
        );
        let destination = stage.path.join(relative);
        std::fs::create_dir_all(destination.parent().context("载荷目录无效")?)?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        output.write_all(&bytes)?;
    }
    for name in &names {
        if name == "package.json" || directories.contains(name) {
            continue;
        }
        if let Some(relative) = name.strip_prefix("plugin/") {
            ensure!(payload_names.contains(relative), "插件包包含未声明的文件");
        } else {
            ensure!(name == "plugin", "插件包包含未知根文件");
        }
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(stage.path.join("plugin.json"))?)?;
    ensure!(
        manifest.get("id").and_then(|v| v.as_str()) == Some(&metadata.id)
            && manifest.get("version").and_then(|v| v.as_str()) == Some(&metadata.version),
        "运行清单与包 ID／版本不一致"
    );
    ensure!(
        matches!(
            manifest.get("api_version").and_then(|v| v.as_u64()),
            Some(1..=3)
        ),
        "不支持的插件 API 版本"
    );
    let entry = manifest
        .get("entry")
        .and_then(|v| v.as_str())
        .context("缺少 Rune 入口")?;
    ensure!(
        stage.path.join(safe_relative(entry)?).is_file(),
        "Rune 入口不存在"
    );
    if let Some(panel) = manifest.get("panel") {
        ensure!(
            manifest
                .get("capabilities")
                .and_then(|v| v.as_array())
                .is_some_and(|caps| caps.iter().any(|cap| cap.as_str() == Some("panel"))),
            "原生面板缺少 panel 能力声明"
        );
        let runtime = panel
            .get("runtime")
            .and_then(|v| v.as_str())
            .context("缺少面板运行指针")?;
        let pointer = stage.path.join(safe_relative(runtime)?);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&pointer)?)?;
        let executable = value
            .get("executable")
            .and_then(|v| v.as_str())
            .context("缺少面板 EXE")?;
        let binary = pointer
            .parent()
            .context("无效的面板指针")?
            .join(executable)
            .canonicalize()?;
        ensure!(
            binary.starts_with(stage.path.canonicalize()?)
                && binary
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
            "面板 EXE 越界"
        );
        validate_pe(&binary)?;
    }
    crate::plugins::validate_directory(&stage.path)?;
    let installed = Installed {
        id: metadata.id,
        name: metadata.name,
        version: metadata.version,
        digest,
        generation: nonce,
        source,
        installed_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        enabled: previous.as_ref().is_none_or(|plugin| plugin.enabled),
    };
    let destination = paths.directory(&installed)?;
    std::fs::create_dir_all(destination.parent().context("安装目录无效")?)?;
    ensure!(
        destination
            .parent()
            .unwrap()
            .canonicalize()?
            .starts_with(&base),
        "安装目录越界"
    );
    std::fs::rename(&stage.path, &destination)?;
    registry.plugins.retain(|plugin| plugin.id != installed.id);
    registry.plugins.push(installed.clone());
    paths.save("installed.json", &registry)?;
    Ok(InstallResult {
        plugin: installed,
        previous: previous.map(|plugin| plugin.version),
    })
}
fn validate_pe(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header)
        .context("面板不是有效的 Windows 程序")?;
    ensure!(&header[..2] == b"MZ", "面板不是 Windows PE 程序");
    let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as u64;
    ensure!(
        offset < file.metadata()?.len().saturating_sub(6),
        "面板 PE 头无效"
    );
    file.seek(SeekFrom::Start(offset))?;
    let mut pe = [0u8; 6];
    file.read_exact(&mut pe)?;
    ensure!(
        &pe[..4] == b"PE\0\0" && u16::from_le_bytes([pe[4], pe[5]]) == 0x8664,
        "面板必须为 Windows x86-64 程序"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Run with ULTRA_BAR_PACKAGE_TEST_FILE pointing to the actual built .ubplugin"]
    fn installs_the_actual_cross_language_packaging_output_without_touching_user_data() {
        let fixture = Fixture::new();
        let paths = Paths {
            root: fixture.0.join("actual-store"),
        };
        let input = std::env::var_os("ULTRA_BAR_PACKAGE_TEST_FILE")
            .expect("set ULTRA_BAR_PACKAGE_TEST_FILE");
        let result = install_at(&paths, Path::new(&input), true, None, None).unwrap();
        assert_eq!(result.plugin.id, "com.emssion.schedule-calendar");
        assert!(
            paths
                .directory(&result.plugin)
                .unwrap()
                .join("bin/schedule-calendar.exe")
                .exists()
        );
    }
    use zip::{ZipWriter, write::SimpleFileOptions};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let base = PathBuf::from("D:/tmp/ultra-bar-ecosystem-tests");
            let directory = base.join(format!(
                "{}-{:x}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let (Ok(root), Ok(base)) = (
                self.0.canonicalize(),
                Path::new("D:/tmp/ultra-bar-ecosystem-tests").canonicalize(),
            ) && root.starts_with(&base)
                && root != base
            {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }
    fn bundle(root: &Path, version: &str, corrupt: bool) -> PathBuf {
        bundle_script(
            root,
            version,
            corrupt,
            b"pub fn render(clicks) { (\"{time}\", \"none\") }",
        )
    }
    fn bundle_script(root: &Path, version: &str, corrupt: bool, script: &[u8]) -> PathBuf {
        let manifest = format!(
            r#"{{"id":"com.example.clock","name":"Clock","version":"{version}","api_version":2,"entry":"main.rn","width":120,"slot":"clock"}}"#
        );
        let package = Package {
            format: "ultra-bar-plugin".into(),
            schema_version: 1,
            id: "com.example.clock".into(),
            name: "Clock".into(),
            version: version.into(),
            platform: PLATFORM.into(),
            host: ">=1.0.0,<2.0.0".into(),
            build: None,
            files: vec![
                FileDigest {
                    path: "plugin.json".into(),
                    sha256: sha256(manifest.as_bytes()),
                },
                FileDigest {
                    path: "main.rn".into(),
                    sha256: if corrupt {
                        "0".repeat(64)
                    } else {
                        sha256(script)
                    },
                },
            ],
        };
        let path = root.join(format!("{version}.ubplugin"));
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        for (name, bytes) in [
            ("package.json", serde_json::to_vec(&package).unwrap()),
            ("plugin/plugin.json", manifest.into_bytes()),
            ("plugin/main.rn", script.to_vec()),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }
    #[test]
    fn atomic_install_semver_force_and_corrupt_rollback() {
        let fixture = Fixture::new();
        let paths = Paths {
            root: fixture.0.join("state"),
        };
        let first = bundle(&fixture.0, "1.9.0", false);
        install_at(&paths, &first, false, None, None).unwrap();
        let newer = bundle(&fixture.0, "1.10.0", false);
        install_at(&paths, &newer, false, None, None).unwrap();
        assert!(install_at(&paths, &first, false, None, None).is_err());
        install_at(&paths, &first, true, None, None).unwrap();
        let previous_directory = paths
            .directory(&paths.registry().unwrap().plugins[0])
            .unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(previous_directory.join("main.rn"))
            .unwrap();
        install_at(&paths, &first, true, None, None).unwrap();
        assert_ne!(
            paths
                .directory(&paths.registry().unwrap().plugins[0])
                .unwrap(),
            previous_directory
        );
        assert!(previous_directory.join("main.rn").exists());
        drop(held);
        let bad = bundle(&fixture.0, "2.0.0", true);
        assert!(install_at(&paths, &bad, true, None, None).is_err());
        let broken = bundle_script(&fixture.0, "2.1.0", false, b"pub fn render( { broken");
        assert!(install_at(&paths, &broken, true, None, None).is_err());
        let registry = paths.registry().unwrap();
        assert_eq!(registry.plugins[0].version, "1.9.0");
        assert!(
            paths
                .directory(&registry.plugins[0])
                .unwrap()
                .join("main.rn")
                .exists()
        );
    }
}
