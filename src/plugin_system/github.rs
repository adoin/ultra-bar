use super::{model::*, package::sha256};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    os::windows::process::CommandExt,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
struct Repository {
    id: u64,
    full_name: String,
    name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppRelease {
    pub version: String,
    pub tag: String,
    installer: Asset,
    checksum: Asset,
}
pub const APP_REPOSITORY_ID: u64 = 1452115414;
fn app_source() -> Result<Source> {
    let source = Source {
        id: format!("github:{APP_REPOSITORY_ID}"),
        full_name: "adoin/ultra-bar".into(),
        name: "Ultra Bar".into(),
    };
    let repo = by_id(&source)?;
    Ok(Source {
        full_name: repo.full_name,
        ..source
    })
}
fn app_release(release: Release) -> Result<AppRelease> {
    ensure!(!release.draft && !release.prerelease, "不使用预览版本更新");
    let version = release
        .tag_name
        .strip_prefix('v')
        .context("本体发布标签无效")?;
    let parsed = semver::Version::parse(version)?;
    ensure!(
        parsed.pre.is_empty() && parsed.build.is_empty(),
        "安装包版本必须为稳定版本"
    );
    let name = format!("UltraBar-{version}-windows-x86_64-setup.exe");
    let installer = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .context("新版安装包尚未就绪")?
        .clone();
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == format!("{name}.sha256"))
        .context("新版安装包缺少校验文件")?
        .clone();
    ensure!(
        installer.size > 0
            && installer.size <= 256 * 1024 * 1024
            && checksum.size > 0
            && checksum.size <= 4096,
        "安装包产物信息无效"
    );
    Ok(AppRelease {
        version: version.into(),
        tag: release.tag_name,
        installer,
        checksum,
    })
}
pub fn latest_app() -> Result<AppRelease> {
    let source = app_source()?;
    app_release(serde_json::from_slice(&request(
        &format!("repos/{}/releases/latest", source.full_name),
        false,
        2 * 1024 * 1024,
    )?)?)
}
pub fn download_app(
    release: &AppRelease,
    directory: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let source = app_source()?;
    ensure!(
        release.tag == format!("v{}", semver::Version::parse(&release.version)?),
        "更新版本无效"
    );
    let fresh = app_release(serde_json::from_slice(&request(
        &format!("repos/{}/releases/tags/{}", source.full_name, release.tag),
        false,
        2 * 1024 * 1024,
    )?)?)?;
    ensure!(
        fresh.version == release.version && fresh.installer.id == release.installer.id,
        "发布产物已变更，请重新检查更新"
    );
    let checksum = String::from_utf8(asset_bytes(&source, &fresh.checksum, 4096)?)?;
    let parts: Vec<_> = checksum.split_whitespace().collect();
    ensure!(
        parts.len() == 2
            && parts[0].len() == 64
            && parts[0].bytes().all(|c| c.is_ascii_hexdigit())
            && parts[1] == fresh.installer.name,
        "安装包校验文件无效"
    );
    std::fs::create_dir_all(directory)?;
    let file = directory.join(&fresh.installer.name);
    if file.exists()
        && std::fs::metadata(&file)?.len() == fresh.installer.size
        && sha256(&std::fs::read(&file)?).eq_ignore_ascii_case(parts[0])
    {
        verify_installer(&file, &fresh.version)?;
        return Ok(file);
    }
    let bytes = asset_bytes(&source, &fresh.installer, 256 * 1024 * 1024)?;
    let hash = sha256(&bytes);
    ensure!(hash.eq_ignore_ascii_case(parts[0]), "安装包 SHA-256 不一致");
    if let Some(digest) = &fresh.installer.digest {
        ensure!(
            digest.eq_ignore_ascii_case(&format!("sha256:{hash}")),
            "GitHub 安装包摘要不一致"
        );
    }
    let file = write_download(directory, &fresh.installer.name, &bytes)?;
    verify_installer(&file, &fresh.version)?;
    Ok(file)
}
pub fn verify_installer(path: &std::path::Path, version: &str) -> Result<()> {
    use std::ptr::{null_mut, read_unaligned};
    use windows_sys::Win32::Storage::FileSystem::*;
    let expected = semver::Version::parse(version)?;
    ensure!(
        expected.major <= 65535 && expected.minor <= 65535 && expected.patch <= 65535,
        "安装包版本范围无效"
    );
    unsafe {
        let name = crate::visual::wide(&path.to_string_lossy());
        let mut handle = 0;
        let size = GetFileVersionInfoSizeW(name.as_ptr(), &mut handle);
        ensure!(size > 0 && size <= 4 * 1024 * 1024, "安装包版本资源无效");
        let mut bytes = vec![0u8; size as usize];
        ensure!(
            GetFileVersionInfoW(name.as_ptr(), 0, size, bytes.as_mut_ptr().cast()) != 0,
            "无法读取安装包版本"
        );
        let mut pointer = null_mut();
        let mut length = 0;
        ensure!(
            VerQueryValueW(
                bytes.as_ptr().cast(),
                crate::visual::wide("\\").as_ptr(),
                &mut pointer,
                &mut length
            ) != 0
                && !pointer.is_null()
                && length as usize >= std::mem::size_of::<VS_FIXEDFILEINFO>(),
            "安装包版本信息缺失"
        );
        let info = read_unaligned(pointer as *const VS_FIXEDFILEINFO);
        ensure!(
            info.dwSignature == 0xfeef04bd
                && info.dwProductVersionMS >> 16 == expected.major as u32
                && info.dwProductVersionMS & 0xffff == expected.minor as u32
                && info.dwProductVersionLS >> 16 == expected.patch as u32
                && info.dwProductVersionLS & 0xffff == 0,
            "安装包资源版本不匹配"
        );
        ensure!(
            VerQueryValueW(
                bytes.as_ptr().cast(),
                crate::visual::wide("\\VarFileInfo\\Translation").as_ptr(),
                &mut pointer,
                &mut length
            ) != 0
                && length >= 4,
            "安装包身份信息缺失"
        );
        let mut valid = false;
        for index in 0..length as usize / 4 {
            let word = pointer as *const u16;
            let language = read_unaligned(word.add(index * 2));
            let page = read_unaligned(word.add(index * 2 + 1));
            let mut values = Vec::new();
            for field in ["ProductName", "FileDescription"] {
                let mut value = null_mut();
                let mut count = 0;
                let query = crate::visual::wide(&format!(
                    "\\StringFileInfo\\{language:04x}{page:04x}\\{field}"
                ));
                if VerQueryValueW(
                    bytes.as_ptr().cast(),
                    query.as_ptr(),
                    &mut value,
                    &mut count,
                ) == 0
                    || value.is_null()
                    || count > size / 2
                {
                    continue;
                }
                let text: Vec<u16> = (0..count as usize)
                    .map(|i| read_unaligned((value as *const u16).add(i)))
                    .take_while(|c| *c != 0)
                    .collect();
                values.push(String::from_utf16_lossy(&text).trim().to_owned());
            }
            valid |= values == ["Ultra Bar", "Ultra Bar Setup"];
        }
        ensure!(valid, "安装包不是 Ultra Bar 正式安装程序");
    }
    Ok(())
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct MarketCatalog {
    schema_version: u32,
    repository_id: u64,
    plugins: Vec<MarketEntry>,
}
#[derive(Deserialize)]
struct MarketEntry {
    #[serde(flatten)]
    entry: CatalogEntry,
    asset_id: u64,
    download_url: String,
    size: u64,
    release_tag: String,
}
fn collect_market(source: &Source, catalog: MarketCatalog) -> Result<Vec<Candidate>> {
    ensure!(catalog.schema_version == 2, "不支持的市场目录版本");
    ensure!(
        source.id == format!("github:{}", catalog.repository_id),
        "市场目录仓库身份不匹配"
    );
    ensure!(catalog.plugins.len() <= 4096, "市场目录过大");
    let mut ids = std::collections::HashSet::new();
    let mut entries = vec![];
    for item in catalog.plugins {
        ensure!(ids.insert(item.entry.id.clone()), "市场目录包含重复插件 ID");
        let tag = format!("{}/v{}", item.entry.id, item.entry.version);
        ensure!(item.release_tag == tag, "插件发布标签与版本不一致");
        let rest = item
            .download_url
            .strip_prefix("https://github.com/")
            .context("插件下载地址不是 GitHub HTTPS 地址")?;
        let mut parts = rest.splitn(3, '/');
        let repository = format!(
            "{}/{}",
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default()
        );
        repository_name(&repository)?;
        let tail = parts.next().unwrap_or_default();
        ensure!(
            tail == format!("releases/download/{tag}/{}", item.entry.asset)
                || tail
                    == format!(
                        "releases/download/{}/{}",
                        tag.replace('/', "%2F"),
                        item.entry.asset
                    ),
            "插件下载地址与仓库、版本不一致"
        );
        ensure!(
            item.asset_id != 0 && item.size > 0 && item.size <= 256 * 1024 * 1024,
            "插件产物信息无效"
        );
        if item.entry.validate().is_err()
            || !semver::Version::parse(&item.entry.version)?.pre.is_empty()
        {
            continue;
        }
        entries.push(Candidate {
            entry: item.entry,
            source: source.clone(),
            asset_id: item.asset_id,
            download_url: item.download_url,
            size: item.size,
            release_tag: tag,
        });
    }
    entries.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    Ok(entries)
}
#[derive(Serialize, Deserialize)]
struct Cache {
    at: u64,
    entries: Vec<Candidate>,
    #[serde(default)]
    host_version: String,
}
fn cache_fresh(cache: &Cache, now: u64) -> bool {
    cache.host_version == env!("CARGO_PKG_VERSION")
        && now.checked_sub(cache.at).is_some_and(|age| age < 900)
}
static CACHE_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn write_download(
    directory: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::create_dir_all(directory)?;
    let file = directory.join(name);
    let pending = directory.join(format!(
        ".{name}.{}.{}.partial",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        std::fs::write(&pending, bytes)?;
        std::fs::rename(&pending, &file)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&pending);
    }
    result?;
    Ok(file)
}

fn request(endpoint: &str, binary: bool, limit: usize) -> Result<Vec<u8>> {
    let mut command = Command::new("gh");
    command.creation_flags(0x08000000).args([
        "api",
        endpoint,
        "-H",
        if binary {
            "Accept: application/octet-stream"
        } else {
            "Accept: application/vnd.github+json"
        },
        "-H",
        "X-GitHub-Api-Version: 2026-03-10",
    ]);
    match command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let mut bytes = Vec::new();
            child
                .stdout
                .take()
                .context("GitHub CLI 输出不可用")?
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                let _ = child.kill();
                let _ = child.wait();
                bail!("GitHub 响应超过大小限制");
            }
            let output = child.wait_with_output()?;
            ensure!(
                output.status.success(),
                "GitHub 请求失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !binary,
                "下载私有或二进制发布产物需要 GitHub CLI；请安装 gh 并登录"
            );
            http(
                &format!(
                    "https://api.github.com/{}",
                    endpoint.trim_start_matches('/')
                ),
                limit,
            )
        }
        Err(error) => Err(error.into()),
    }
}

