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
#[derive(Clone, Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Serialize, Deserialize)]
struct Cache {
    at: u64,
    entries: Vec<Candidate>,
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
        && now.saturating_sub(cache.at) < 900
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
        releases.extend(batch);
        if done {
            break;
        }
    }
    let entries = collect_releases(&actual, releases, |asset| {
        ensure!(asset.size <= 2 * 1024 * 1024, "发布目录过大");
        let bytes = asset_bytes(&actual, asset, 2 * 1024 * 1024)?;
        Ok(serde_json::from_slice::<Catalog>(&bytes)?)
    })?;
    let pending = file.with_extension("json.new");
    std::fs::write(
        &pending,
        serde_json::to_vec(&Cache {
            at: now,
            entries: entries.clone(),
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
        candidate.download_url.starts_with(&format!(
            "https://github.com/{}/releases/download/",
            candidate.source.full_name
        )),
        "发布下载地址不属于此仓库"
    );
    let asset = Asset {
        id: candidate.asset_id,
        name: candidate.entry.asset.clone(),
        size: candidate.size,
        browser_download_url: candidate.download_url.clone(),
        digest: None,
    };
    let bytes = asset_bytes(&candidate.source, &asset, 256 * 1024 * 1024)?;
    ensure!(
        sha256(&bytes).eq_ignore_ascii_case(&candidate.entry.sha256),
        "发布产物 SHA-256 不一致"
    );
    let directory = paths.root.join("downloads");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!(
        "{}.ubplugin",
        candidate.entry.sha256.to_lowercase()
    ));
    std::fs::write(&path, bytes)?;
    Ok(path)
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
