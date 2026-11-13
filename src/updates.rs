//! Low-frequency background checks. Download/install only follows a UI action.
use crate::plugin_system::{github, model::*, package};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
pub const DAILY: Duration = Duration::from_secs(24 * 60 * 60);
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub main: Option<github::AppRelease>,
    pub plugins: HashMap<String, Candidate>,
    pub checking: bool,
    pub busy: Option<String>,
    pub status: String,
    pub installer: Option<PathBuf>,
}
enum Request {
    Check(bool),
    Inventory,
    Enable(bool),
    Plugin(Box<Candidate>),
    DownloadMain(Box<github::AppRelease>),
    Installed,
    Stop,
}
pub struct Updates {
    sender: SyncSender<Request>,
    receive: Receiver<Arc<Snapshot>>,
    pub snapshot: Arc<Snapshot>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Cache {
    at: u64,
    main: Option<github::AppRelease>,
    candidates: Vec<Candidate>,
    #[serde(default)]
    host_version: String,
    #[serde(default)]
    sources: Vec<String>,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// Custom installations stay on their selected repository. Otherwise a custom
/// same-ID candidate outranks official; an older custom fork never downgrades.
pub fn select(installed: &Installed, candidates: &[Candidate]) -> Option<Candidate> {
    let same: Vec<_> = candidates
        .iter()
        .filter(|c| {
            c.entry.id == installed.id
                && c.entry.validate().is_ok()
                && semver::Version::parse(&c.entry.version).is_ok_and(|v| v.pre.is_empty())
        })
        .collect();
    let custom_source = installed.source.as_ref().filter(|s| !s.official_source());
    let has_custom = same.iter().any(|c| !c.source.official_source());
    let candidate = same
        .into_iter()
        .filter(|c| {
            if let Some(source) = custom_source {
                c.source.id == source.id
            } else {
                !has_custom || !c.source.official_source()
            }
        })
        .max_by(|a, b| {
            precedence(&a.entry.version, &b.entry.version).unwrap_or(std::cmp::Ordering::Equal)
        })?;
    if custom_source.is_some() {
        return eligible_update(installed, candidate).then(|| candidate.clone());
    }
    precedence(&candidate.entry.version, &installed.version)
        .ok()
        .filter(|order| order.is_gt())?;
    Some(candidate.clone())
}
fn inventory(paths: &Paths, all: &[Candidate]) -> Result<HashMap<String, Candidate>> {
    let sources = paths.sources()?;
    let allowed: Vec<_> = all
        .iter()
        .filter(|c| sources.repositories.iter().any(|s| s.id == c.source.id))
        .cloned()
        .collect();
    Ok(paths
        .registry()?
        .plugins
        .iter()
        .filter_map(|p| select(p, &allowed).map(|c| (p.id.clone(), c)))
        .collect())
}
pub fn install_update(paths: &Paths, candidate: &Candidate) -> Result<package::InstallResult> {
    let sources: Vec<_> = paths
        .sources()?
        .repositories
        .into_iter()
        .map(|s| s.id)
        .collect();
    ensure!(
        sources.contains(&candidate.source.id),
        "此仓库已移除，请重新检查更新"
    );
    let file = github::download(candidate, paths)?;
    let _guard = package::lock()?;
    let installed = paths
        .registry()?
        .plugins
        .into_iter()
        .find(|p| p.id == candidate.entry.id)
        .context("插件已卸载")?;
    ensure!(
        precedence(&candidate.entry.version, &installed.version)?.is_gt(),
        "插件已更新，不能降级"
    );
    ensure!(
        installed
            .source
            .as_ref()
            .is_none_or(|s| s.official_source() || s.id == candidate.source.id),
        "插件已绑定其他自定义仓库，请重新检查"
    );
    ensure!(
        paths
            .sources()?
            .repositories
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>()
            == sources,
        "仓库配置已变化，请重新检查更新"
    );
    let switch = installed
        .source
        .as_ref()
        .is_some_and(|s| s.id != candidate.source.id);
    package::install_at(
        paths,
        &file,
        switch,
        Some(candidate.source.clone()),
        Some(&candidate.entry),
    )
}
impl Updates {
    pub fn new(paths: Paths, mut enabled: bool) -> Self {
        let (sender, requests) = mpsc::sync_channel(8);
        let (output, receive) = mpsc::channel();
        let initial = Arc::new(Snapshot {
            status: "尚未检查更新".into(),
            ..Default::default()
        });
        let snapshot = initial.clone();
        std::thread::spawn(move || {
            let mut state = (*initial).clone();
            let mut candidates = Vec::new();
            let cache_file = paths.root.join("cache/updates.json");
            let mut due = Instant::now() + Duration::from_secs(10);
            let mut known_sources: Vec<_> = paths
                .sources()
                .map(|s| s.repositories.into_iter().map(|s| s.id).collect())
                .unwrap_or_default();
            if let Ok(bytes) = std::fs::read(&cache_file)
                && bytes.len() <= 8 * 1024 * 1024
                && let Ok(cache) = serde_json::from_slice::<Cache>(&bytes)
                && cache.host_version == env!("CARGO_PKG_VERSION")
                && cache.sources == known_sources
                && now()
                    .checked_sub(cache.at)
                    .is_some_and(|age| age < DAILY.as_secs())
            {
                candidates = cache.candidates;
                state.main = cache.main.filter(|r| {
                    precedence(&r.version, env!("CARGO_PKG_VERSION")).is_ok_and(|o| o.is_gt())
                });
                state.plugins = inventory(&paths, &candidates).unwrap_or_default();
                state.status = if state.main.is_some() || !state.plugins.is_empty() {
                    "发现可用更新".into()
                } else {
                    "上次检查已是最新版本".into()
                };
                due = Instant::now()
                    + Duration::from_secs(
                        DAILY.as_secs() - now().saturating_sub(cache.at).min(DAILY.as_secs()),
                    );
                let _ = output.send(Arc::new(state.clone()));
            }
            loop {
                let request = if enabled {
                    match requests.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(r) => r,
                        Err(mpsc::RecvTimeoutError::Timeout) => Request::Check(false),
                        Err(_) => break,
                    }
                } else {
                    match requests.recv() {
                        Ok(r) => r,
                        Err(_) => break,
                    }
                };
                let stop = matches!(&request, Request::Stop);
                let result = (|| -> Result<()> {
                    match request {
                        Request::Stop => return Ok(()),
                        Request::Enable(value) => {
                            enabled = value;
                            due = Instant::now() + Duration::from_secs(10);
                            return Ok(());
                        }
                        Request::Inventory => {
                            let current: Vec<_> = paths
                                .sources()?
                                .repositories
                                .into_iter()
                                .map(|s| s.id)
                                .collect();
                            if current != known_sources {
                                due = Instant::now();
                                state.plugins.clear();
                                state.status = "插件仓库已变化，请检查更新".into();
                            } else {
                                state.plugins = inventory(&paths, &candidates)?;
                            }
                        }
                        Request::Check(force) => {
                            state.checking = true;
                            state.status = "正在检查更新…".into();
                            let _ = output.send(Arc::new(state.clone()));
                            let main = github::latest_app();
                            let mut errors = Vec::new();
                            let sources = match paths.sources() {
                                Ok(s) => s,
                                Err(error) => {
                                    errors.push(format!("插件仓库：{error:#}"));
                                    Sources {
                                        repositories: vec![],
                                    }
                                }
                            };
                            known_sources =
                                sources.repositories.iter().map(|s| s.id.clone()).collect();
                            let mut failed_sources = std::collections::HashSet::new();
                            let mut all = Vec::new();
                            for source in &sources.repositories {
                                match github::catalog(&paths, source, force) {
                                    Ok(entries) => all.extend(entries),
                                    Err(error) => {
                                        failed_sources.insert(source.id.clone());
                                        errors.push(format!("{}：{error:#}", source.name))
                                    }
                                }
                            }
                            match main {
                                Ok(release) => {
                                    state.main =
                                        if precedence(&release.version, env!("CARGO_PKG_VERSION"))?
                                            .is_gt()
                                        {
                                            Some(release)
                                        } else {
                                            None
                                        }
                                }
                                Err(error) => errors.push(format!("本体：{error:#}")),
                            }
                            // If a custom repository is unavailable, do not silently
                            // replace its preferred same-ID override with official.
                            let custom_failed = sources
                                .repositories
                                .iter()
                                .any(|s| !s.official_source() && failed_sources.contains(&s.id));
                            if custom_failed {
                                all.retain(|c| !c.source.official_source());
                            }
                            candidates = all;
                            state.plugins = match inventory(&paths, &candidates) {
                                Ok(entries) => entries,
                                Err(error) => {
                                    errors.push(format!("插件清单：{error:#}"));
                                    HashMap::new()
                                }
                            };
                            state.status = if errors.is_empty() {
                                if state.main.is_none() && state.plugins.is_empty() {
                                    "已是最新版本".into()
                                } else {
                                    "发现可用更新".into()
                                }
                            } else {
                                format!("部分更新检查失败：{}", errors.join("；"))
                            };
                            if errors.is_empty() {
                                std::fs::create_dir_all(cache_file.parent().unwrap())?;
                                let pending = cache_file.with_extension("json.new");
                                std::fs::write(
                                    &pending,
                                    serde_json::to_vec(&Cache {
                                        at: now(),
                                        main: state.main.clone(),
                                        candidates: candidates.clone(),
                                        host_version: env!("CARGO_PKG_VERSION").into(),
                                        sources: known_sources.clone(),
                                    })?,
                                )?;
                                std::fs::rename(pending, &cache_file)?;
                            }
                            due = Instant::now()
                                + if errors.is_empty() {
                                    DAILY
                                } else {
                                    Duration::from_secs(60 * 60)
                                };
                        }
                        Request::Plugin(candidate) => {
                            let id = candidate.entry.id.clone();
                            let installed = paths
                                .registry()?
                                .plugins
                                .into_iter()
                                .find(|p| p.id == id)
                                .context("插件已卸载")?;
                            ensure!(
                                precedence(&candidate.entry.version, &installed.version)?.is_gt(),
                                "插件已是最新版本"
                            );
                            state.busy = Some(id.clone());
                            state.status = format!("正在更新 {}…", candidate.entry.name);
                            let _ = output.send(Arc::new(state.clone()));
                            ensure!(
                                select(&installed, &candidates)
                                    .is_some_and(|next| next.source.id == candidate.source.id
                                        && next.entry.version == candidate.entry.version),
                                "插件更新来源已变化，请重新检查"
                            );
                            let result = install_update(&paths, &candidate)?;
                            state.plugins = inventory(&paths, &candidates)?;
                            state.status = result.summary();
                            notify_inventory();
                        }
                        Request::DownloadMain(release) => {
                            ensure!(
                                state
                                    .main
                                    .as_ref()
                                    .is_some_and(|current| current.version == release.version
                                        && current.tag == release.tag),
                                "更新版本已变化，请重新确认"
                            );
                            state.busy = Some("main".into());
                            state.status = format!("正在下载 Ultra Bar {}…", release.version);
                            let _ = output.send(Arc::new(state.clone()));
                            state.installer =
                                Some(github::download_app(&release, &paths.root.join("updates"))?);
                            state.status = "安装包已就绪".into();
                        }
                        Request::Installed => {
                            state.installer = None;
                            state.status = "已打开更新安装程序".into();
                        }
                    }
                    Ok(())
                })();
                if stop {
                    break;
                }
                if let Err(error) = result {
                    state.status = format!("更新失败：{error:#}");
                    due = Instant::now() + Duration::from_secs(60 * 60);
                }
                state.checking = false;
                state.busy = None;
                crate::visual::diagnostics::event(
                    "updates",
                    "state",
                    serde_json::json!({"status":state.status,"plugins":state.plugins.len(),"main":state.main.as_ref().map(|r|&r.version)}),
                );
                if output.send(Arc::new(state.clone())).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            receive,
            snapshot,
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.receive.try_recv() {
            self.snapshot = update;
            changed = true;
        }
        changed
    }
    pub fn inventory_changed(&self) {
        let _ = self.sender.try_send(Request::Inventory);
    }
    pub fn check(&mut self) {
        self.poll();
        if self.snapshot.checking || self.snapshot.busy.is_some() {
            return;
        }
        if self.sender.try_send(Request::Check(true)).is_ok() {
            let mut state = (*self.snapshot).clone();
            state.checking = true;
            state.status = "正在检查更新…".into();
            self.snapshot = Arc::new(state);
        }
    }
    pub fn enabled(&self, value: bool) {
        let _ = self.sender.try_send(Request::Enable(value));
    }
    pub fn plugin(&mut self, candidate: Candidate) {
        if self.snapshot.busy.is_some() {
            return;
        }
        let id = candidate.entry.id.clone();
        if self
            .sender
            .try_send(Request::Plugin(Box::new(candidate)))
            .is_ok()
        {
            let mut state = (*self.snapshot).clone();
            state.busy = Some(id);
            self.snapshot = Arc::new(state);
        }
    }
    pub fn download_main(&mut self, release: github::AppRelease) {
        if self.snapshot.busy.is_some() {
            return;
        }
        if self
            .sender
            .try_send(Request::DownloadMain(Box::new(release)))
            .is_ok()
        {
            let mut state = (*self.snapshot).clone();
            state.installer = None;
            state.busy = Some("main".into());
            state.status = "正在下载安装包…".into();
            self.snapshot = Arc::new(state);
        }
    }
    pub fn installer_started(&self) {
        let _ = self.sender.try_send(Request::Installed);
    }
}
impl Drop for Updates {
    fn drop(&mut self) {
        let _ = self.sender.try_send(Request::Stop);
    }
}
pub fn smoke(directory: &std::path::Path, download: bool) -> Result<()> {
    ensure!(directory.is_absolute(), "更新验证目录必须为绝对路径");
    std::fs::create_dir_all(directory)?;
    if download {
        let release = github::latest_app()?;
        let file = github::download_app(&release, &directory.join("download"))?;
        std::fs::write(
            directory.join("download-result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"version":release.version,"file":file,"verified":true,"installer_launched":false}),
            )?,
        )?;
        let paths = Paths {
            root: directory.join(format!(
                "isolated-plugin-system-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            )),
        };
        let candidates = github::catalog(&paths, &Source::official(), false)?;
        let candidate = candidates.first().context("官方库没有可验证的插件")?;
        let archive = github::download(candidate, &paths)?;
        package::install_at(
            &paths,
            &archive,
            false,
            Some(candidate.source.clone()),
            Some(&candidate.entry),
        )?;
        let mut registry = paths.registry()?;
        registry.plugins[0].version = "0.0.0".into();
        registry.plugins[0].enabled = false;
        paths.save("installed.json", &registry)?;
        let marker = directory.join("plugin-user-data-marker.json");
        std::fs::write(&marker, b"preserved test user data")?;
        let result = install_update(&paths, candidate)?;
        ensure!(
            std::fs::read(&marker)? == b"preserved test user data" && !result.plugin.enabled,
            "插件更新改变了数据或启用设置"
        );
        std::fs::write(
            directory.join("plugin-update-result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"id":result.plugin.id,"version":result.plugin.version,"source":result.plugin.source,"data_preserved":true,"enabled_preserved":true,"plugin_process_launched":false}),
            )?,
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn custom(id: u64) -> Source {
        Source {
            id: format!("github:{id}"),
            full_name: format!("example/repo{id}"),
            name: "官方插件库".into(),
        }
    }
    fn installed(source: Option<Source>, version: &str) -> Installed {
        Installed {
            id: "com.example.calendar".into(),
            name: "日历".into(),
            version: version.into(),
            source,
            digest: "a".repeat(64),
            generation: "one".into(),
            installed_at: 0,
            enabled: true,
        }
    }
    fn candidate(source: Source, version: &str) -> Candidate {
        Candidate {
            entry: CatalogEntry {
                id: "com.example.calendar".into(),
                name: "日历".into(),
                version: version.into(),
                host: "*".into(),
                platform: PLATFORM.into(),
                asset: "calendar.ubplugin".into(),
                sha256: "a".repeat(64),
                description: String::new(),
            },
            source,
            asset_id: 1,
            download_url: String::new(),
            size: 1,
            release_tag: String::new(),
        }
    }
    #[test]
    fn custom_priority_uses_ids_and_keeps_an_installed_custom_source() {
        let official = candidate(Source::official(), "3.0.0");
        let a = candidate(custom(100), "2.0.0");
        let b = candidate(custom(200), "9.0.0");
        assert_eq!(
            select(
                &installed(Some(Source::official()), "1.0.0"),
                &[official.clone(), a.clone()]
            )
            .unwrap()
            .source
            .id,
            a.source.id
        );
        assert_eq!(
            select(
                &installed(Some(custom(100)), "1.0.0"),
                &[official.clone(), a.clone(), b.clone()]
            )
            .unwrap()
            .source
            .id,
            a.source.id
        );
        assert!(
            select(
                &installed(Some(custom(100)), "1.0.0"),
                &[official.clone(), b]
            )
            .is_none()
        );
        assert!(
            select(
                &installed(Some(Source::official()), "2.5.0"),
                &[official.clone(), a.clone()]
            )
            .is_none()
        );
        assert_eq!(
            select(&installed(None, "1.0.0"), &[official.clone(), a])
                .unwrap()
                .source
                .id,
            custom(100).id
        );
        let mut namesake = candidate(custom(300), "99.0.0");
        namesake.entry.id = "com.example.other".into();
        assert!(select(&installed(None, "1.0.0"), &[namesake]).is_none());
    }
    #[test]
    fn automatic_updates_reject_prereleases_metadata_only_changes_and_incompatible_hosts() {
        let p = installed(None, "1.0.0");
        assert!(select(&p, &[candidate(Source::official(), "1.0.0+another")]).is_none());
        assert!(select(&p, &[candidate(Source::official(), "2.0.0-rc.1")]).is_none());
        let mut incompatible = candidate(custom(100), "5.0.0");
        incompatible.entry.host = ">=99.0.0".into();
        assert_eq!(
            select(&p, &[incompatible, candidate(Source::official(), "2.0.0")])
                .unwrap()
                .source
                .id,
            OFFICIAL_ID
        );
    }
}
