//! Safe first-stage addon manager.
//!
//! This module intentionally supports declarative addon manifests only. It does
//! not execute uploaded server code or client JavaScript. That keeps the
//! persisted contract in place while the signed client-extension runtime is
//! designed separately.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = "manifest.json";
const STATE_FILE: &str = "state.json";
const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 80;
const MAX_DESCRIPTION_LEN: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    /// Only `declarative` is installable in this first stage.
    pub kind: String,
    #[serde(default)]
    pub min_server_version: Option<String>,
    #[serde(default)]
    pub min_client_version: Option<String>,
    #[serde(default)]
    pub widget_types: Vec<AddonWidgetType>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Reserved for the signed client-extension phase. Code-bearing fields
    /// are rejected until signature verification and sandboxing exist.
    #[serde(default)]
    pub client_entrypoint: Option<String>,
    #[serde(default)]
    pub server_entrypoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonWidgetType {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON-Schema-like object consumed by the editor in a later stage.
    #[serde(default)]
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonRecord {
    #[serde(flatten)]
    pub manifest: AddonManifest,
    pub enabled: bool,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AddonState {
    enabled: bool,
    installed_at: String,
}

#[derive(Debug)]
pub enum AddonError {
    Invalid(String),
    NotFound,
    AlreadyInstalled,
    Io(String),
}

impl std::fmt::Display for AddonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "{message}"),
            Self::NotFound => write!(f, "Addon not found"),
            Self::AlreadyInstalled => write!(f, "Addon is already installed"),
            Self::Io(message) => write!(f, "Addon storage error: {message}"),
        }
    }
}

fn storage_dir() -> PathBuf {
    let root = std::env::var("SIGNAGE_DATA_DIR").unwrap_or_else(|_| "/home/board/data".to_string());
    PathBuf::from(root).join("addons")
}

fn addon_dir(id: &str) -> Result<PathBuf, AddonError> {
    validate_id(id)?;
    Ok(storage_dir().join(id))
}

fn validate_id(id: &str) -> Result<(), AddonError> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(AddonError::Invalid(
            "Addon id must be 1-64 characters".to_string(),
        ));
    }
    if !id.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
    }) || !id.as_bytes()[0].is_ascii_lowercase() && !id.as_bytes()[0].is_ascii_digit()
    {
        return Err(AddonError::Invalid(
            "Addon id must start with a lowercase letter or digit and contain only lowercase letters, digits, '.', '_' or '-'".to_string(),
        ));
    }
    Ok(())
}

fn validate_token(value: &str, field: &str) -> Result<(), AddonError> {
    if value.is_empty()
        || value.len() > MAX_ID_LEN
        || !value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
        })
    {
        return Err(AddonError::Invalid(format!(
            "{field} must contain only lowercase letters, digits, '.', '_' or '-'"
        )));
    }
    Ok(())
}

pub fn validate_manifest(manifest: &AddonManifest) -> Result<(), AddonError> {
    if manifest.schema_version != 1 {
        return Err(AddonError::Invalid(
            "Unsupported addon manifest schema_version; expected 1".to_string(),
        ));
    }
    validate_id(&manifest.id)?;
    if manifest.name.trim().is_empty() || manifest.name.len() > MAX_NAME_LEN {
        return Err(AddonError::Invalid(
            "Addon name must be 1-80 characters".to_string(),
        ));
    }
    if manifest.description.len() > MAX_DESCRIPTION_LEN {
        return Err(AddonError::Invalid(
            "Addon description must be at most 500 characters".to_string(),
        ));
    }
    validate_token(&manifest.version, "Addon version")?;
    if manifest.kind != "declarative" {
        return Err(AddonError::Invalid(
            "Only declarative addons are supported in this release; executable client/server addons are not enabled yet".to_string(),
        ));
    }
    if manifest.client_entrypoint.is_some() || manifest.server_entrypoint.is_some() {
        return Err(AddonError::Invalid(
            "Code entrypoints are not accepted until signed addon execution is available"
                .to_string(),
        ));
    }
    if !manifest.permissions.is_empty() {
        return Err(AddonError::Invalid(
            "Declarative addons cannot request runtime permissions".to_string(),
        ));
    }
    for widget in &manifest.widget_types {
        validate_token(&widget.id, "Widget type id")?;
        if widget.name.trim().is_empty() || widget.name.len() > MAX_NAME_LEN {
            return Err(AddonError::Invalid(
                "Widget type name must be 1-80 characters".to_string(),
            ));
        }
        if !widget.schema.is_null() && !widget.schema.is_object() {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' schema must be a JSON object",
                widget.id
            )));
        }
    }
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> AddonError {
    AddonError::Io(error.to_string())
}