pub fn resolve(input: &str) -> Result<Source> {
    let name = repository_name(input)?;
    let repository: Repository =
        serde_json::from_slice(&request(&format!("repos/{name}"), false, 2 * 1024 * 1024)?)?;
    Ok(Source {
        id: format!("github:{}", repository.id),
        full_name: repository.full_name,
        name: repository.name,
    })
}
fn by_id(source: &Source) -> Result<Repository> {
    let id = source
        .id
        .strip_prefix("github:")
        .context("不支持的仓库 ID")?
        .parse::<u64>()?;
    let repository: Repository = serde_json::from_slice(&request(
        &format!("repositories/{id}"),
        false,
        2 * 1024 * 1024,
    )?)?;
    ensure!(repository.id == id, "仓库身份不匹配");
    Ok(repository)
}

pub fn catalog(paths: &Paths, source: &Source, force: bool) -> Result<Vec<Candidate>> {
    let cache_dir = paths.root.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    let file = cache_dir.join(format!("{}.json", sha256(source.id.as_bytes())));
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if !force
        && file.exists()
        && let Ok(cache) = serde_json::from_slice::<Cache>(&std::fs::read(&file)?)
        && cache_fresh(&cache, now)
    {
        return Ok(cache.entries);
    }
    let repository = by_id(source)?;
    let actual = Source {
        id: source.id.clone(),
        full_name: repository.full_name.clone(),
        name: if source.official_source() {
            "官方插件库".into()
        } else {
            source.name.clone()
        },
    };
    let mut releases = vec![];
    let mut market = None;
    for page in 1..=10 {
        let batch: Vec<Release> = serde_json::from_slice(&request(
            &format!(
                "repos/{}/releases?per_page=100&page={page}",
                repository.full_name
            ),
            false,
            8 * 1024 * 1024,
        )?)?;
        let done = batch.len() < 100;
        if let Some(index) = batch
            .iter()
            .filter(|r| !r.draft && !r.prerelease)
            .flat_map(|r| r.assets.iter())
            .find(|a| a.name == "plugin-market.json")
        {
            ensure!(index.size <= 2 * 1024 * 1024, "市场目录过大");
            let bytes = asset_bytes(&actual, index, 2 * 1024 * 1024)?;
            market = Some(collect_market(&actual, serde_json::from_slice(&bytes)?)?);
            break;
        }
        releases.extend(batch);
        if done {
            break;
        }
    }
    let entries = if let Some(entries) = market {
        entries
    } else {
        collect_releases(&actual, releases, |asset| {
            ensure!(asset.size <= 2 * 1024 * 1024, "发布目录过大");
            let bytes = asset_bytes(&actual, asset, 2 * 1024 * 1024)?;
            Ok(serde_json::from_slice::<Catalog>(&bytes)?)
        })?
    };
    let _guard = CACHE_WRITE
        .lock()
        .map_err(|_| anyhow::anyhow!("更新目录缓存暂不可用"))?;
    let pending = file.with_extension("json.new");
    std::fs::write(
        &pending,
        serde_json::to_vec(&Cache {
            at: now,
            entries: entries.clone(),
            host_version: env!("CARGO_PKG_VERSION").into(),
        })?,
    )?;
    std::fs::rename(pending, file)?;
    Ok(entries)
}

