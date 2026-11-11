mod job;
mod native;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::PostMessageW};
pub const UPDATE: u32 = 0x8008;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Query {
    pub hwnd: usize,
    pub pid: u32,
    pub executable: String,
    pub shortcut: Option<String>,
    pub app_id: Option<String>,
    pub output: PathBuf,
    pub prefix: String,
}
impl Query {
    pub fn new(hwnd: usize, pid: u32, executable: String, shortcut: Option<String>) -> Self {
        Self {
            hwnd,
            pid,
            executable,
            shortcut,
            app_id: None,
            output: PathBuf::new(),
            prefix: String::new(),
        }
    }
    fn key(&self) -> String {
        format!(
            "{}:{}:{}:{:?}:{:?}",
            self.hwnd, self.pid, self.executable, self.shortcut, self.app_id
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub title: String,
    pub category: String,
    pub path: PathBuf,
    pub kind: String,
    pub icon: String,
    pub icon_index: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wire {
    pub app_id: Option<String>,
    pub custom_supported: bool,
    pub entries: Vec<Entry>,
}
struct Lease {
    root: PathBuf,
    prefix: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(files) = std::fs::read_dir(&self.root) {
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if (name.starts_with(&format!("{}-", self.prefix))
                    || name == format!("{}.json", self.prefix)
                    || name == format!("{}.request.json", self.prefix))
                    && file.file_type().is_ok_and(|kind| kind.is_file())
                {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
}
#[derive(Clone)]
pub struct Snapshot {
    pub wire: Wire,
    _lease: Arc<Lease>,
}
impl Snapshot {
    pub fn invoke(&self, index: usize, owner: HWND) -> Result<()> {
        native::invoke(
            self.wire.entries.get(index).context("快捷操作已失效")?,
            owner,
        )
    }
}
pub struct Update {
    pub generation: u64,
    pub result: Result<Snapshot>,
}
pub struct Hub {
    sender: Option<Sender<(u64, Query)>>,
    pub updates: Receiver<Update>,
    generation: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
    done: Receiver<()>,
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.sender.take();
        if !matches!(
            self.done.recv_timeout(Duration::from_millis(750)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}
impl Hub {
    pub fn start(owner: usize) -> Self {
        let (sender, requests) = mpsc::channel::<(u64, Query)>();
        let (outgoing, updates) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let worker = std::thread::spawn(move || {
            if let Ok(root) = crate::data_paths::root() {
                sweep_stale(&root.join("cache/jump-lists"));
            }
            let mut cache: HashMap<String, (Instant, Snapshot)> = HashMap::new();
            while let Ok(mut request) = requests.recv() {
                while let Ok(next) = requests.try_recv() {
                    request = next;
                }
                let (tag, query) = request;
                if current.load(Ordering::Relaxed) != tag {
                    continue;
                }
                let key = query.key();
                let result = if let Some((time, snapshot)) = cache.get(&key)
                    && time.elapsed() < Duration::from_secs(5)
                {
                    Ok(snapshot.clone())
                } else {
                    let result = crate::data_paths::root()
                        .map(|root| root.join("cache/jump-lists"))
                        .and_then(|root| read_isolated(query, &root, tag, &current));
                    if let Ok(snapshot) = &result {
                        if cache.len() >= 8
                            && let Some(oldest) = cache
                                .iter()
                                .min_by_key(|(_, value)| value.0)
                                .map(|(key, _)| key.clone())
                        {
                            cache.remove(&oldest);
                        }
                        cache.insert(key, (Instant::now(), snapshot.clone()));
                    }
                    result
                };
                if current.load(Ordering::Relaxed) != tag {
                    continue;
                }
                if outgoing
                    .send(Update {
                        generation: tag,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
                unsafe {
                    PostMessageW(owner as HWND, UPDATE, 0, 0);
                }
            }
            drop(cache);
            let _ = finished.send(());
        });
        Self {
            sender: Some(sender),
            updates,
            generation,
            worker: Some(worker),
            done,
        }
    }
    pub fn request(&self, query: Query) -> u64 {
        let tag = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(sender) = &self.sender {
            let _ = sender.send((tag, query));
        }
        tag
    }
}
fn sweep_stale(root: &Path) {
    if let Ok(files) = std::fs::read_dir(root) {
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let fields: Vec<_> = name.split('.').next().unwrap_or("").split('-').collect();
            let generated = (fields.len() == 4 || fields.len() == 5)
                && fields[0] == "q"
                && fields
                    .iter()
                    .skip(1)
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                && [".lnk", ".pidl", ".json"]
                    .iter()
                    .any(|extension| name.ends_with(extension));
            if generated
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
                && file.file_type().is_ok_and(|kind| kind.is_file())
                && file
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|time| time.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(86400))
            {
                let _ = std::fs::remove_file(file.path());
            }
        }
    }
}
fn read_isolated(
    mut query: Query,
    directory: &Path,
    generation: u64,
    current: &AtomicU64,
) -> Result<Snapshot> {
    std::fs::create_dir_all(directory)?;
    let root = directory.canonicalize()?;
    query.prefix = format!(
        "q-{}-{}-{generation}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    query.output = root.join(format!("{}.json", query.prefix));
    let lease = Arc::new(Lease {
        root: root.clone(),
        prefix: query.prefix.clone(),
    });
    let input = root.join(format!("{}.request.json", query.prefix));
    std::fs::write(&input, serde_json::to_vec(&query)?)?;
    job::run(&input, generation, current)?;
    ensure!(
        std::fs::metadata(&query.output)?.len() <= 2 * 1024 * 1024,
        "Jump List result too large"
    );
    let wire: Wire = serde_json::from_slice(&std::fs::read(&query.output)?)?;
    ensure!(wire.entries.len() <= 32, "Too many Jump List entries");
    for entry in &wire.entries {
        ensure!(
            entry.path.parent() == Some(root.as_path())
                && entry.path.file_name().is_some_and(|name| name
                    .to_string_lossy()
                    .starts_with(&format!("{}-", query.prefix))),
            "Unexpected Jump List cache path"
        );
        ensure!(
            matches!(entry.kind.as_str(), "link" | "pidl"),
            "Unsupported Shell destination"
        );
        ensure!(
            std::fs::metadata(&entry.path)?.len() <= 1024 * 1024,
            "Shell destination too large"
        );
    }
    Ok(Snapshot {
        wire,
        _lease: lease,
    })
}
pub fn helper(input: &Path) -> Result<()> {
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(0x0001 | 0x0002 | 0x8000);
    }
    ensure!(
        std::fs::metadata(input)?.len() < 128 * 1024,
        "Invalid query"
    );
    let query: Query = serde_json::from_slice(&std::fs::read(input)?)?;
    ensure!(
        query.prefix.starts_with("q-")
            && query.prefix.len() < 100
            && query
                .prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "Invalid cache prefix"
    );
    ensure!(
        query.output.parent() == input.parent(),
        "Invalid output directory"
    );
    let wire = native::read(&query)?;
    std::fs::write(&query.output, serde_json::to_vec(&wire)?)?;
    Ok(())
}
pub fn valid_pidl(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || bytes.len() > 1024 * 1024 {
        return false;
    }
    let mut offset = 0;
    while offset + 2 <= bytes.len() {
        let size = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if size == 0 {
            return offset + 2 == bytes.len();
        }
        if size < 2 {
            return false;
        }
        let Some(next) = offset.checked_add(size) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        offset = next;
    }
    false
}
pub fn diagnose(directory: &Path, args: &[String]) -> Result<()> {
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
    };
    let mut query = Query::new(
        value("--hwnd").map(|v| v.parse()).transpose()?.unwrap_or(0),
        value("--pid").map(|v| v.parse()).transpose()?.unwrap_or(0),
        value("--exe").cloned().unwrap_or_default(),
        value("--shortcut").cloned(),
    );
    query.app_id = value("--appid").cloned();
    let snapshot = read_isolated(query, directory, 1, &AtomicU64::new(1))?;
    let report = serde_json::json!({"app_id":snapshot.wire.app_id,"custom_supported":snapshot.wire.custom_supported,"items":snapshot.wire.entries.iter().map(|entry|serde_json::json!({"title":entry.title,"category":entry.category,"kind":entry.kind})).collect::<Vec<_>>()});
    crate::task_popup::preview_context(directory, &snapshot)?;
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
pub fn smoke(directory: &Path) -> Result<()> {
    ensure!(directory.is_absolute(), "Output directory must be absolute");
    std::fs::create_dir_all(directory)?;
    if !crate::data_paths::is_isolated() {
        crate::data_paths::set(directory.join("profile"))?;
    }
    let working = directory.join("working directory 中文");
    std::fs::create_dir_all(&working)?;
    let marker = directory.join("参数验证 marker.txt");
    if marker.exists() {
        std::fs::remove_file(&marker)?;
    }
    let id = format!(
        "UltraBar.JumpListTest.{}.{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let _fixture = native::fixture(id.clone(), &marker, &working)?;
    let mut query = Query::new(
        0,
        0,
        std::env::current_exe()?.to_string_lossy().into(),
        None,
    );
    query.app_id = Some(id);
    let snapshot = read_isolated(
        query.clone(),
        &directory.join("cache"),
        1,
        &AtomicU64::new(1),
    )?;
    let index = snapshot
        .wire
        .entries
        .iter()
        .position(|entry| entry.title == "原生任务参数验证")
        .context("Published task was not read back")?;
    snapshot.invoke(index, std::ptr::null_mut())?;
    let deadline = Instant::now() + Duration::from_secs(4);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    ensure!(marker.exists(), "Saved Shell task did not execute");
    let actual = std::fs::read_to_string(&marker)?;
    ensure!(
        PathBuf::from(actual).canonicalize()? == working.canonicalize()?,
        "Working directory or Unicode arguments changed"
    );
    let hub = Hub::start(0);
    let first_tag = hub.request(query.clone());
    let first = hub.updates.recv_timeout(Duration::from_secs(4))?;
    ensure!(first.generation == first_tag, "Wrong menu generation");
    let first = first.result?;
    let second_tag = hub.request(query);
    let second = hub.updates.recv_timeout(Duration::from_secs(1))?;
    ensure!(
        second.generation == second_tag,
        "Wrong cached menu generation"
    );
    let second = second.result?;
    ensure!(
        !first.wire.entries.is_empty() && first.wire.entries[0].path == second.wire.entries[0].path,
        "Short cache was not reused"
    );
    let cached = first.wire.entries[0].path.clone();
    drop(first);
    drop(second);
    drop(hub);
    ensure!(!cached.exists(), "Query cache was not released on shutdown");
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"native_task_read":"passed","task_execution":"passed","unicode_and_spaces":"passed","working_directory":"passed","async_reader_cache_and_cleanup":"passed","global_input":false}),
        )?,
    )?;
    println!("Native Jump List task round-trip passed");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pidl_validation_rejects_truncated_oversize_and_trailing_data() {
        assert!(valid_pidl(&[0, 0]));
        assert!(valid_pidl(&[4, 0, 1, 2, 0, 0]));
        for bytes in [
            &[1, 0][..],
            &[4, 0, 1][..],
            &[0, 0, 1][..],
            &[255, 255, 0, 0][..],
        ] {
            assert!(!valid_pidl(bytes));
        }
    }
}
