use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
fn file_presence(value: &Value) -> Value {
    match value.as_str() {
        Some(path) if path.starts_with("appx:") || path.starts_with("shell:AppsFolder\\") => {
            Value::Null
        }
        Some(path) => json!(Path::new(path).exists()),
        None => Value::Null,
    }
}
fn snapshot(path: &Path) -> Value {
    if !path.exists() {
        return json!({"exists":false});
    }
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    if size > 1024 * 1024 {
        return json!({"exists":true,"bytes":size,"oversized":true});
    }
    let bytes = std::fs::read(path).unwrap_or_default();
    let parsed = serde_json::from_slice::<Value>(&bytes).ok();
    match path.file_name().and_then(|n| n.to_str()) {
        Some("taskbar-apps.json") | Some("taskbar-scripts.json") => {
            json!({"exists":true,"pins":parsed.as_ref().and_then(Value::as_array).map(|a|a.iter().map(|p|json!({"name":p["name"],"executable":p["executable"],"launch":p["launch"],"target_kind":if p["executable"].as_str().is_some_and(|s|s.starts_with("appx:")){"packaged_app"}else{"file"},"launch_exists":file_presence(&p["launch"]),"executable_exists":file_presence(&p["executable"])})).collect::<Vec<_>>()),"bytes":bytes.len()})
        }
        Some("settings.json") => json!({"exists":true,"settings":parsed,"bytes":bytes.len()}),
        Some("tray.json") => json!({"exists":true,"bytes":bytes.len(),"preferences":parsed}),
        _ => json!({"exists":true,"bytes":bytes.len()}),
    }
}
pub fn start() -> Option<RecommendedWatcher> {
    let root = crate::data_paths::root().ok()?;
    let _ = std::fs::create_dir_all(&root);
    let mut initial = HashMap::new();
    for name in [
        "settings.json",
        "taskbar-apps.json",
        "taskbar-scripts.json",
        "tray.json",
    ] {
        let path = root.join(name);
        let state = snapshot(&path);
        ultra_bar_native_ui::diagnostics::event(
            "config",
            "startup.snapshot",
            json!({"file":name,"state":state}),
        );
        initial.insert(path, state);
    }
    let previous = Arc::new(Mutex::new(initial));
    let mut watcher=notify::recommended_watcher(move|event:notify::Result<notify::Event>|{
        let Ok(event)=event else{return;};
        for path in event.paths {
            let name=path.file_name().and_then(|n|n.to_str()).unwrap_or("");
            if ["settings.json","taskbar-apps.json","taskbar-scripts.json","tray.json"].contains(&name) {
                let next=snapshot(&path);let Ok(mut states)=previous.lock() else{return;};let old=states.insert(path.clone(),next.clone());
                if old.as_ref()!=Some(&next){ultra_bar_native_ui::diagnostics::event("config","file.changed",json!({"file":name,"kind":format!("{:?}",event.kind),"before":old,"after":next}));}
            }else if path.extension().is_some_and(|s|s.eq_ignore_ascii_case("lnk")) {
                ultra_bar_native_ui::diagnostics::event("tasks","shortcut.file_changed",json!({"path":path,"kind":format!("{:?}",event.kind),"exists":path.exists()}));
            }
        }
    }).ok()?;
    if watcher.watch(&root, RecursiveMode::NonRecursive).is_err() {
        return None;
    }
    let shortcuts: PathBuf = root.join("shortcuts");
    if shortcuts.is_dir() {
        let _ = watcher.watch(&shortcuts, RecursiveMode::NonRecursive);
    }
    Some(watcher)
}
