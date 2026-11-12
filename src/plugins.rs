use anyhow::{Context, Result, bail};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rune::{Context as RuneContext, Source, Sources, Vm};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

const API_VERSION: u32 = 1;
const MAX_SOURCE_BYTES: u64 = 64 * 1024;
const MAX_PLUGINS: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub id: String,
    pub text: String,
    pub action: String,
    pub width: i32,
    pub slot: Slot,
    pub panel: Option<PathBuf>,
    pub root: PathBuf,
    pub name: String,
    pub background: bool,
    pub icon: Option<String>,
    pub service: bool,
    pub shell_open: bool,
    pub foreground_events: bool,
}

#[derive(Clone, Copy, Default, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    #[default]
    Taskbar,
    Clock,
    Leading,
    Tray,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Panel {
    runtime: String,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    binary: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    #[serde(default = "default_version")]
    version: String,
    api_version: u32,
    entry: String,
    width: i32,
    #[serde(default)]
    slot: Slot,
    #[serde(default)]
    name: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    panel: Option<Panel>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    service: bool,
}
fn default_version() -> String {
    "0.0.0".into()
}

struct Plugin {
    manifest: Manifest,
    vm: Vm,
    clicks: i64,
    root: PathBuf,
}

impl Plugin {
    fn render(&mut self) -> Result<Widget> {
        let value = rune::alloc::limit::with(8 * 1024 * 1024, || {
            rune::runtime::budget::with(25_000, || self.vm.call(["render"], (self.clicks,))).call()
        })
        .call()?;
        let (text, action): (String, String) = rune::from_value(value)?;
        if self
            .manifest
            .icon
            .as_ref()
            .is_some_and(|icon| icon.chars().count() > 1)
        {
            bail!("clock icon must be one glyph or empty");
        }
        if text.chars().count() > 80
            || !matches!(
                action.as_str(),
                "none" | "start" | "notifications" | "settings" | "panel"
            )
        {
            bail!("invalid widget text or unsupported action");
        }
        if action == "panel"
            && (!self
                .manifest
                .capabilities
                .iter()
                .any(|capability| capability == "panel")
                || self.manifest.panel.is_none())
        {
            bail!("panel action requires a declared panel capability");
        }
        let panel = if let Some(spec) = &self.manifest.panel {
            let pointer = self.root.join(&spec.runtime);
            if pointer.exists() {
                let pointer = pointer.canonicalize()?;
                if !pointer.starts_with(&self.root) {
                    bail!("panel runtime escapes plugin directory");
                }
                #[derive(Deserialize)]
                struct Runtime {
                    executable: String,
                }
                let runtime: Runtime = serde_json::from_str(&read_small(&pointer)?)?;
                let binary = pointer
                    .parent()
                    .context("invalid runtime pointer")?
                    .join(runtime.executable)
                    .canonicalize()?;
                if !binary.starts_with(&self.root)
                    || binary
                        .extension()
                        .is_none_or(|extension| extension != "exe")
                {
                    bail!("invalid panel executable");
                }
                Some(binary)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Widget {
            id: self.manifest.id.clone(),
            text,
            action,
            width: self.manifest.width,
            slot: self.manifest.slot,
            panel,
            root: self.root.clone(),
            name: if self.manifest.name.is_empty() {
                self.manifest.id.clone()
            } else {
                self.manifest.name.clone()
            },
            background: self
                .manifest
                .panel
                .as_ref()
                .is_some_and(|panel| panel.background),
            icon: self.manifest.icon.clone(),
            service: self.manifest.service,
            shell_open: self.manifest.capabilities.iter().any(|c| c == "shell.open"),
            foreground_events: self.manifest.capabilities.iter().any(|c| c == "foreground"),
        })
    }
}

enum Event {
    Changed,
    Click(String),
}

pub struct Host {
    events: SyncSender<Event>,
    pub updates: Receiver<Vec<Widget>>,
}

impl Host {
    pub fn start(
        root: PathBuf,
        hwnd: isize,
        reload_message: u32,
        prefer_development: bool,
    ) -> Self {
        let (events, receiver) = mpsc::sync_channel(32);
        let (updates_tx, updates) = mpsc::sync_channel(1);
        let watcher_events = events.clone();
        std::thread::spawn(move || {
            let mut watcher: Option<RecommendedWatcher> = notify::recommended_watcher(
                move |event: notify::Result<notify::Event>| match event {
                    Ok(event)
                        if !matches!(event.kind, notify::EventKind::Access(_))
                            && event.paths.iter().any(|path| {
                                path.extension().is_none_or(|extension| {
                                    extension == "rn" || extension == "json"
                                })
                            }) =>
                    {
                        let _ = watcher_events.try_send(Event::Changed);
                    }
                    Err(error) => eprintln!("[plugins] watcher: {error}"),
                    _ => {}
                },
            )
            .ok();
            if let Some(watcher) = watcher.as_mut()
                && let Err(error) = watcher.watch(&root, RecursiveMode::Recursive)
            {
                eprintln!("[plugins] cannot watch {}: {error}", root.display());
            }
            let mut plugins = load_all(&root, Vec::new(), prefer_development);
            publish(&mut plugins, &updates_tx, hwnd, reload_message);
            while let Ok(event) = receiver.recv() {
                match event {
                    Event::Click(id) => {
                        if let Some(plugin) = plugins.iter_mut().find(|p| p.manifest.id == id) {
                            plugin.clicks = plugin.clicks.saturating_add(1);
                        }
                    }
                    Event::Changed => {
                        // Coalesce editor saves; never compile on the window's thread.
                        loop {
                            match receiver.recv_timeout(Duration::from_millis(350)) {
                                Ok(Event::Changed) => {}
                                Ok(Event::Click(id)) => {
                                    if let Some(p) =
                                        plugins.iter_mut().find(|p| p.manifest.id == id)
                                    {
                                        p.clicks = p.clicks.saturating_add(1);
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        plugins = load_all(&root, plugins, prefer_development);
                    }
                }
                publish(&mut plugins, &updates_tx, hwnd, reload_message);
            }
        });
        Self { events, updates }
    }

    pub fn click(&self, id: &str) {
        let _ = self.events.try_send(Event::Click(id.to_owned()));
    }
    pub fn refresh(&self) {
        let _ = self.events.try_send(Event::Changed);
    }
}

fn publish(plugins: &mut [Plugin], sender: &SyncSender<Vec<Widget>>, hwnd: isize, message: u32) {
    let widgets = plugins
        .iter_mut()
        .filter_map(|p| match p.render() {
            Ok(widget) => Some(widget),
            Err(error) => {
                eprintln!("[plugins] {}: {error}", p.manifest.id);
                None
            }
        })
        .collect();
    // Backpressure also prevents a noisy plugin from filling the UI message queue.
    if sender.send(widgets).is_ok() {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(hwnd as _, message, 0, 0);
        }
    }
}

fn load_all(root: &Path, old: Vec<Plugin>, prefer_development: bool) -> Vec<Plugin> {
    let mut old = old;
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let mut dirs: Vec<_> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect(),
        Err(error) => {
            eprintln!("[plugins] {}: {error}", root.display());
            vec![]
        }
    };
    dirs.sort();
    match crate::plugin_system::model::Paths::user().and_then(|paths| paths.active_directories()) {
        Ok(mut installed) => {
            if prefer_development {
                dirs.extend(installed);
            } else {
                installed.extend(dirs);
                dirs = installed;
            }
        }
        Err(error) => eprintln!("[plugins] installed registry: {error:#}"),
    }
    for dir in dirs.into_iter().take(MAX_PLUGINS) {
        let manifest_path = dir.join("plugin.json");
        if !manifest_path.is_file() {
            continue;
        }
        match load(&dir) {
            Ok(mut plugin) => {
                if !seen.insert(plugin.manifest.id.clone()) {
                    eprintln!("[plugins] duplicate id: {}", plugin.manifest.id);
                    continue;
                }
                if let Some(previous) = old.iter().find(|p| p.manifest.id == plugin.manifest.id) {
                    plugin.clicks = previous.clicks;
                }
                // Validate before replacing the last working plugin.
                if let Err(error) = plugin.render() {
                    eprintln!("[plugins] {}: {error}", dir.display());
                    if let Some(index) =
                        old.iter().position(|p| p.manifest.id == plugin.manifest.id)
                    {
                        output.push(old.remove(index));
                    }
                } else {
                    output.push(plugin);
                }
            }
            Err(error) => {
                eprintln!("[plugins] {}: {error:#}", dir.display());
                // A half-written or invalid save keeps the last working version for this directory.
                if let Some(index) = old
                    .iter()
                    .position(|p| p.root == dir.canonicalize().unwrap_or_else(|_| dir.clone()))
                {
                    let previous = old.remove(index);
                    if seen.insert(previous.manifest.id.clone()) {
                        output.push(previous);
                    }
                }
            }
        }
    }
    eprintln!("[plugins] loaded {} plugin(s)", output.len());
    ultra_bar_native_ui::diagnostics::event(
        "plugins",
        "loaded",
        serde_json::json!({"plugins":output.iter().map(|p|serde_json::json!({"id":p.manifest.id,"version":p.manifest.version,"root":p.root})).collect::<Vec<_>>()}),
    );
    output
}

fn read_small(path: &Path) -> Result<String> {
    if std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
        bail!("file exceeds 64 KiB");
    }
    Ok(std::fs::read_to_string(path)?)
}

fn load(dir: &Path) -> Result<Plugin> {
    let manifest: Manifest = serde_json::from_str(&read_small(&dir.join("plugin.json"))?)?;
    if ![API_VERSION, 2, 3].contains(&manifest.api_version)
        || !(if manifest.api_version == 3 { 16 } else { 64 }..=320).contains(&manifest.width)
        || !(crate::plugin_system::model::valid_id(&manifest.id)
            || manifest.id == dir.file_name().unwrap_or_default().to_string_lossy())
    {
        bail!("invalid plugin API version, width or stable id");
    }
    semver::Version::parse(&manifest.version)?;
    if manifest.api_version == 3 && !crate::plugin_system::model::valid_id(&manifest.id) {
        bail!("API 3 requires a stable reverse-domain id");
    }
    if let Some(binary) = manifest
        .panel
        .as_ref()
        .and_then(|panel| panel.binary.as_ref())
        && (binary.is_empty()
            || !binary.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte)
            }))
    {
        bail!("invalid native binary name");
    }
    if manifest.api_version == 1 && (manifest.slot != Slot::Taskbar || manifest.panel.is_some()) {
        bail!("clock slots and native panels require api_version=2");
    }
    if (manifest.service || matches!(manifest.slot, Slot::Leading | Slot::Tray))
        && manifest.api_version != 3
    {
        bail!("resident services and leading/tray slots require API 3");
    }
    if manifest.service
        && (!manifest.capabilities.iter().any(|c| c == "service")
            || !manifest.panel.as_ref().is_some_and(|p| p.background))
    {
        bail!("service requires capability and a background native binary");
    }
    if manifest.panel.is_some()
        && !manifest
            .capabilities
            .iter()
            .any(|capability| capability == "panel")
    {
        bail!("native panel requires the panel capability");
    }
    let root = dir.canonicalize()?;
    let entry = dir.join(&manifest.entry).canonicalize()?;
    if !entry.starts_with(&root) {
        bail!("entry escapes plugin directory");
    }
    let source = read_small(&entry)?;
    let context = RuneContext::with_default_modules()?;
    let mut sources = Sources::new();
    sources.insert(Source::memory(&source)?)?;
    let mut diagnostics = rune::Diagnostics::new();
    let unit = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build()
        .map_err(|error| {
            anyhow::anyhow!(
                "Rune compilation failed: {error}; {:?}",
                diagnostics.diagnostics()
            )
        })?;
    let vm = Vm::new(
        std::sync::Arc::new(context.runtime()?),
        std::sync::Arc::new(unit),
    );
    Ok(Plugin {
        manifest,
        vm,
        clicks: 0,
        root,
    })
}
pub(crate) fn validate_directory(root: &Path) -> Result<Widget> {
    load(root)?.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn example_renders_and_changes_on_click() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins/examples/counter");
        let mut plugin = load(&dir).unwrap();
        assert_eq!(plugin.render().unwrap().text, "Clicks: 0");
        plugin.clicks = 3;
        assert_eq!(plugin.render().unwrap().text, "Clicks: 3");
    }
    #[test]
    fn calendar_is_a_clock_extension_with_declared_native_panel_permission() {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins/schedule-calendar");
        let mut plugin = load(&dir).unwrap();
        let widget = plugin.render().unwrap();
        assert_eq!(widget.slot, Slot::Clock);
        assert_eq!(widget.text, "{time}");
        assert_eq!(widget.action, "panel");
        assert!(widget.background);
    }
    #[test]
    fn resident_plugins_declare_distinct_slots_and_minimal_capabilities() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins");
        let search = load(&root.join("everything-search"))
            .unwrap()
            .render()
            .unwrap();
        let usage = load(&root.join("agent-usage")).unwrap().render().unwrap();
        assert_eq!(search.slot, Slot::Leading);
        assert!(search.service && search.shell_open && !search.foreground_events);
        assert_eq!(usage.slot, Slot::Tray);
        assert!(usage.service && usage.foreground_events && !usage.shell_open);
    }
    #[test]
    fn infinite_loop_exhausts_budget() {
        let context = RuneContext::with_default_modules().unwrap();
        let mut sources = Sources::new();
        sources
            .insert(Source::memory("pub fn render() { loop {} }").unwrap())
            .unwrap();
        let unit = rune::prepare(&mut sources)
            .with_context(&context)
            .build()
            .unwrap();
        let mut vm = Vm::new(
            std::sync::Arc::new(context.runtime().unwrap()),
            std::sync::Arc::new(unit),
        );
        assert!(
            rune::runtime::budget::with(1000, || vm.call(["render"], ()))
                .call()
                .is_err()
        );
    }
}