fn read_record(dir: &Path) -> Result<AddonRecord, AddonError> {
    let manifest_text = fs::read_to_string(dir.join(MANIFEST_FILE)).map_err(io_error)?;
    let manifest: AddonManifest = serde_json::from_str(&manifest_text).map_err(io_error)?;
    validate_manifest(&manifest)?;
    let state_text = fs::read_to_string(dir.join(STATE_FILE)).map_err(io_error)?;
    let state: AddonState = serde_json::from_str(&state_text).map_err(io_error)?;
    Ok(AddonRecord {
        manifest,
        enabled: state.enabled,
        installed_at: state.installed_at,
    })
}

pub fn list() -> Result<Vec<AddonRecord>, AddonError> {
    let root = storage_dir();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry.file_type().map_err(io_error)?.is_dir() {
            records.push(read_record(&entry.path())?);
        }
    }
    records.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(records)
}

pub fn install(manifest: AddonManifest) -> Result<AddonRecord, AddonError> {
    validate_manifest(&manifest)?;
    let dir = addon_dir(&manifest.id)?;
    if dir.exists() {
        return Err(AddonError::AlreadyInstalled);
    }
    fs::create_dir_all(&dir).map_err(io_error)?;
    let installed_at = chrono::Utc::now().to_rfc3339();
    let state = AddonState {
        enabled: true,
        installed_at: installed_at.clone(),
    };
    let manifest_text = serde_json::to_string_pretty(&manifest).map_err(io_error)?;
    let state_text = serde_json::to_string_pretty(&state).map_err(io_error)?;
    if let Err(error) = fs::write(dir.join(MANIFEST_FILE), manifest_text)
        .and_then(|_| fs::write(dir.join(STATE_FILE), state_text))
    {
        let _ = fs::remove_dir_all(&dir);
        return Err(io_error(error));
    }
    Ok(AddonRecord {
        manifest,
        enabled: state.enabled,
        installed_at,
    })
}

pub fn set_enabled(id: &str, enabled: bool) -> Result<AddonRecord, AddonError> {
    let dir = addon_dir(id)?;
    if !dir.exists() {
        return Err(AddonError::NotFound);
    }
    let mut record = read_record(&dir)?;
    record.enabled = enabled;
    let state = AddonState {
        enabled,
        installed_at: record.installed_at.clone(),
    };
    let state_text = serde_json::to_string_pretty(&state).map_err(io_error)?;
    fs::write(dir.join(STATE_FILE), state_text).map_err(io_error)?;
    Ok(record)
}

pub fn remove(id: &str) -> Result<(), AddonError> {
    let dir = addon_dir(id)?;
    if !dir.exists() {
        return Err(AddonError::NotFound);
    }
    fs::remove_dir_all(dir).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Mutex, OnceLock};

    static STORAGE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn manifest() -> AddonManifest {
        AddonManifest {
            schema_version: 1,
            id: "example.weather".to_string(),
            name: "Example Weather".to_string(),
            description: "A declarative widget definition".to_string(),
            version: "0.1.0".to_string(),
            kind: "declarative".to_string(),
            min_server_version: Some("0.1.4".to_string()),
            min_client_version: None,
            widget_types: vec![AddonWidgetType {
                id: "example.weather".to_string(),
                name: "Example Weather".to_string(),
                description: String::new(),
                schema: serde_json::json!({"type": "object"}),
            }],
            permissions: Vec::new(),
            client_entrypoint: None,
            server_entrypoint: None,
        }
    }

    #[test]
    fn accepts_declarative_manifest() {
        assert!(validate_manifest(&manifest()).is_ok());
    }

    #[test]
    fn rejects_executable_manifest() {
        let mut value = manifest();
        value.kind = "client_widget".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_path_traversal_id() {
        let mut value = manifest();
        value.id = "../escape".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn storage_round_trip_preserves_enable_state() {
        let _guard = STORAGE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        let installed = install(manifest()).unwrap();
        assert!(installed.enabled);
        assert_eq!(list().unwrap().len(), 1);
        let disabled = set_enabled("example.weather", false).unwrap();
        assert!(!disabled.enabled);
        assert!(!list().unwrap()[0].enabled);
        remove("example.weather").unwrap();
        assert!(list().unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }
}