fn collect_releases(
    source: &Source,
    releases: Vec<Release>,
    mut read: impl FnMut(&Asset) -> Result<Catalog>,
) -> Result<Vec<Candidate>> {
    let mut latest: HashMap<String, Candidate> = HashMap::new();
    for release in releases {
        if release.draft || release.prerelease {
            continue;
        }
        let tagged = release
            .tag_name
            .rsplit_once("/v")
            .filter(|(id, version)| valid_id(id) && semver::Version::parse(version).is_ok());
        if let Some((id, version)) = tagged
            && latest.get(id).is_some_and(|entry| {
                precedence(&entry.entry.version, version).is_ok_and(|order| !order.is_lt())
            })
        {
            continue;
        }
        let entries = if let Some(index) = release
            .assets
            .iter()
            .find(|asset| asset.name == "plugin-catalog.json")
        {
            let catalog = read(index)?;
            ensure!(catalog.schema_version == 1, "不支持的发布目录版本");
            catalog.plugins
        } else if let Some((id, version)) = release.tag_name.rsplit_once("/v") {
            if !valid_id(id) {
                continue;
            }
            let Ok(parsed) = semver::Version::parse(version) else {
                continue;
            };
            if !parsed.pre.is_empty() {
                continue;
            }
            release
                .assets
                .iter()
                .filter(|asset| asset.name.ends_with("windows-x86_64.ubplugin"))
                .filter_map(|asset| {
                    let digest = asset.digest.as_ref()?.strip_prefix("sha256:")?;
                    Some(CatalogEntry {
                        id: id.into(),
                        name: id.into(),
                        version: version.into(),
                        platform: PLATFORM.into(),
                        host: "*".into(),
                        asset: asset.name.clone(),
                        sha256: digest.into(),
                        description: String::new(),
                    })
                })
                .collect()
        } else {
            continue;
        };
        for entry in entries {
            if let Some((id, version)) = tagged
                && (entry.id != id || entry.version != version)
            {
                continue;
            }
            if entry.validate().is_err() || !semver::Version::parse(&entry.version)?.pre.is_empty()
            {
                continue;
            }
            let Some(asset) = release
                .assets
                .iter()
                .find(|asset| asset.name == entry.asset)
            else {
                continue;
            };
            if asset.size > 256 * 1024 * 1024 {
                continue;
            }
            if latest.get(&entry.id).is_some_and(|existing| {
                precedence(&entry.version, &existing.entry.version)
                    .is_ok_and(|order| !order.is_gt())
            }) {
                continue;
            }
            latest.insert(
                entry.id.clone(),
                Candidate {
                    entry,
                    source: source.clone(),
                    asset_id: asset.id,
                    download_url: asset.browser_download_url.clone(),
                    size: asset.size,
                    release_tag: release.tag_name.clone(),
                },
            );
        }
    }
    let mut entries: Vec<_> = latest.into_values().collect();
    entries.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    Ok(entries)
}
fn asset_bytes(source: &Source, asset: &Asset, limit: usize) -> Result<Vec<u8>> {
    ensure!(asset.size as usize <= limit, "发布产物过大");
    // Prefer authenticated gh for private repositories. Public installs also work
    // on end-user machines without a development CLI.
    let available = Command::new("gh")
        .creation_flags(0x08000000)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    let bytes = if available {
        request(
            &format!("repos/{}/releases/assets/{}", source.full_name, asset.id),
            true,
            limit,
        )?
    } else {
        http(&asset.browser_download_url, limit)?
    };
    ensure!(bytes.len() as u64 == asset.size, "发布产物大小不一致");
    Ok(bytes)
}
pub fn download(candidate: &Candidate, paths: &Paths) -> Result<std::path::PathBuf> {
    candidate.entry.validate()?;
    ensure!(
        by_id(&candidate.source)?
            .full_name
            .eq_ignore_ascii_case(&candidate.source.full_name),
        "插件仓库地址已变化，请重新检查更新"
    );
    // Asset IDs remain valid when a repository is renamed. Resolve the asset
    // through that repository again rather than executing a stale catalog URL.
    let asset: Asset = serde_json::from_slice(&request(
        &format!(
            "repos/{}/releases/assets/{}",
            candidate.source.full_name, candidate.asset_id
        ),
        false,
        2 * 1024 * 1024,
    )?)?;
    ensure!(
        asset.id == candidate.asset_id
            && asset.name == candidate.entry.asset
            && asset.size == candidate.size,
        "插件产物信息已变化，请重新检查"
    );
    if let Some(digest) = &asset.digest {
        ensure!(
            digest.eq_ignore_ascii_case(&format!("sha256:{}", candidate.entry.sha256)),
            "GitHub 插件摘要与目录不一致"
        );
    }
    let bytes = asset_bytes(&candidate.source, &asset, 256 * 1024 * 1024)?;
    ensure!(
        sha256(&bytes).eq_ignore_ascii_case(&candidate.entry.sha256),
        "发布产物 SHA-256 不一致"
    );
    let directory = paths.root.join("downloads");
    std::fs::create_dir_all(&directory)?;
    write_download(
        &directory,
        &format!("{}.ubplugin", candidate.entry.sha256.to_lowercase()),
        &bytes,
    )
}

