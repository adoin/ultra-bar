//! Resident plugins are owned processes; existing Everything and agent applications are not.
use crate::{
    plugins::Widget,
    visual::{theme, wide},
};
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    io::{BufReader, Write},
    mem::{size_of, zeroed},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
use ultra_bar_plugin_sdk::{HostEvent, Page, PluginEvent, read_line, valid_pages};
use windows_sys::Win32::{
    Foundation::*,
    System::{JobObjects::*, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};

struct Job(isize);
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0 as _);
        }
    }
}
impl Job {
    fn new(cpu: u32, memory: usize) -> Result<Self> {
        unsafe {
            let job = Self(CreateJobObjectW(std::ptr::null(), std::ptr::null()) as isize);
            ensure!(job.0 != 0, "Cannot create plugin job");
            let mut allowed = 0;
            let mut system = 0;
            ensure!(
                GetProcessAffinityMask(GetCurrentProcess(), &mut allowed, &mut system) != 0,
                "Cannot query affinity"
            );
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_PRIORITY_CLASS
                | JOB_OBJECT_LIMIT_AFFINITY
                | JOB_OBJECT_LIMIT_JOB_MEMORY;
            limits.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
            limits.BasicLimitInformation.Affinity = allowed & allowed.wrapping_neg();
            limits.JobMemoryLimit = memory;
            ensure!(
                SetInformationJobObject(
                    job.0 as _,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32
                ) != 0,
                "Cannot limit plugin memory"
            );
            let limits = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 { CpuRate: cpu * 100 },
            };
            ensure!(
                SetInformationJobObject(
                    job.0 as _,
                    JobObjectCpuRateControlInformation,
                    (&limits as *const JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
                    size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32
                ) != 0,
                "Cannot limit plugin CPU"
            );
            Ok(job)
        }
    }
}
struct Worker {
    binary: PathBuf,
    input: SyncSender<HostEvent>,
    output: Receiver<PluginEvent>,
    job: Option<Job>,
    child: std::process::Child,
    pages: Vec<Page>,
    page_index: usize,
    rotated: Instant,
    flip: Option<Instant>,
    started: Instant,
    retries: u8,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.input.try_send(HostEvent::Shutdown);
        // A bounded grace period preserves orderly flushing, then only this owned job is closed.
        let end = Instant::now() + Duration::from_millis(150);
        while Instant::now() < end && self.child.try_wait().ok().flatten().is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.job.take();
        let _ = self.child.wait();
    }
}
impl Worker {
    fn start(widget: &Widget, owner: isize, global: isize) -> Result<Self> {
        let binary = widget
            .panel
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("插件尚未构建"))?;
        let job = Job::new(5, 384 * 1024 * 1024)?;
        let mut child = Command::new(binary)
            .args(["--service", "--owner", &owner.to_string()])
            .current_dir(&widget.root)
            .env("ULTRA_BAR_ACCENT", theme::hex(theme::accent_rgb()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
            .spawn()?;
        // SDK services wait for Initialize before doing any work. No auth or scanning during startup.
        use std::os::windows::io::AsRawHandle;
        if unsafe { AssignProcessToJobObject(global as _, child.as_raw_handle() as _) } == 0
            || unsafe { AssignProcessToJobObject(job.0 as _, child.as_raw_handle() as _) } == 0
        {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Cannot isolate plugin process");
        }
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (input, receive) = mpsc::sync_channel(16);
        let (send, output) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            while let Ok(value) = receive.recv() {
                if serde_json::to_writer(&mut stdin, &value).is_err()
                    || stdin.write_all(b"\n").is_err()
                    || stdin.flush().is_err()
                {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(line)) = read_line(&mut reader) {
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        let data = crate::data_paths::root()?.join("plugins").join(&widget.id);
        std::fs::create_dir_all(&data)?;
        input.try_send(HostEvent::Initialize {
            protocol: 1,
            id: widget.id.clone(),
            data_dir: data.to_string_lossy().into_owned(),
            accent: theme::hex(theme::accent_rgb()),
        })?;
        Ok(Self {
            binary: binary.clone(),
            input,
            output,
            job: Some(job),
            child,
            pages: vec![],
            page_index: 0,
            rotated: Instant::now(),
            flip: None,
            started: Instant::now(),
            retries: 0,
        })
    }
}
#[derive(Default)]
pub struct Manager {
    workers: HashMap<String, Worker>,
    global_job: Option<Job>,
    foreground: (String, String),
    accent: String,
}
impl Manager {
    pub fn sync(&mut self, widgets: &[Widget], owner: isize) {
        if self.global_job.is_none() && widgets.iter().any(|w| w.service) {
            match Job::new(10, 1024 * 1024 * 1024) {
                Ok(job) => self.global_job = Some(job),
                Err(e) => {
                    eprintln!("[service] {e}");
                    return;
                }
            }
        }
        self.workers.retain(|id, worker| {
            widgets
                .iter()
                .any(|w| &w.id == id && w.service && w.panel.as_ref() == Some(&worker.binary))
        });
        for w in widgets.iter().filter(|w| w.service && w.panel.is_some()) {
            if !self.workers.contains_key(&w.id) {
                match Worker::start(w, owner, self.global_job.as_ref().unwrap().0) {
                    Ok(worker) => {
                        self.workers.insert(w.id.clone(), worker);
                        self.foreground = (String::new(), String::new());
                    }
                    Err(e) => eprintln!("[service] {}: {e}", w.id),
                }
            }
        }
    }
    pub fn show(&self, id: &str) -> Result<()> {
        self.workers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("后台插件未启动"))?
            .input
            .try_send(HostEvent::Show)?;
        Ok(())
    }
    pub fn page(&self, id: &str) -> Option<(&Page, Option<&Page>, f64)> {
        let w = self.workers.get(id)?;
        if w.pages.is_empty() {
            return None;
        }
        let n = w.page_index % w.pages.len();
        let phase = w
            .flip
            .map(|t| (t.elapsed().as_secs_f64() / 0.18).min(1.0))
            .unwrap_or(1.0);
        Some((
            &w.pages[n],
            (w.pages.len() > 1 && phase < 1.0)
                .then(|| &w.pages[(n + w.pages.len() - 1) % w.pages.len()]),
            phase,
        ))
    }
    pub fn animating(&self) -> bool {
        self.workers
            .keys()
            .any(|id| self.page(id).is_some_and(|(_, old, _)| old.is_some()))
    }
    pub fn poll(
        &mut self,
        widgets: &[Widget],
        owner: isize,
        executable: String,
        title: String,
    ) -> bool {
        let fg = (executable, title);
        let accent = theme::hex(theme::accent_rgb());
        let mut changed = false;
        for w in widgets.iter().filter(|w| w.service) {
            let Some(worker) = self.workers.get_mut(&w.id) else {
                continue;
            };
            if worker.child.try_wait().ok().flatten().is_some()
                && worker.retries < 3
                && worker.started.elapsed().as_secs() >= 5u64 << worker.retries
            {
                let retries = worker.retries + 1;
                worker.retries = retries;
                if let Ok(mut replacement) =
                    Worker::start(w, owner, self.global_job.as_ref().unwrap().0)
                {
                    replacement.retries = retries;
                    *worker = replacement;
                    changed = true;
                }
            }
            if w.foreground_events && self.foreground != fg {
                let _ = worker.input.try_send(HostEvent::Foreground {
                    executable: fg.0.clone(),
                    title: fg.1.clone(),
                });
            }
            if self.accent != accent {
                let _ = worker.input.try_send(HostEvent::Theme {
                    accent: accent.clone(),
                });
            }
            for _ in 0..8 {
                let Ok(event) = worker.output.try_recv() else {
                    break;
                };
                match event {
                    PluginEvent::Ready => {
                        if w.foreground_events {
                            let _ = worker.input.try_send(HostEvent::Foreground {
                                executable: fg.0.clone(),
                                title: fg.1.clone(),
                            });
                        }
                    }
                    PluginEvent::Widget { pages } if valid_pages(&pages) => {
                        if worker.pages != pages {
                            let id = worker.pages.get(worker.page_index).map(|p| p.id.clone());
                            worker.page_index = id
                                .and_then(|id| pages.iter().position(|p| p.id == id))
                                .unwrap_or(0);
                            worker.pages = pages;
                            changed = true;
                        }
                    }
                    PluginEvent::Open {
                        path,
                        select_parent,
                    } if w.shell_open && path.len() <= 32768 => {
                        let _ = open_external(&path, select_parent);
                    }
                    _ => {}
                }
            }
            if worker.pages.len() > 1 && worker.rotated.elapsed().as_secs() >= 5 {
                worker.page_index = (worker.page_index + 1) % worker.pages.len();
                worker.rotated = Instant::now();
                worker.flip = Some(Instant::now());
                changed = true;
            }
        }
        self.foreground = fg;
        self.accent = accent;
        changed
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        for w in self.workers.values() {
            let _ = w.input.try_send(HostEvent::Shutdown);
        }
        if !self.workers.is_empty() {
            std::thread::sleep(Duration::from_millis(150));
        }
        // One shared deadline, even if many plugins ignore shutdown.
        for w in self.workers.values_mut() {
            w.job.take();
            let _ = w.child.wait();
        }
    }
}
pub fn smoke(directory: &Path) -> Result<()> {
    ensure!(
        directory.is_absolute(),
        "Absolute output directory required"
    );
    std::fs::create_dir_all(directory)?;
    crate::data_paths::set(directory.join("profile"))?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins");
    let widgets = ["everything-search", "agent-usage"]
        .iter()
        .map(|name| crate::plugins::validate_directory(&root.join(name)))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        widgets.iter().all(|w| w.panel.is_some()),
        "Build native plugins first"
    );
    let mut manager = Manager::default();
    manager.sync(&widgets, 0);
    ensure!(
        manager.workers.len() == 2,
        "Both service processes must start"
    );
    if std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR").is_some() {
        for widget in &widgets {
            manager.show(&widget.id)?;
        }
    }
    let mut before = 0i64;
    let multiple = std::env::args().any(|a| a == "--simulate-multiple-agents");
    let mut observed_animation = false;
    for i in 0..10 {
        manager.poll(
            &widgets,
            0,
            if multiple && i == 2 {
                "Claude.exe"
            } else {
                "Codex.exe"
            }
            .into(),
            "Codex".into(),
        );
        observed_animation |= manager.animating();
        if i == 6 {
            before = manager.cpu_time();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    manager.poll(&widgets, 0, "Codex.exe".into(), "Codex".into());
    let idle_cpu_cores = (manager.cpu_time() - before) as f64 / 10_000_000.0 / 4.0;
    let pages: Vec<_> = manager
        .workers
        .iter()
        .map(|(id, w)| serde_json::json!({"id":id,"pid":w.child.id(),"pages":w.pages}))
        .collect();
    let pids: Vec<_> = manager.workers.values().map(|w| w.child.id()).collect();
    if multiple {
        ensure!(
            manager.workers.values().any(|w| w.pages.len() == 2) && observed_animation,
            "Multiple active agents did not rotate"
        );
    }
    drop(manager);
    let mut remaining = Vec::new();
    for pid in pids {
        unsafe {
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if !process.is_null() {
                if WaitForSingleObject(process, 0) != WAIT_OBJECT_0 {
                    remaining.push(pid);
                }
                CloseHandle(process);
            }
        }
    }
    std::fs::write(
        directory.join("services.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"plugins":pages,"idle_cpu_cores":idle_cpu_cores,"aggregate_cpu_cap_percent":10,"per_plugin_cpu_cap_percent":5,"remaining_owned_processes":remaining,"simulated_activity":multiple,"observed_rotation":observed_animation}),
        )?,
    )?;
    ensure!(
        remaining.is_empty(),
        "Service processes survived host shutdown"
    );
    Ok(())
}
impl Manager {
    fn cpu_time(&self) -> i64 {
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            if self.global_job.as_ref().is_some_and(|job| {
                QueryInformationJobObject(
                    job.0 as _,
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as _,
                    std::ptr::null_mut(),
                ) != 0
            }) {
                info.TotalKernelTime + info.TotalUserTime
            } else {
                0
            }
        }
    }
}
fn open_external(path: &str, select_parent: bool) -> Result<()> {
    ensure!(
        Path::new(path).is_absolute() && !path.contains('\0'),
        "Invalid file path"
    );
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["--shell-open", path]);
    if select_parent {
        command.arg("--select-parent");
    }
    // The helper is outside plugin and development jobs. Disabling a plugin cannot kill an opened editor.
    command
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()?;
    Ok(())
}
pub fn shell_open(path: &str, select_parent: bool) -> Result<()> {
    ensure!(
        Path::new(path).is_absolute() && !path.contains('\0') && !path.contains('"'),
        "Invalid file path"
    );
    unsafe {
        let result = if select_parent {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide("open").as_ptr(),
                wide("explorer.exe").as_ptr(),
                wide(&format!("/select,\"{path}\"")).as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } else {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide("open").as_ptr(),
                wide(path).as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        ensure!(result as isize > 32, "无法打开文件");
    }
    Ok(())
}
