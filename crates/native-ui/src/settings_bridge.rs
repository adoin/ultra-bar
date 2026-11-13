//! Declarative, event-notified settings shared with the host. Secret values never enter snapshots.
use crate::form::{Field, Kind};
use std::{
    io::{self, Read},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
const FILE: &str = "settings-schema.json";
pub const COPYDATA_SETTINGS: usize = 0x55425331;
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SaveRequest {
    pub revision: u64,
    pub values: Vec<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub request_id: u64,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActionResult {
    pub request_id: u64,
    pub ok: bool,
    pub message: String,
}
static WRITE: Mutex<()> = Mutex::new(());
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Schema {
    pub revision: u64,
    pub fields: Vec<Field>,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub ack: Option<u64>,
    #[serde(default)]
    pub action_result: Option<ActionResult>,
}
pub fn changed_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(crate::wide("UltraBar.SettingsSchemaChanged.v1").as_ptr())
    })
}
pub fn load(dir: &Path) -> io::Result<Schema> {
    let mut bytes = Vec::new();
    std::fs::File::open(dir.join(FILE))?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let schema: Schema = serde_json::from_slice(&bytes)?;
    if schema.fields.len() > 32
        || schema.fields.iter().any(|f| {
            f.label.chars().count() > 160
                || f.value.len() > 65536
                || matches!(&f.kind,Kind::Choice(c) if c.is_empty() || c.len()>32)
                || f.action.as_ref().is_some_and(|action| {
                    action.id.is_empty()
                        || action.id.len() > 64
                        || !action
                            .id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_')
                        || action.tooltip.chars().count() > 160
                        || action.icon.chars().count() > 4
                })
        })
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(schema)
}
pub fn publish(dir: &Path, owner: isize, fields: Vec<Field>, error: String) -> io::Result<()> {
    publish_ack(dir, owner, fields, error, None)
}
pub fn publish_ack(
    dir: &Path,
    owner: isize,
    mut fields: Vec<Field>,
    error: String,
    ack: Option<u64>,
) -> io::Result<()> {
    let _guard = WRITE.lock().map_err(|_| io::ErrorKind::Other)?;
    for field in &mut fields {
        if matches!(field.kind, Kind::Secret) {
            field.value.clear();
        }
    }
    let revision = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let revision = revision.max(load(dir).map_or(0, |s| s.revision.saturating_add(1)));
    let schema = Schema {
        revision,
        fields,
        error,
        ack,
        action_result: None,
    };
    let bytes = serde_json::to_vec(&schema)?;
    if bytes.len() > 65536 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    std::fs::create_dir_all(dir)?;
    let pending = dir.join("settings-schema.json.new");
    std::fs::write(&pending, bytes)?;
    std::fs::rename(pending, dir.join(FILE))?;
    crate::diagnostics::event(
        "settings",
        "schema.published",
        serde_json::json!({"data_dir":dir,"revision":revision,"ack":ack,"fields":schema.fields.len(),"error":schema.error}),
    );
    if owner != 0 {
        unsafe {
            PostMessageW(owner as HWND, changed_message(), 0, 0);
        }
    }
    Ok(())
}
pub fn error(dir: &Path, owner: isize, error: String) -> io::Result<()> {
    publish(dir, owner, load(dir)?.fields, error)
}
pub fn publish_action_result(
    dir: &Path,
    owner: isize,
    request_id: u64,
    ok: bool,
    message: String,
) -> io::Result<()> {
    let _guard = WRITE.lock().map_err(|_| io::ErrorKind::Other)?;
    let mut schema = load(dir)?;
    schema.action_result = Some(ActionResult {
        request_id,
        ok,
        message: message.chars().take(512).collect(),
    });
    crate::diagnostics::event(
        "settings",
        "action.completed",
        serde_json::json!({"request_id":request_id,"ok":ok}),
    );
    let pending = dir.join("settings-schema.json.new");
    std::fs::write(&pending, serde_json::to_vec(&schema)?)?;
    std::fs::rename(pending, dir.join(FILE))?;
    if owner != 0 {
        unsafe {
            PostMessageW(owner as HWND, changed_message(), 0, 0);
        }
    }
    Ok(())
}
pub fn error_ack(dir: &Path, owner: isize, error: String, ack: Option<u64>) -> io::Result<()> {
    publish_ack(dir, owner, load(dir)?.fields, error, ack)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_redact_secrets_and_acknowledge_exact_requests_without_losing_valid_data() {
        let dir = std::path::PathBuf::from(format!(
            "D:/tmp/ultra-bar-inline-settings-20261111/bridge-{}",
            std::process::id()
        ));
        let mut secret = Field::secret("API key");
        secret.value = "test-secret-not-for-disk".into();
        publish(
            &dir,
            0,
            vec![Field::text("过滤", "old"), secret],
            String::new(),
        )
        .unwrap();
        let first = load(&dir).unwrap();
        publish_action_result(&dir, 0, 77, true, "连接成功".into()).unwrap();
        let action = load(&dir).unwrap();
        assert_eq!(action.revision, first.revision);
        assert_eq!(action.action_result.unwrap().request_id, 77);
        assert!(action.fields[1].value.is_empty());
        assert!(
            !std::fs::read_to_string(dir.join(FILE))
                .unwrap()
                .contains("test-secret-not-for-disk")
        );
        assert!(first.fields[1].value.is_empty());
        publish_ack(
            &dir,
            0,
            vec![Field::text("过滤", "new")],
            String::new(),
            Some(first.revision),
        )
        .unwrap();
        let saved = load(&dir).unwrap();
        assert!(saved.revision > first.revision);
        assert_eq!(saved.ack, Some(first.revision));
        error_ack(&dir, 0, "规则无效".into(), Some(saved.revision)).unwrap();
        let failed = load(&dir).unwrap();
        assert_eq!(failed.fields[0].value, "new");
        assert_eq!(failed.error, "规则无效");
        assert_eq!(failed.ack, Some(saved.revision));
        assert!(
            publish(
                &dir,
                0,
                vec![Field::text("too long", "x".repeat(65536))],
                String::new()
            )
            .is_err()
        );
        assert_eq!(load(&dir).unwrap().revision, failed.revision);
    }
}