fn http(url: &str, limit: usize) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Networking::WinHttp::*;
    struct Handle(*mut std::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
    let rest = url.strip_prefix("https://").context("仅支持 HTTPS")?;
    let (host, path) = rest
        .split_once('/')
        .map(|(host, path)| (host, format!("/{path}")))
        .unwrap_or((rest, "/".into()));
    ensure!(
        host == "api.github.com"
            || host == "github.com"
            || host.ends_with(".githubusercontent.com"),
        "不支持的 GitHub 下载主机"
    );
    unsafe {
        let session = Handle(WinHttpOpen(
            crate::visual::wide("UltraBar/0.1 PluginCatalog").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ));
        ensure!(!session.0.is_null(), "无法初始化 HTTPS");
        WinHttpSetTimeouts(session.0, 5000, 5000, 15000, 30000);
        let connection = Handle(WinHttpConnect(
            session.0,
            crate::visual::wide(host).as_ptr(),
            443,
            0,
        ));
        ensure!(!connection.0.is_null(), "无法连接 GitHub");
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            crate::visual::wide("GET").as_ptr(),
            crate::visual::wide(&path).as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ));
        ensure!(!request.0.is_null(), "无法创建 HTTPS 请求");
        let headers = crate::visual::wide(
            "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2026-03-10\r\n",
        );
        ensure!(
            WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                (headers.len() - 1) as _,
                null(),
                0,
                0,
                0
            ) != 0
                && WinHttpReceiveResponse(request.0, null_mut()) != 0,
            "GitHub HTTPS 请求失败"
        );
        let mut code = 0u32;
        let mut size = 4;
        ensure!(
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut code as *mut u32).cast(),
                &mut size,
                null_mut()
            ) != 0
                && code == 200,
            "GitHub 返回 HTTP {code}"
        );
        let mut output = Vec::new();
        loop {
            let mut buffer = [0u8; 65536];
            let mut read = 0;
            ensure!(
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as _,
                    &mut read
                ) != 0,
                "GitHub 下载中断"
            );
            if read == 0 {
                break;
            }
            ensure!(output.len() + read as usize <= limit, "GitHub 响应过大");
            output.extend_from_slice(&buffer[..read as usize]);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_cache_expires_after_host_changes_or_clock_rollback() {
        let mut cache = Cache {
            at: 1000,
            entries: vec![],
            host_version: env!("CARGO_PKG_VERSION").into(),
        };
        assert!(cache_fresh(&cache, 1100));
        assert!(!cache_fresh(&cache, 999));
        assert!(!cache_fresh(&cache, 1900));
        cache.host_version = "0.0.0".into();
        assert!(!cache_fresh(&cache, 1100));
    }
    #[test]
    fn app_release_requires_a_stable_tag_matching_installer_and_checksum() {
        let release = serde_json::json!({"tag_name":"v2.0.0","draft":false,"prerelease":false,"assets":[{"id":1,"name":"UltraBar-2.0.0-windows-x86_64-setup.exe","size":100,"browser_download_url":"https://github.com/adoin/ultra-bar/releases/download/v2.0.0/setup.exe"},{"id":2,"name":"UltraBar-2.0.0-windows-x86_64-setup.exe.sha256","size":100,"browser_download_url":"https://github.com/adoin/ultra-bar/releases/download/v2.0.0/setup.exe.sha256"}]});
        assert_eq!(
            app_release(serde_json::from_value(release.clone()).unwrap())
                .unwrap()
                .version,
            "2.0.0"
        );
        for (key, value) in [
            ("draft", serde_json::json!(true)),
            ("prerelease", serde_json::json!(true)),
            ("tag_name", serde_json::json!("v2.0.0-rc.1")),
        ] {
            let mut bad = release.clone();
            bad[key] = value;
            assert!(app_release(serde_json::from_value(bad).unwrap()).is_err());
        }
        let mut missing = release.clone();
        missing["assets"].as_array_mut().unwrap().pop();
        assert!(app_release(serde_json::from_value(missing).unwrap()).is_err());
    }
    #[test]
    fn installer_validation_rejects_an_application_binary() {
        assert!(
            verify_installer(&std::env::current_exe().unwrap(), env!("CARGO_PKG_VERSION")).is_err()
        );
    }
    #[test]
    fn concurrent_downloads_publish_complete_files_without_partial_debris() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::path::PathBuf::from(format!(
            "D:/tmp/ultra-bar-update-tests/{}-{nonce}",
            std::process::id()
        ));
        let first = directory.clone();
        let second = directory.clone();
        let a = std::thread::spawn(move || {
            write_download(&first, "verified.ubplugin", b"verified complete bytes")
        });
        let b = std::thread::spawn(move || {
            write_download(&second, "verified.ubplugin", b"verified complete bytes")
        });
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
        assert_eq!(
            std::fs::read(directory.join("verified.ubplugin")).unwrap(),
            b"verified complete bytes"
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(directory.join("verified.ubplugin")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
    fn market_fixture() -> serde_json::Value {
        serde_json::json!({"schema_version":2,"repository_id":1452116417,"plugins":[{
            "id":"com.example.calendar","name":"Calendar","version":"1.0.0","platform":PLATFORM,
            "host":">=1.0.0,<2.0.0","asset":"calendar.ubplugin","sha256":"a".repeat(64),
            "asset_id":123,"size":64,"release_tag":"com.example.calendar/v1.0.0",
            "download_url":"https://github.com/adoin/ultra-bar-plugins/releases/download/com.example.calendar/v1.0.0/calendar.ubplugin"
        }]})
    }
    #[test]
    fn market_snapshot_reuses_an_older_release_asset_without_attaching_it_again() {
        let entries = collect_market(
            &Source::official(),
            serde_json::from_value(market_fixture()).unwrap(),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].asset_id, 123);
        assert_eq!(entries[0].release_tag, "com.example.calendar/v1.0.0");
        assert!(
            entries[0]
                .download_url
                .ends_with("/com.example.calendar/v1.0.0/calendar.ubplugin")
        );
    }
    #[test]
    fn market_rejects_foreign_sources_duplicate_ids_and_mismatched_versions() {
        for field in ["repository_id", "download_url", "release_tag", "asset_id"] {
            let mut value = market_fixture();
            match field {
                "repository_id" => value[field] = 1.into(),
                "asset_id" => value["plugins"][0][field] = 0.into(),
                "release_tag" => value["plugins"][0][field] = "com.example.calendar/v2.0.0".into(),
                _ => value["plugins"][0][field] = "https://example.com/calendar.ubplugin".into(),
            }
            assert!(
                collect_market(&Source::official(), serde_json::from_value(value).unwrap())
                    .is_err()
            );
        }
        let mut value = market_fixture();
        let entry = value["plugins"][0].clone();
        value["plugins"].as_array_mut().unwrap().push(entry);
        assert!(
            collect_market(&Source::official(), serde_json::from_value(value).unwrap()).is_err()
        );
    }
    #[test]
    fn releases_choose_latest_per_id_and_ignore_drafts_prereleases() {
        let source = Source::official();
        let make = |version: &str, draft, pre| Release {
            tag_name: format!("com.example.calendar/v{version}"),
            draft,
            prerelease: pre,
            assets: vec![Asset {
                id: 1,
                name: "com.example.calendar--windows-x86_64.ubplugin".into(),
                size: 1,
                browser_download_url: "https://github.com/x/y/releases/download/tag/a.ubplugin"
                    .into(),
                digest: Some(format!("sha256:{}", "a".repeat(64))),
            }],
        };
        let mut second_plugin = make("0.5.0", false, false);
        second_plugin.tag_name = "com.example.timer/v0.5.0".into();
        let entries = collect_releases(
            &source,
            vec![
                make("1.9.0", false, false),
                make("1.10.0", false, false),
                make("9.0.0", true, false),
                make("2.0.0-beta.1", false, true),
                second_plugin,
            ],
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .find(|candidate| candidate.entry.id == "com.example.calendar")
                .unwrap()
                .entry
                .version,
            "1.10.0"
        );
        assert_eq!(
            entries
                .iter()
                .find(|candidate| candidate.entry.id == "com.example.timer")
                .unwrap()
                .entry
                .version,
            "0.5.0"
        );
    }
}
