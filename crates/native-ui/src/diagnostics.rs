//! Bounded asynchronous diagnostic records; seven distinct active local dates.
use serde_json::{Value, json};
use std::os::windows::{
    fs::{MetadataExt, OpenOptionsExt},
    io::IntoRawHandle,
};
use std::{
    collections::{HashMap, hash_map::Entry},
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{
        Diagnostics::Debug::*,
        SystemInformation::{GetLocalTime, GetTickCount64},
        Threading::*,
    },
};
const MARKER: &str = ".ultra-bar-active-day";
const FILE_LIMIT: u64 = 8 * 1024 * 1024;
static LOGGER: OnceLock<Logger> = OnceLock::new();
static CRASH: AtomicIsize = AtomicIsize::new(0);
struct Logger {
    root: PathBuf,
    session: String,
    role: String,
    tx: SyncSender<Record>,
    stop: Arc<AtomicBool>,
    dropped: AtomicU64,
    sequence: AtomicU64,
}
struct Record {
    day: String,
    category: String,
    bytes: Vec<u8>,
}
pub struct Session {
    done: Option<Receiver<()>>,
    owns: bool,
}
pub fn root() -> Option<&'static Path> {
    LOGGER.get().map(|l| l.root.as_path())
}
pub fn session() -> Option<&'static str> {
    LOGGER.get().map(|l| l.session.as_str())
}
pub fn smoke(directory: &Path) -> io::Result<()> {
    if !directory.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let fixture = directory.join("retention-check");
    std::fs::create_dir_all(&fixture)?;
    let dates = [
        "2025-01-01",
        "2025-01-05",
        "2025-02-10",
        "2025-03-02",
        "2025-04-18",
        "2025-06-09",
        "2025-07-21",
        "2025-08-15",
        "2025-10-30",
    ];
    for day in dates {
        prepare_day(&fixture, day)?;
    }
    let unmanaged = fixture.join("2024-01-01");
    std::fs::create_dir_all(&unmanaged)?;
    std::fs::write(unmanaged.join("keep.txt"), "unmanaged")?;
    prune(&fixture)?;
    for (i, day) in dates.iter().enumerate() {
        if fixture.join(day).exists() != (i >= 2) {
            return Err(io::Error::other("Active-date retention failed"));
        }
    }
    if !unmanaged.join("keep.txt").is_file() {
        return Err(io::Error::other("Unmanaged file was removed"));
    }
    for category in [
        "lifecycle",
        "shell",
        "tasks",
        "tray",
        "plugins",
        "settings",
        "input",
        "render",
        "search",
        "usage",
        "calendar",
        "health",
    ] {
        event(
            category,
            "probe.record",
            json!({"unicode":"诊断日志验证","active_dates":7}),
        );
    }
    event(
        "errors",
        "probe.redaction",
        json!({"api_key":"fake-key-never-write","nested":{"access_token":"fake-token-never-write"},"message":"Authorization: Bearer fake-bearer-never-write"}),
    );
    let mut workers = vec![];
    for i in 0..4 {
        workers.push(std::thread::spawn(move || {
            for j in 0..20 {
                event("concurrency", "probe.record", json!({"worker":i,"index":j}));
            }
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    emergency(0xe0554254, 0);
    std::fs::write(
        directory.join("retention-check.json"),
        serde_json::to_vec_pretty(
            &json!({"kept_active_dates":&dates[2..],"removed_active_dates":&dates[..2],"unmanaged_preserved":true}),
        )?,
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_fields_nested_values_and_headers_are_redacted_without_breaking_unicode() {
        let mut value = json!({"api_key":"secret-a","nested":[{"refresh_token":"secret-b","password":"secret-c"}],"message":"Authorization: Bearer secret-d\n下一行","title":"私密内容","name":"Google Chrome"});
        redact(&mut value);
        let text = value.to_string();
        for secret in ["secret-a", "secret-b", "secret-c", "secret-d", "私密内容"] {
            assert!(!text.contains(secret));
        }
        assert!(text.contains("Google Chrome"));
        assert!(text.contains("下一行"));
        let mut long = Value::String("中文".repeat(5000));
        redact(&mut long);
        assert!(long.as_str().unwrap().len() < 8300);
    }
    #[test]
    fn retention_uses_seven_sparse_active_dates_not_seven_calendar_days() {
        let root = PathBuf::from(format!(
            "D:/tmp/ultra-bar-logging-20261112/retention-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let days = [
            "2024-01-01",
            "2024-02-05",
            "2024-03-01",
            "2024-05-12",
            "2024-07-19",
            "2025-01-01",
            "2025-02-07",
            "2025-05-09",
            "2025-11-11",
        ];
        for day in days {
            prepare_day(&root, day).unwrap();
        }
        std::fs::create_dir_all(root.join("2020-01-01")).unwrap();
        std::fs::write(root.join("2020-01-01/user.txt"), "keep").unwrap();
        prune(&root).unwrap();
        for (i, day) in days.iter().enumerate() {
            assert_eq!(root.join(day).exists(), i >= 2, "{day}");
        }
        assert!(root.join("2020-01-01/user.txt").is_file());
        prune(&root).unwrap();
    }
}
pub fn process_metrics() -> Value {
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::*;
        let mut memory: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as _;
        let ok = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            memory.cb,
        ) != 0;
        let (mut created, mut ended, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = std::mem::zeroed();
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut ended,
            &mut kernel,
            &mut user,
        );
        let time = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) / 10000;
        let mut handles = 0;
        GetProcessHandleCount(GetCurrentProcess(), &mut handles);
        json!({"memory_available":ok,"working_set_bytes":memory.WorkingSetSize,"private_bytes":memory.PrivateUsage,"kernel_cpu_ms":time(kernel),"user_cpu_ms":time(user),"handles":handles})
    }
}
fn now() -> (String, String, u64) {
    unsafe {
        let mut t: SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        let day = format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay);
        let time = format!(
            "{day}T{:02}:{:02}:{:02}.{:03}",
            t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        );
        (
            day,
            time,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        )
    }
}
fn safe_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(32)
        .collect()
}
pub fn init(role: &str, version: &str) -> io::Result<Session> {
    init_in(role, version, None)
}
pub fn init_in(role: &str, version: &str, directory: Option<PathBuf>) -> io::Result<Session> {
    if LOGGER.get().is_some() {
        return Ok(Session {
            done: None,
            owns: false,
        });
    }
    let exe = std::env::current_exe()?;
    let dev_target = exe
        .ancestors()
        .find(|p| {
            matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("dev-run" | "dev-build")
            ) && p
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                == Some("target")
        })
        .and_then(Path::parent);
    let root = directory
        .or_else(|| std::env::var_os("ULTRA_BAR_LOG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            dev_target
                .unwrap_or_else(|| exe.parent().unwrap_or(Path::new(".")))
                .join("logs")
        });
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Log directory must be absolute",
        ));
    }
    std::fs::create_dir_all(&root)?;
    let (day, _, ms) = now();
    let role = safe_name(role);
    let id = format!("{}-{ms}", std::process::id());
    prepare_day(&root, &day)?;
    open_crash(&root, &day, &role, &id)?;
    let (tx, rx) = mpsc::sync_channel(1024);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    LOGGER
        .set(Logger {
            root: root.clone(),
            session: id.clone(),
            role: role.clone(),
            tx,
            stop: stop.clone(),
            dropped: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
        })
        .map_err(|_| io::ErrorKind::AlreadyExists)?;
    std::thread::Builder::new()
        .name("diagnostic-writer".into())
        .spawn(move || {
            writer(root, role, id, rx, stop);
            let _ = done_tx.send(());
        })?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emergency(0xe0554250, 0);
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic");
        event(
            "errors",
            "rust.panic",
            json!({"message":payload,"location":info.location().map(|p|format!("{}:{}",p.file(),p.line())),"backtrace":format!("{}",std::backtrace::Backtrace::force_capture())}),
        );
        previous(info);
    }));
    unsafe {
        let mut reserve = 65536;
        SetThreadStackGuarantee(&mut reserve);
        SetUnhandledExceptionFilter(Some(native_exception));
    }
    event(
        "lifecycle",
        "process.start",
        json!({"version":version,"exe":exe,"role":LOGGER.get().unwrap().role,"parent_session":std::env::var("ULTRA_BAR_PARENT_SESSION").ok(),"log_root":LOGGER.get().unwrap().root,"retention_active_dates":7}),
    );
    Ok(Session {
        done: Some(done_rx),
        owns: true,
    })
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.owns {
            if let Some(l) = LOGGER.get() {
                l.stop.store(true, Ordering::Release);
            }
            event("lifecycle", "logger.flush", json!({}));
            if let Some(done) = self.done.take() {
                let _ = done.recv_timeout(Duration::from_secs(2));
            }
        }
    }
}
struct LastError(u32);
impl Drop for LastError {
    fn drop(&mut self) {
        unsafe {
            SetLastError(self.0);
        }
    }
}
#[track_caller]
pub fn event(category: &str, name: &str, data: Value) {
    let Some(l) = LOGGER.get() else {
        return;
    };
    let _error = LastError(unsafe { GetLastError() });
    let (day, time, utc_ms) = now();
    let mut data = data;
    redact(&mut data);
    let lower = format!(
        "{name} {}",
        data.get("message").and_then(Value::as_str).unwrap_or("")
    )
    .to_ascii_lowercase();
    let failed = data.get("ok").and_then(Value::as_bool) == Some(false)
        || data
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
    let level = if category == "errors"
        || lower.contains("panic")
        || lower.contains("fatal")
        || (name == "process.end" && failed)
    {
        "ERROR"
    } else if failed
        || [
            "failed", "error", "timeout", "rejected", "超时", "失败", "暂停",
        ]
        .iter()
        .any(|s| lower.contains(s))
    {
        "WARN"
    } else {
        "INFO"
    };
    let at = std::panic::Location::caller();
    let mut value = json!({"time":time,"utc_ms":utc_ms,"tick_ms":unsafe{GetTickCount64()},"level":level,"source":{"file":at.file(),"line":at.line()},"seq":l.sequence.fetch_add(1,Ordering::Relaxed),"pid":std::process::id(),"tid":unsafe{GetCurrentThreadId()},"session":l.session,"role":l.role,"category":category,"event":name,"data":data});
    let Ok(mut bytes) = serde_json::to_vec(&value) else {
        return;
    };
    if bytes.len() > 16384 {
        value["data"] = json!({"truncated":true,"original_bytes":bytes.len()});
        bytes = serde_json::to_vec(&value).unwrap_or_default();
    }
    bytes.push(b'\n');
    if l.tx
        .try_send(Record {
            day,
            category: safe_name(category),
            bytes,
        })
        .is_err()
    {
        l.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
pub fn message(source: &str, line: u32, text: &str) {
    let safe_text = if let Some(at) = text.find('{') {
        if let Ok(mut value) = serde_json::from_str::<Value>(&text[at..]) {
            redact(&mut value);
            format!("{}{}", &text[..at], value)
        } else {
            text.to_owned()
        }
    } else {
        text.to_owned()
    };
    let category = if text.starts_with("[tray") || source.contains("::tray") {
        "tray"
    } else if text.starts_with("[task") || source.contains("task_apps") {
        "tasks"
    } else if source.contains("settings") || text.starts_with("[settings") {
        "settings"
    } else if source.contains("plugin")
        || text.starts_with("[plugins")
        || text.starts_with("[service")
    {
        "plugins"
    } else if source.contains("usage_meter") || text.contains("DirectComposition") {
        "render"
    } else if source.contains("shell") || source.contains("lifecycle") {
        "shell"
    } else {
        "runtime"
    };
    event(
        category,
        "diagnostic",
        json!({"source":source,"line":line,"message":safe_text}),
    );
}
fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                let k = key.to_ascii_lowercase().replace('-', "_");
                let compact = k.replace('_', "");
                if [
                    "api_key",
                    "token",
                    "apikey",
                    "authorization",
                    "access_token",
                    "refresh_token",
                    "password",
                    "secret",
                    "cookie",
                    "prompt",
                    "notes",
                    "query",
                    "title",
                    "content",
                    "body",
                ]
                .iter()
                .any(|secret| k == *secret || k.ends_with(&format!("_{secret}")))
                    || [
                        "apikey",
                        "accesstoken",
                        "refreshtoken",
                        "authorization",
                        "password",
                        "secret",
                        "cookie",
                    ]
                    .iter()
                    .any(|name| compact == *name)
                {
                    *v = Value::String("[redacted]".into());
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(items) => {
            items.truncate(128);
            for item in items {
                redact(item);
            }
        }
        Value::String(text) => {
            for marker in [
                "Bearer ",
                "api_key=",
                "apikey=",
                "password=",
                "access_token=",
                "refresh_token=",
                "\"api_key\":\"",
                "\"api_key\": \"",
                "\"apikey\":\"",
                "\"access_token\":\"",
                "\"refresh_token\":\"",
                "\"password\":\"",
                "X-QW-Api-Key: ",
            ] {
                let lower = text.to_ascii_lowercase();
                if let Some(start) = lower.find(&marker.to_ascii_lowercase()) {
                    let start = start + marker.len();
                    let end = text[start..]
                        .find([' ', '\r', '\n', '&', '\"'])
                        .map_or(text.len(), |n| start + n);
                    text.replace_range(start..end, "[redacted]");
                }
            }
            if text.len() > 8192 {
                let end = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|i| *i <= 8192)
                    .last()
                    .unwrap_or(0);
                text.truncate(end);
                text.push_str("…[truncated]");
            }
        }
        _ => {}
    }
}
fn prepare_day(root: &Path, day: &str) -> io::Result<PathBuf> {
    let path = root.join(day);
    std::fs::create_dir_all(&path)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(MARKER))?;
    Ok(path)
}
fn date_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        && name[5..7]
            .parse::<u32>()
            .is_ok_and(|n| (1..=12).contains(&n))
        && name[8..10]
            .parse::<u32>()
            .is_ok_and(|n| (1..=31).contains(&n))
}
fn prune(root: &Path) -> io::Result<()> {
    let absolute = root.canonicalize()?;
    let mut days = vec![];
    for item in std::fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        let metadata = std::fs::symlink_metadata(item.path())?;
        if date_name(&name)
            && metadata.is_dir()
            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && item.path().join(MARKER).is_file()
        {
            days.push((name, item.path()));
        }
    }
    days.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in days.into_iter().skip(7) {
        let target = path.canonicalize()?;
        if target.starts_with(&absolute) && target != absolute {
            std::fs::remove_dir_all(target)?;
        }
    }
    Ok(())
}
struct Output {
    file: File,
    bytes: u64,
    part: u32,
}
fn output(day: &Path, category: &str, role: &str, id: &str, part: u32) -> io::Result<Output> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .share_mode(7)
        .open(day.join(format!("{category}-{role}-{id}-{part}.jsonl")))?;
    let bytes = file.metadata()?.len();
    Ok(Output { file, bytes, part })
}
fn writer(root: PathBuf, role: String, id: String, rx: Receiver<Record>, stop: Arc<AtomicBool>) {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    let mut date = String::new();
    let mut files: HashMap<String, Output> = HashMap::new();
    loop {
        let record = if stop.load(Ordering::Acquire) {
            match rx.try_recv() {
                Ok(record) => record,
                Err(_) => break,
            }
        } else {
            match rx.recv() {
                Ok(record) => record,
                Err(_) => break,
            }
        };
        if date != record.day {
            files.clear();
            date = record.day.clone();
            let _ = open_crash(&root, &date, &role, &id);
            let _ = prepare_day(&root, &date);
            if let Err(error) = prune(&root) {
                event(
                    "errors",
                    "logger.retention_failed",
                    json!({"error":error.to_string()}),
                );
            }
        }
        let day = root.join(&date);
        let result = (|| -> io::Result<()> {
            if let Entry::Vacant(e) = files.entry(record.category.clone()) {
                e.insert(output(&day, &record.category, &role, &id, 0)?);
            }
            let out = files.get_mut(&record.category).unwrap();
            if out.bytes >= FILE_LIMIT {
                *out = output(&day, &record.category, &role, &id, out.part + 1)?;
            }
            out.file.write_all(&record.bytes)?;
            out.bytes += record.bytes.len() as u64;
            if let Some(l) = LOGGER.get() {
                let dropped = l.dropped.swap(0, Ordering::Relaxed);
                if dropped != 0 {
                    let v = json!({"event":"logger.dropped","count":dropped,"pid":std::process::id(),"time":now().1});
                    writeln!(out.file, "{v}")?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            emergency(0xe055424c, 0);
        }
    }
    for f in files.values_mut() {
        let _ = f.file.flush();
    }
}
fn open_crash(root: &Path, day: &str, role: &str, id: &str) -> io::Result<()> {
    let day = prepare_day(root, day)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .share_mode(7)
        .open(day.join(format!("crash-{role}-{id}.jsonl")))?;
    // Keep old handles until process exit; a fault on another thread can be using one.
    CRASH.store(file.into_raw_handle() as isize, Ordering::Release);
    Ok(())
}
fn emergency(code: u32, address: usize) {
    unsafe {
        let handle = CRASH.load(Ordering::Acquire);
        if handle == 0 {
            return;
        }
        let mut bytes = [0u8; 256];
        let mut len = 0;
        fn add(out: &mut [u8], n: &mut usize, text: &[u8]) {
            for b in text {
                if *n < out.len() {
                    out[*n] = *b;
                    *n += 1;
                }
            }
        }
        fn number(out: &mut [u8], n: &mut usize, mut value: u64) {
            let mut digits = [0u8; 20];
            let mut at = 20;
            loop {
                at -= 1;
                digits[at] = b'0' + (value % 10) as u8;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            add(out, n, &digits[at..]);
        }
        add(
            &mut bytes,
            &mut len,
            b"{\"event\":\"fatal.native\",\"pid\":",
        );
        number(&mut bytes, &mut len, GetCurrentProcessId() as u64);
        add(&mut bytes, &mut len, b",\"tid\":");
        number(&mut bytes, &mut len, GetCurrentThreadId() as u64);
        add(&mut bytes, &mut len, b",\"code\":");
        number(&mut bytes, &mut len, code as u64);
        add(&mut bytes, &mut len, b",\"address\":");
        number(&mut bytes, &mut len, address as u64);
        add(&mut bytes, &mut len, b",\"tick_ms\":");
        number(&mut bytes, &mut len, GetTickCount64());
        add(&mut bytes, &mut len, b"}\n");
        let mut written = 0;
        WriteFile(
            handle as HANDLE,
            bytes.as_ptr(),
            len as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        FlushFileBuffers(handle as HANDLE);
    }
}
unsafe extern "system" fn native_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    unsafe {
        if !info.is_null() && !(*info).ExceptionRecord.is_null() {
            emergency(
                (*(*info).ExceptionRecord).ExceptionCode as u32,
                (*(*info).ExceptionRecord).ExceptionAddress as usize,
            );
        }
        0
    }
}
#[macro_export]
macro_rules! diagnostic_stderr {($($arg:tt)*)=>{{let text=format!($($arg)*);$crate::diagnostics::message(module_path!(),line!(),&text);::std::eprintln!("{text}");}};}
