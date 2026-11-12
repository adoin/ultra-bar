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
#[cfg(test)]
mod cpu_budget_tests {
    use super::*;
    #[test]
    fn nested_limits_keep_the_intended_budget_instead_of_multiplying_percentages() {
        assert_eq!(relative_cpu(10, 20), 50);
        assert_eq!(relative_cpu(5, 10), 50);
        assert_eq!(20 * relative_cpu(10, 20) * relative_cpu(5, 10) / 10000, 5);
        for parent in 1..=100 {
            let global = parent.min(10);
            assert!(parent * relative_cpu(10, parent) / 100 <= 10);
            assert!(global * relative_cpu(5, global) / 100 <= 5);
        }
    }
}
fn relative_cpu(desired: u32, parent: u32) -> u32 {
    (desired * 100 / parent.max(1)).clamp(1, 100)
}
fn inherited_cpu() -> u32 {
    unsafe {
        let mut rate: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION = zeroed();
        if QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectCpuRateControlInformation,
            (&mut rate as *mut JOBOBJECT_CPU_RATE_CONTROL_INFORMATION).cast(),
            size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as _,
            std::ptr::null_mut(),
        ) != 0
            && rate.ControlFlags & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE != 0
            && rate.ControlFlags & JOB_OBJECT_CPU_RATE_CONTROL_WEIGHT_BASED == 0
        {
            rate.Anonymous.CpuRate.div_ceil(100).clamp(1, 100)
        } else {
            100
        }
    }
}
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
    settings: bool,
    exit_logged: bool,
}
impl Drop for Worker {
    fn drop(&mut self) {
        ultra_bar_native_ui::diagnostics::event(
            "plugins",
            "service.stop_requested",
            serde_json::json!({"pid":self.child.id(),"binary":self.binary}),
        );
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
    fn start(widget: &Widget, owner: isize, global: isize, global_cpu: u32) -> Result<Self> {
        let binary = widget
            .panel
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("插件尚未构建"))?;
        let job = Job::new(relative_cpu(5, global_cpu), 384 * 1024 * 1024)?;
        let mut child = Command::new(binary)
            .args(["--service", "--owner", &owner.to_string()])
            .current_dir(&widget.root)
            .env("ULTRA_BAR_ACCENT", theme::hex(theme::accent_rgb()))
            .env(
                "ULTRA_BAR_LOG_DIR",
                ultra_bar_native_ui::diagnostics::root().unwrap_or(Path::new("logs")),
            )
            .env(
                "ULTRA_BAR_PARENT_SESSION",
                ultra_bar_native_ui::diagnostics::session().unwrap_or(""),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let stderr = child.stderr.take().unwrap();
        let diagnostic_id = widget.id.clone();
        let diagnostic_pid = child.id();
        ultra_bar_native_ui::diagnostics::event(
            "plugins",
            "service.start",
            serde_json::json!({"id":widget.id,"pid":child.id(),"binary":binary}),
        );
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                let chunk = match reader.fill_buf() {
                    Ok(chunk) if !chunk.is_empty() => chunk,
                    _ => break,
                };
                let end = chunk.iter().position(|b| *b == b'\n');
                let count = end.map_or(chunk.len(), |n| n + 1);
                let remaining = 8192usize.saturating_sub(line.len());
                line.extend_from_slice(&chunk[..count.min(remaining)]);
                reader.consume(count);
                if end.is_some() {
                    ultra_bar_native_ui::diagnostics::event(
                        "plugins",
                        "service.stderr",
                        serde_json::json!({"id":diagnostic_id,"pid":diagnostic_pid,"message":String::from_utf8_lossy(&line)}),
                    );
                    line.clear();
                }
            }
            if !line.is_empty() {
                ultra_bar_native_ui::diagnostics::event(
                    "plugins",
                    "service.stderr",
                    serde_json::json!({"id":diagnostic_id,"pid":diagnostic_pid,"message":String::from_utf8_lossy(&line)}),
                );
            }
        });
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
            host: owner as usize,
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
            settings: false,
            exit_logged: false,
        })
    }
}
#[derive(Default)]
pub struct Manager {
    workers: HashMap<String, Worker>,
    global_job: Option<Job>,
    foreground: (String, String),
    accent: String,
    global_cpu: u32,
}
impl Manager {
    pub fn diagnostics(&mut self) -> serde_json::Value {
        let workers:Vec<_>=self.workers.iter_mut().map(|(id,worker)|serde_json::json!({"id":id,"pid":worker.child.id(),"exited":worker.child.try_wait().ok().flatten().map(|s|s.code()),"retries":worker.retries,"pages":worker.pages})).collect();
        serde_json::json!({"foreground_executable":self.foreground.0,"workers":workers})
    }
    pub fn sync(&mut self, widgets: &[Widget], owner: isize) {
        if self.global_job.is_none() && widgets.iter().any(|w| w.service) {
            let parent = inherited_cpu();
            self.global_cpu = parent.min(10);
            match Job::new(relative_cpu(10, parent), 1024 * 1024 * 1024) {
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
                match Worker::start(
                    w,
                    owner,
                    self.global_job.as_ref().unwrap().0,
                    self.global_cpu,
                ) {
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
        let worker = self
            .workers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("后台插件未启动"))?;
        unsafe {
            AllowSetForegroundWindow(worker.child.id());
        }
        worker.input.try_send(HostEvent::Show)?;
        Ok(())
    }
    pub fn configure(&self, id: &str) -> Result<()> {
        let worker = self
            .workers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("后台插件未启动"))?;
        ensure!(worker.settings, "此插件未提供设置入口");
        unsafe {
            AllowSetForegroundWindow(worker.child.id());
        }
        worker.input.try_send(HostEvent::Settings)?;
        Ok(())
    }
    pub fn save_settings(&self, id: &str, revision: u64, values: Vec<String>) -> Result<()> {
        ultra_bar_native_ui::diagnostics::event(
            "settings",
            "plugin.save_requested",
            serde_json::json!({"id":id,"revision":revision,"field_count":values.len()}),
        );
        let worker = self
            .workers
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("后台插件未启动"))?;
        ensure!(worker.settings, "此插件未提供设置入口");
        let event = HostEvent::SettingsValues { revision, values };
        ensure!(
            serde_json::to_vec(&event)?.len() < ultra_bar_plugin_sdk::MAX_LINE,
            "设置内容过长"
        );
        worker.input.try_send(event)?;
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
            if let Ok(Some(status)) = worker.child.try_wait()
                && !worker.exit_logged
            {
                worker.exit_logged = true;
                ultra_bar_native_ui::diagnostics::event(
                    "plugins",
                    "service.exit",
                    serde_json::json!({"id":w.id,"pid":worker.child.id(),"exit_code":status.code(),"retries":worker.retries}),
                );
            }
            if worker.child.try_wait().ok().flatten().is_some()
                && worker.retries < 3
                && worker.started.elapsed().as_secs() >= 5u64 << worker.retries
            {
                let retries = worker.retries + 1;
                worker.retries = retries;
                if let Ok(mut replacement) = Worker::start(
                    w,
                    owner,
                    self.global_job.as_ref().unwrap().0,
                    self.global_cpu,
                ) {
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
                    PluginEvent::Capabilities { settings } => {
                        worker.settings = settings;
                    }
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
    let mut provider = unsafe { crate::editors::TestProvider::new(&widgets)? };
    let mut manager = Manager::default();
    manager.sync(&widgets, provider.hwnd as isize);
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
            provider.hwnd as isize,
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
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            unsafe {
                provider.pump();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    manager.poll(
        &widgets,
        provider.hwnd as isize,
        "Codex.exe".into(),
        "Codex".into(),
    );
    unsafe {
        provider.verify_mount(directory)?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            provider.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
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
    provider.close_mounts();
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
    ultra_bar_native_ui::diagnostics::event(
        "input",
        "file.open_requested",
        serde_json::json!({"path":path,"select_parent":select_parent}),
    );
    ensure!(
        (Path::new(path).is_absolute() || packaged_app_target(path)) && !path.contains('\0'),
        "Invalid file path"
    );
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(root) = ultra_bar_native_ui::diagnostics::root() {
        command.env("ULTRA_BAR_LOG_DIR", root);
    }
    if let Some(session) = ultra_bar_native_ui::diagnostics::session() {
        command.env("ULTRA_BAR_PARENT_SESSION", session);
    }
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
fn packaged_app_target(path: &str) -> bool {
    let Some(id) = path.strip_prefix("shell:AppsFolder\\") else {
        return false;
    };
    let Some((package, app)) = id.split_once('!') else {
        return false;
    };
    id.len() <= 512
        && package.contains('_')
        && !app.is_empty()
        && !app.contains('!')
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'!'))
}
pub fn shell_open(path: &str, select_parent: bool) -> Result<()> {
    ensure!(
        (Path::new(path).is_absolute() || packaged_app_target(path))
            && !path.contains('\0')
            && !path.contains('"'),
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

#[cfg(test)]
mod app_target_tests {
    use super::*;
    #[test]
    fn allows_only_a_single_packaged_app_target() {
        assert!(packaged_app_target(
            "shell:AppsFolder\\OpenAI.Codex_2p2nqsd0c76g0!App"
        ));
        for bad in [
            "shell:startup",
            "shell:AppsFolder\\App",
            "shell:AppsFolder\\P_x!App /arg",
            "shell:AppsFolder\\P_x!App\\..",
            "shell:AppsFolder\\P_x!App!Other",
        ] {
            assert!(!packaged_app_target(bad));
        }
    }
}
