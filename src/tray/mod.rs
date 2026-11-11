mod native;
mod observer;
mod popup;
mod registry;
mod store;
use anyhow::Result;
pub use native::diagnose_details;
pub use native::{Click, event};
pub use observer::smoke as sync_smoke;
pub use popup::Popup;
use std::sync::{
    Arc,
    atomic::Ordering,
    mpsc::{self, Receiver, SyncSender},
};

pub struct Snapshot {
    pub items: Vec<native::Item>,
    pub preferences: store::Preferences,
    pub status: String,
    pub offer_import: bool,
}
impl Snapshot {
    pub fn empty() -> Self {
        Self {
            items: vec![],
            preferences: store::Preferences::default(),
            status: String::new(),
            offer_import: false,
        }
    }
    pub fn pinned(&self) -> Vec<usize> {
        let mut indices: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                !self
                    .preferences
                    .collapsed_or(&item.preference_key, item.collapsed)
            })
            .map(|(index, _)| index)
            .collect();
        indices.sort_by_key(|index| {
            self.preferences
                .entries
                .iter()
                .position(|entry| entry.id == self.items[*index].preference_key)
                .unwrap_or(usize::MAX)
        });
        indices
    }
}

#[derive(Clone)]
pub enum Command {
    Refresh,
    Import,
    SetCollapsed(String, bool),
    Click(String, Click),
    FramesChanged,
    Stop,
}
pub struct Host {
    pub sender: SyncSender<Command>,
    pub updates: Receiver<Arc<Snapshot>>,
    _observer: observer::Subscription,
}
impl Host {
    pub fn start(owner: isize, message: u32) -> Self {
        let (sender, requests) = mpsc::sync_channel(32);
        let (outgoing, updates) = mpsc::sync_channel(1);
        let (subscription, frames) = observer::start(sender.clone());
        std::thread::spawn(move || {
            let loaded = store::Preferences::load();
            let can_save = loaded.is_ok();
            let mut preferences = loaded.unwrap_or_else(|error| {
                eprintln!("[tray] settings: {error:#}");
                store::Preferences::default()
            });
            // Persist before offering, so Skip and development restarts cannot
            // turn this into a recurring empty-state prompt.
            let mut offer_import =
                can_save && preferences.entries.is_empty() && !preferences.import_prompt_shown;
            if offer_import {
                preferences.import_prompt_shown = true;
                if let Err(error) = preferences.save() {
                    eprintln!("[tray] initial prompt state: {error:#}");
                    offer_import = false;
                }
            }
            let mut current = Arc::new(Snapshot::empty());
            let mut command = Command::Refresh;
            loop {
                if matches!(command, Command::Stop) || frames.stopped() {
                    break;
                }
                if matches!(command, Command::FramesChanged) {
                    let mut items = current.items.clone();
                    if frames.apply(&mut items) {
                        current = Arc::new(Snapshot {
                            items,
                            preferences: current.preferences.clone(),
                            status: current.status.clone(),
                            offer_import: false,
                        });
                        if outgoing.send(current.clone()).is_err() {
                            break;
                        }
                        unsafe {
                            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                                owner as _, message, 0, 0,
                            );
                        }
                    }
                    match requests.recv() {
                        Ok(next) => command = next,
                        Err(_) => break,
                    }
                    continue;
                }
                frames.reconnect_if_changed();
                let mut status = String::new();
                if let Command::Click(ref key, click) = command
                    && let Some(item) = current.items.iter().find(|item| item.key == *key)
                    && let Err(error) = native::relay(&item.endpoint, click)
                {
                    eprintln!("[tray] click: {error:#}");
                    status = "图标暂不可用，请刷新后重试".into();
                }
                let mut items = native::enumerate().unwrap_or_else(|error| {
                    eprintln!("[tray] enumeration: {error:#}");
                    status = "暂时无法读取原生托盘图标".into();
                    vec![]
                });
                frames.apply(&mut items);
                let from_live = |collapsed_override: Option<bool>| {
                    items.iter().map(move |item| store::Preference {
                        id: item.preference_key.clone(),
                        executable: item.executable.clone(),
                        uid: item.uid,
                        title: item.title.clone(),
                        collapsed: collapsed_override.unwrap_or(item.collapsed),
                    })
                };
                let mut next = preferences.clone();
                let mut save = false;
                match &command {
                    Command::Import => {
                        let mut incoming: Vec<_> = from_live(None).collect();
                        let mut partial = false;
                        match registry::read() {
                            Ok(registry) => incoming.extend(registry),
                            Err(error) => {
                                eprintln!("[tray] import: {error:#}");
                                partial = true;
                            }
                        }
                        let added = next.merge(incoming);
                        if !next.entries.is_empty() {
                            save = true;
                            status = if partial {
                                format!("新增 {added} 项；部分系统设置不可读")
                            } else {
                                format!("新增 {added} 项，保留已有选择")
                            };
                        } else {
                            status = "未找到可导入的系统托盘设置，请刷新后重试".into();
                        }
                    }
                    Command::SetCollapsed(key, collapsed) => {
                        if let Some(entry) = next.entries.iter_mut().find(|entry| entry.id == *key)
                        {
                            entry.collapsed = *collapsed;
                            save = true;
                        } else if let Some(item) = items
                            .iter()
                            .chain(current.items.iter())
                            .find(|item| item.preference_key == *key)
                        {
                            next.merge([store::Preference {
                                id: item.preference_key.clone(),
                                executable: item.executable.clone(),
                                uid: item.uid,
                                title: item.title.clone(),
                                collapsed: *collapsed,
                            }]);
                            save = true;
                        }
                    }
                    _ => {
                        if !next.entries.is_empty() {
                            save = next.merge(from_live(None)) > 0;
                        }
                    }
                }
                if save {
                    if !can_save {
                        status = "本地托盘设置无法读取，未覆盖原文件".into();
                    } else {
                        match next.save() {
                            Ok(()) => preferences = next,
                            Err(error) => {
                                eprintln!("[tray] save: {error:#}");
                                status = "托盘设置保存失败，请重试".into();
                            }
                        }
                    }
                }
                if status.is_empty() && native::SKIPPED.load(Ordering::Relaxed) > 0 {
                    status = "部分图标暂不可用".into();
                }
                current = Arc::new(Snapshot {
                    items,
                    preferences: preferences.clone(),
                    status,
                    offer_import: std::mem::take(&mut offer_import),
                });
                if outgoing.send(current.clone()).is_err() {
                    break;
                }
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                        owner as _, message, 0, 0,
                    );
                }
                match requests.recv() {
                    Ok(next) => command = next,
                    Err(_) => break,
                }
            }
        });
        Self {
            sender,
            updates,
            _observer: subscription,
        }
    }
    pub fn refresh_if_dirty(&self) {
        if native::DIRTY.swap(false, Ordering::Relaxed) {
            let _ = self.sender.try_send(Command::Refresh);
        }
    }
    pub fn send(&self, command: Command) {
        let _ = self.sender.try_send(command);
    }
}

pub fn diagnose() -> Result<()> {
    match native::enumerate() {
        Ok(items) => println!(
            "tray: supported=true live={} native_visible={} native_collapsed={}",
            items.len(),
            items.iter().filter(|item| !item.collapsed).count(),
            items.iter().filter(|item| item.collapsed).count()
        ),
        Err(error) => println!("tray: supported=false error={error:#}"),
    }
    let saved = store::Preferences::load()?;
    println!(
        "tray: saved_preferences={} (read-only diagnostic)",
        saved.entries.len()
    );
    println!(
        "tray: skipped={} registry_preferences={} (no input or settings writes)",
        native::SKIPPED.load(Ordering::Relaxed),
        registry::read()?.len()
    );
    Ok(())
}
