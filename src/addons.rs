//! Safe first-stage addon manager.
//!
//! This module intentionally supports declarative addon manifests only. It does
//! not execute uploaded server code or client JavaScript. That keeps the
//! persisted contract in place while the signed client-extension runtime is
//! designed separately.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const MANIFEST_FILE: &str = "manifest.json";
const STATE_FILE: &str = "state.json";
const ROLLBACK_DIR: &str = ".rollback";
const SUPPORTED_CLIENT_API_VERSION: &str = "1";
const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 80;
const MAX_DESCRIPTION_LEN: usize = 500;
const HOST_RENDERERS: &[&str] = &[
    "text",
    "image",
    "video",
    "clock",
    "date",
    "weather",
    "traffic",
    "countdown",
    "sound",
    "embed",
    "calendar",
    "camera",
    "news",
    "rectangle",
];

pub const MAX_ADDON_STORAGE_KEY_BYTES: usize = 128;
pub const MAX_ADDON_STORAGE_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_CLIENT_STATUS_BYTES: usize = 256 * 1024;
const MAX_CLIENT_STATUS_DETAIL_BYTES: usize = 160;
const MAX_CLIENT_STATUS_ERROR_BYTES: usize = 96;

/// Validate the addon namespace before it reaches either database backend.
/// This is deliberately the same conservative identifier shape used by addon
/// manifests, not a filesystem path or SQL identifier.
pub fn validate_addon_storage_namespace(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_ID_LEN {
        return Err("Invalid addon storage namespace".to_string());
    }
    let mut chars = value.chars();
    if !chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric())
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err("Invalid addon storage namespace".to_string());
    }
    Ok(())
}

pub fn validate_addon_storage_key(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_ADDON_STORAGE_KEY_BYTES
        || value.chars().any(|ch| ch.is_control() || ch == '/')
    {
        return Err("Invalid addon storage key".to_string());
    }
    Ok(())
}

pub fn validate_addon_storage_value(value: &str) -> Result<(), String> {
    if value.len() > MAX_ADDON_STORAGE_VALUE_BYTES {
        return Err("Addon storage value exceeds the 64 KiB limit".to_string());
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|_| "Addon storage value must be valid JSON".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedServiceLaunchProfile {
    pub command: Vec<String>,
    pub gateway_socket_path: String,
    pub rootless: bool,
    pub non_root: bool,
    pub network_disabled: bool,
    pub read_only_rootfs: bool,
    pub no_new_privileges: bool,
    pub drop_all_capabilities: bool,
    pub seccomp_default: bool,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedServerServiceSpec {
    pub image: String,
    pub image_digest: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub storage_namespace: String,
    pub api_namespace: String,
    pub launch_profile: TrustedServiceLaunchProfile,
    #[serde(default)]
    pub client_extension_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonDataContract {
    pub api_version: String,
    #[serde(default)]
    pub region_scope: Vec<String>,
    #[serde(default)]
    pub endpoints: Vec<AddonDataEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonDataEndpoint {
    pub path: String,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientServiceSpec {
    pub id: String,
    pub image: String,
    pub image_digest: String,
    pub socket_name: String,
    pub cpu_millis: u32,
    pub memory_bytes: u64,
    pub pids: u32,
    pub writable_bytes: u64,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_sha256: Option<String>,
}

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
    /// Explicit client-side capability requests. These are metadata only until
    /// the signed client runtime and capability broker are enabled.
    #[serde(default)]
    pub client_capabilities: Vec<String>,
    /// Optional host API contract used by declarative widget packages. This
    /// describes existing, read-only server endpoints; it does not grant the
    /// addon arbitrary network, database, or server-code execution access.
    #[serde(default)]
    pub data_contract: Option<AddonDataContract>,
    /// Reserved for the signed client-extension phase. Code-bearing fields
    /// are rejected until signature verification and sandboxing exist.
    #[serde(default)]
    pub client_entrypoint: Option<String>,
    #[serde(default)]
    pub server_entrypoint: Option<String>,
    #[serde(default)]
    pub trusted_service: Option<TrustedServerServiceSpec>,
    /// Signed client-service metadata. This is inert until the client runtime
    /// and capability broker are separately enabled.
    #[serde(default)]
    pub client_service: Option<ClientServiceSpec>,
    #[serde(default)]
    pub client_sha256: Option<String>,
    #[serde(default)]
    pub client_api_version: Option<String>,
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
    /// Existing trusted renderer used by this declarative alias. This keeps
    /// the first runtime phase code-free while still making an addon visible
    /// in the editor and usable by the player.
    #[serde(default, alias = "renderer_type")]
    pub host_renderer: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub default_props: serde_json::Value,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAddonStatus {
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    pub state: String,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientServiceStatus {
    pub id: String,
    pub version: String,
    pub state: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_sha256: Option<String>,
}

const CLIENT_SERVICE_STATES: &[&str] = &[
    "staged",
    "starting",
    "running",
    "degraded",
    "stopped",
    "failed",
    "rollback_ready",
];

const CLIENT_CAPABILITIES: &[&str] = &[
    "render_only",
    "camera_capture",
    "microphone_capture",
    "local_inference",
    "client_service",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCapabilityStatus {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAddonStatusReport {
    pub at: Option<String>,
    pub addons: Vec<ClientAddonStatus>,
    /// Hardware/runtime observations are report-only until a capability broker
    /// and signed client service lifecycle are enabled.
    pub capabilities: Vec<ClientCapabilityStatus>,
    /// Signed client-service observations are report-only until the trusted
    /// client supervisor and capability broker are enabled.
    pub services: Vec<ClientServiceStatus>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredClientStatusPayload {
    client_id: String,
    received_at: String,
    report: ClientAddonStatusReport,
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

fn data_dir() -> PathBuf {
    std::env::var("SIGNAGE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/board/data"))
}

fn reject_symlinked_path_ancestors(
    path: &Path,
    description: &str,
) -> Result<(), AddonError> {
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AddonError::Invalid(description.to_string()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    Ok(())
}

fn checked_data_dir(create: bool) -> Result<Option<PathBuf>, AddonError> {
    let root = data_dir();
    reject_symlinked_path_ancestors(&root, "Invalid addon data directory")?;
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid addon data directory".to_string()),
        ),
        Ok(_) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(&root).map_err(io_error)?;
            let metadata = fs::symlink_metadata(&root).map_err(io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid addon data directory".to_string(),
                ));
            }
            Ok(Some(root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

pub(crate) fn checked_addon_data_root(
    create: bool,
) -> Result<Option<PathBuf>, AddonError> {
    checked_data_dir(create)
}

fn storage_dir() -> PathBuf {
    data_dir().join("addons")
}

fn checked_addon_storage_dir(create: bool) -> Result<Option<PathBuf>, AddonError> {
    let Some(data_root) = checked_data_dir(create)? else {
        return Ok(None);
    };
    let root = data_root.join("addons");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid addon storage directory".to_string()),
        ),
        Ok(_) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(&root).map_err(io_error)?;
            let metadata = fs::symlink_metadata(&root).map_err(io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid addon storage directory".to_string(),
                ));
            }
            Ok(Some(root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn client_status_path(client_id: &str) -> Result<PathBuf, AddonError> {
    if client_id.is_empty() || client_id.len() > 200 || client_id.chars().any(|ch| ch.is_control())
    {
        return Err(AddonError::Invalid("Invalid client id".to_string()));
    }
    let digest = Sha256::digest(client_id.as_bytes());
    Ok(storage_dir()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("addon-client-status")
        .join(format!("{:x}.json", digest)))
}

fn checked_client_status_dir(create: bool) -> Result<Option<PathBuf>, AddonError> {
    let Some(data_root) = checked_data_dir(create)? else {
        return Ok(None);
    };
    let dir = data_root.join("addon-client-status");
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid addon client status directory".to_string()),
        ),
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    return Err(AddonError::Invalid(
                        "Invalid addon client status directory permissions".to_string(),
                    ));
                }
            }
            Ok(Some(dir))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(&dir).map_err(io_error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::symlink_metadata(&dir)
                    .map_err(io_error)?
                    .permissions();
                permissions.set_mode(0o700);
                fs::set_permissions(&dir, permissions).map_err(io_error)?;
            }
            let metadata = fs::symlink_metadata(&dir).map_err(io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid addon client status directory".to_string(),
                ));
            }
            Ok(Some(dir))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_client_status_text(
    value: Option<&str>,
    field: &str,
    maximum_bytes: usize,
    reject_sensitive_markers: bool,
) -> Result<(), AddonError> {
    let Some(value) = value else {
        return Ok(());
    };
    let lowered = value.to_ascii_lowercase();
    let sensitive = ["password", "credential", "secret", "token", "://", "/"];
    if value.is_empty()
        || value.len() > maximum_bytes
        || value
            .chars()
            .any(|character| character.is_control())
        || (reject_sensitive_markers && sensitive.iter().any(|marker| lowered.contains(marker)))
    {
        return Err(AddonError::Invalid(format!("Invalid {field}")));
    }
    Ok(())
}

fn validate_client_status_timestamp(
    value: Option<&str>,
    field: &str,
) -> Result<(), AddonError> {
    validate_client_status_text(value, field, 64, false)?;
    if let Some(value) = value {
        chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ")
            .map_err(|_| AddonError::Invalid(format!("Invalid {field}")))?;
    }
    Ok(())
}

fn validate_client_status_report(report: &ClientAddonStatusReport) -> Result<(), AddonError> {
    if report.at.is_none() {
        return Err(AddonError::Invalid(
            "Addon status timestamp is required".to_string(),
        ));
    }
    validate_client_status_timestamp(report.at.as_deref(), "addon status timestamp")?;
    if report.addons.len() > 64 {
        return Err(AddonError::Invalid(
            "Addon status contains too many entries".to_string(),
        ));
    }
    if report.capabilities.len() > CLIENT_CAPABILITIES.len() {
        return Err(AddonError::Invalid(
            "Client status contains too many capabilities".to_string(),
        ));
    }
    let mut seen_capabilities = BTreeSet::new();
    for capability in &report.capabilities {
        if !CLIENT_CAPABILITIES.contains(&capability.id.as_str()) {
            return Err(AddonError::Invalid(
                "Unsupported client capability status".to_string(),
            ));
        }
        if !seen_capabilities.insert(&capability.id) {
            return Err(AddonError::Invalid(
                "Duplicate client capability status".to_string(),
            ));
        }
        if !matches!(
            capability.state.as_str(),
            "available" | "unavailable" | "disabled"
        ) {
            return Err(AddonError::Invalid(
                "Unsupported client capability state".to_string(),
            ));
        }
        validate_client_status_text(
            capability.detail.as_deref(),
            "client capability detail",
            MAX_CLIENT_STATUS_DETAIL_BYTES,
            true,
        )?;
    }
    if report.services.len() > 16 {
        return Err(AddonError::Invalid(
            "Client status contains too many services".to_string(),
        ));
    }
    let mut seen_services = BTreeSet::new();
    for service in &report.services {
        validate_token(&service.id, "Client service status id")?;
        validate_token(&service.version, "Client service status version")?;
        if !CLIENT_SERVICE_STATES.contains(&service.state.as_str()) {
            return Err(AddonError::Invalid(
                "Unsupported client service status".to_string(),
            ));
        }
        if !seen_services.insert(&service.id) {
            return Err(AddonError::Invalid(
                "Duplicate client service status".to_string(),
            ));
        }
        if service.restart_count > 32 {
            return Err(AddonError::Invalid(
                "Client service restart count exceeds the bounded limit".to_string(),
            ));
        }
        validate_client_status_text(
            service.detail.as_deref(),
            "client service detail",
            MAX_CLIENT_STATUS_DETAIL_BYTES,
            true,
        )?;
        validate_client_service_model_identity(
            service.model_id.as_deref(),
            service.model_sha256.as_deref(),
        )?;
    }
    let mut seen_addons = BTreeSet::new();
    for addon in &report.addons {
        validate_id(&addon.id)?;
        if !seen_addons.insert(&addon.id) {
            return Err(AddonError::Invalid(
                "Duplicate addon client status".to_string(),
            ));
        }
        if let Some(version) = addon.version.as_deref() {
            validate_token(version, "Addon status version")?;
        }
        if !matches!(
            addon.state.as_str(),
            "verified" | "verified_cached" | "failed"
        ) {
            return Err(AddonError::Invalid(
                "Unsupported addon client status".to_string(),
            ));
        }
        if let Some(entrypoint) = addon.entrypoint.as_deref() {
            validate_client_status_entrypoint(entrypoint)?;
        }
        validate_client_status_text(
            addon.error.as_deref(),
            "addon status error",
            MAX_CLIENT_STATUS_ERROR_BYTES,
            true,
        )?;
    }
    validate_client_status_text(
        report.error.as_deref(),
        "addon status error",
        MAX_CLIENT_STATUS_ERROR_BYTES,
        true,
    )
}

pub fn save_client_status(
    client_id: &str,
    report: ClientAddonStatusReport,
) -> Result<(), AddonError> {
    let path = client_status_path(client_id)?;
    validate_client_status_report(&report)?;
    let payload = serde_json::json!({
        "client_id": client_id,
        "received_at": chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        "report": report,
    });
    let bytes = serde_json::to_vec_pretty(&payload).map_err(io_error)?;
    if bytes.len() > MAX_CLIENT_STATUS_BYTES {
        return Err(AddonError::Invalid(
            "Addon status payload is too large".to_string(),
        ));
    }
    let _ = checked_client_status_dir(true)?;
    let parent = path
        .parent()
        .ok_or_else(|| AddonError::Io("Invalid addon status path".to_string()))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    write_addon_file_atomic(&path, &bytes, "status")
}

pub fn load_client_status(client_id: &str) -> Result<Option<serde_json::Value>, AddonError> {
    let Some(_status_dir) = checked_client_status_dir(false)? else {
        return Ok(None);
    };
    let path = client_status_path(client_id)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AddonError::Invalid(
            "Invalid stored addon status".to_string(),
        ));
    }
    let bytes = fs::read(path).map_err(io_error)?;
    if bytes.len() > MAX_CLIENT_STATUS_BYTES {
        return Err(AddonError::Invalid(
            "Stored addon status is too large".to_string(),
        ));
    }
    let payload: StoredClientStatusPayload = serde_json::from_slice(&bytes)
        .map_err(|error| AddonError::Io(format!("Invalid stored addon status: {error}")))?;
    if payload.client_id != client_id {
        return Err(AddonError::Invalid(
            "Stored addon status client identity mismatch".to_string(),
        ));
    }
    validate_client_status_timestamp(
        Some(&payload.received_at),
        "stored addon status timestamp",
    )?;
    validate_client_status_report(&payload.report)?;
    serde_json::to_value(payload)
        .map(Some)
        .map_err(|error| AddonError::Io(format!("Invalid stored addon status: {error}")))
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

fn validate_client_service_model_identity(
    model_id: Option<&str>,
    model_sha256: Option<&str>,
) -> Result<(), AddonError> {
    if model_id.is_some() != model_sha256.is_some() {
        return Err(AddonError::Invalid(
            "Client service model_id and model_sha256 must be declared together".to_string(),
        ));
    }
    if let Some(model_id) = model_id {
        validate_token(model_id, "Client service model id")?;
    }
    if let Some(model_sha256) = model_sha256 {
        if model_sha256.len() != 64
            || !model_sha256.bytes().all(|byte| {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            })
        {
            return Err(AddonError::Invalid(
                "Client service model_sha256 must be a 64-character lowercase hexadecimal digest"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_client_service_spec(
    service: &ClientServiceSpec,
    requires_model: bool,
) -> Result<(), AddonError> {
    validate_token(&service.id, "Client service id")?;
    validate_trusted_service_image_reference(&service.image).map_err(AddonError::Invalid)?;
    validate_trusted_service_image_digest(&service.image_digest).map_err(AddonError::Invalid)?;
    validate_token(&service.socket_name, "Client service socket name")?;
    if !(1..=4_000).contains(&service.cpu_millis) {
        return Err(AddonError::Invalid(
            "Client service cpu_millis must be between 1 and 4000".to_string(),
        ));
    }
    if !(64 * 1024 * 1024..=2 * 1024 * 1024 * 1024).contains(&service.memory_bytes) {
        return Err(AddonError::Invalid(
            "Client service memory_bytes is outside the bounded range".to_string(),
        ));
    }
    if !(1..=256).contains(&service.pids) {
        return Err(AddonError::Invalid(
            "Client service pids must be between 1 and 256".to_string(),
        ));
    }
    if service.writable_bytes > 512 * 1024 * 1024 {
        return Err(AddonError::Invalid(
            "Client service writable_bytes exceeds the bounded limit".to_string(),
        ));
    }
    if requires_model && service.model_id.is_none() {
        return Err(AddonError::Invalid(
            "local_inference requires an explicit model identity and digest".to_string(),
        ));
    }
    validate_client_service_model_identity(
        service.model_id.as_deref(),
        service.model_sha256.as_deref(),
    )?;
    Ok(())
}

const TRUSTED_SERVICE_CAPABILITIES: &[&str] = &[
    "storage",
    "read:clients",
    "read:layouts",
    "client_extension",
];

pub(crate) fn validate_trusted_service_image_reference(image: &str) -> Result<(), String> {
    if image.is_empty()
        || image.len() > 255
        || image.contains("..")
        || image.contains("://")
        || image.contains(':')
        || image.contains('@')
        || image.contains("//")
        || !image.contains('/')
        || image.starts_with('/')
        || image.ends_with('/')
        || image
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || ch.is_ascii_uppercase())
        || !image
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | ':'))
        || image.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component
                    .bytes()
                    .next()
                    .map_or(true, |byte| !byte.is_ascii_alphanumeric())
                || component
                    .bytes()
                    .last()
                    .map_or(true, |byte| !byte.is_ascii_alphanumeric())
                || component.starts_with(':')
                || component.ends_with(':')
        })
    {
        return Err("Invalid trusted service image reference".to_string());
    }
    Ok(())
}

pub(crate) fn validate_trusted_service_image_digest(digest: &str) -> Result<(), String> {
    if digest.len() != "sha256:".len() + 64
        || !digest.starts_with("sha256:")
        || !digest["sha256:".len()..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err("Invalid trusted service image digest".to_string());
    }
    Ok(())
}

pub(crate) fn validate_trusted_service_launch_profile(
    profile: &TrustedServiceLaunchProfile,
) -> Result<(), AddonError> {
    if profile.gateway_socket_path != crate::trusted_service_gateway::GATEWAY_SOCKET_PATH
        || !profile.rootless
        || !profile.non_root
        || !profile.network_disabled
        || !profile.read_only_rootfs
        || !profile.no_new_privileges
        || !profile.drop_all_capabilities
        || !profile.seccomp_default
    {
        return Err(AddonError::Invalid(
            "Invalid trusted service isolation profile".to_string(),
        ));
    }
    let command_bytes = profile
        .command
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(part.len()));
    if profile.command.is_empty()
        || profile.command.len() > 64
        || command_bytes.map_or(true, |bytes| bytes > 4 * 1024)
        || profile.command.iter().any(|part| {
            let executable = Path::new(part)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(part);
            part.is_empty()
                || part.len() > 256
                || part.chars().any(|character| character.is_control())
                || matches!(
                    executable,
                    "sh" | "bash"
                        | "dash"
                        | "zsh"
                        | "ksh"
                        | "fish"
                        | "csh"
                        | "tcsh"
                        | "pwsh"
                        | "powershell"
                        | "cmd"
                        | "cmd.exe"
                )
                || matches!(part.as_str(), "-c" | "-lc")
        })
        || profile.memory_bytes == 0
        || profile.memory_bytes > 4 * 1024 * 1024 * 1024
        || profile.cpu_millis == 0
        || profile.cpu_millis > 16_000
        || profile.pids == 0
        || profile.pids > 4_096
        || profile.writable_bytes == 0
        || profile.writable_bytes > 1024 * 1024 * 1024
    {
        return Err(AddonError::Invalid(
            "Invalid trusted service launch profile".to_string(),
        ));
    }
    Ok(())
}

fn validate_trusted_service_spec(spec: &TrustedServerServiceSpec) -> Result<(), AddonError> {
    validate_trusted_service_image_reference(&spec.image).map_err(AddonError::Invalid)?;
    validate_trusted_service_image_digest(&spec.image_digest).map_err(AddonError::Invalid)?;
    validate_trusted_service_launch_profile(&spec.launch_profile)?;
    validate_id(&spec.storage_namespace).map_err(|_| {
        AddonError::Invalid("Invalid trusted service storage namespace".to_string())
    })?;
    validate_id(&spec.api_namespace)
        .map_err(|_| AddonError::Invalid("Invalid trusted service API namespace".to_string()))?;
    if let Some(client_extension_id) = spec.client_extension_id.as_deref() {
        validate_id(client_extension_id).map_err(|_| {
            AddonError::Invalid("Invalid trusted service client extension ID".to_string())
        })?;
    }
    if spec.capabilities.len() > TRUSTED_SERVICE_CAPABILITIES.len() {
        return Err(AddonError::Invalid(
            "Too many trusted service capabilities".to_string(),
        ));
    }
    let mut seen_capabilities = BTreeSet::new();
    for capability in &spec.capabilities {
        if !TRUSTED_SERVICE_CAPABILITIES.contains(&capability.as_str())
            || !seen_capabilities.insert(capability)
        {
            return Err(AddonError::Invalid(format!(
                "Unknown trusted service capability: {capability}"
            )));
        }
    }
    Ok(())
}

fn validate_data_contract(contract: &AddonDataContract) -> Result<(), AddonError> {
    if contract.api_version != "1" {
        return Err(AddonError::Invalid(
            "Unsupported addon data contract version".to_string(),
        ));
    }
    if contract.region_scope.len() > 16 || contract.endpoints.is_empty() || contract.endpoints.len() > 8 {
        return Err(AddonError::Invalid(
            "Addon data contract has invalid bounds".to_string(),
        ));
    }
    for region in &contract.region_scope {
        if region.is_empty()
            || region.len() > 32
            || !region.bytes().all(|byte| byte.is_ascii_uppercase() || byte == b'-')
        {
            return Err(AddonError::Invalid(
                "Addon data contract contains an invalid region".to_string(),
            ));
        }
    }
    const READ_ONLY_ENDPOINTS: &[&str] = &[
        "/api/weather/forecast",
        "/api/weather/lookup",
        "/api/traffic/events",
        "/api/traffic/map.png",
    ];
    for endpoint in &contract.endpoints {
        if endpoint.method != "GET" || !READ_ONLY_ENDPOINTS.contains(&endpoint.path.as_str()) {
            return Err(AddonError::Invalid(
                "Addon data contract contains an unsupported or mutating endpoint".to_string(),
            ));
        }
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
    if manifest.kind == "trusted_server_service" {
        let trusted_service = manifest.trusted_service.as_ref().ok_or_else(|| {
            AddonError::Invalid(
                "trusted_server_service requires trusted_service metadata".to_string(),
            )
        })?;
        validate_trusted_service_spec(trusted_service)?;
        return Err(AddonError::Invalid(
            "Trusted server addon services are contract-only; container execution is not enabled yet"
                .to_string(),
        ));
    }
    if manifest.kind != "declarative" {
        return Err(AddonError::Invalid(
            "Only declarative addons are supported in this release; executable client/server addons are not enabled yet".to_string(),
        ));
    }
    if manifest.trusted_service.is_some() {
        return Err(AddonError::Invalid(
            "trusted_service metadata requires kind=trusted_server_service".to_string(),
        ));
    }
    if let Some(contract) = &manifest.data_contract {
        validate_data_contract(contract)?;
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
    if !manifest.client_capabilities.is_empty() {
        return Err(AddonError::Invalid(
            "Declarative addons cannot request client capabilities".to_string(),
        ));
    }
    if manifest.client_service.is_some() {
        return Err(AddonError::Invalid(
            "Declarative addons cannot declare a client service".to_string(),
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
        if !HOST_RENDERERS.contains(&widget.host_renderer.as_str()) {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' must select a supported host_renderer",
                widget.id
            )));
        }
        if !widget.default_props.is_null() && !widget.default_props.is_object() {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' default_props must be a JSON object",
                widget.id
            )));
        }
    }
    Ok(())
}

/// Flatten enabled declarative addons into the small registration contract
/// consumed by the editor. The player continues using the selected trusted
/// base renderer; no addon code is loaded at runtime in this phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddonWidgetRegistration {
    pub id: String,
    pub addon_id: String,
    pub addon_version: String,
    pub name: String,
    pub description: String,
    pub host_renderer: String,
    pub icon: String,
    pub schema: serde_json::Value,
    pub default_props: serde_json::Value,
}

pub fn client_package(id: &str) -> Result<Vec<u8>, AddonError> {
    let _ = checked_addon_storage_dir(false)?;
    let dir = addon_dir(id)?;
    let metadata = fs::symlink_metadata(&dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AddonError::NotFound
        } else {
            io_error(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::NotFound);
    }
    let record = read_record(&dir)?;
    if !record.enabled || record.manifest.kind != "client_extension" {
        return Err(AddonError::NotFound);
    }
    let entrypoint = record
        .manifest
        .client_entrypoint
        .as_deref()
        .ok_or_else(|| AddonError::Invalid("Client package entrypoint is missing".to_string()))?;
    let manifest_path = dir.join(MANIFEST_FILE);
    let signature_path = dir.join("manifest.sig");
    let client_path = dir.join(entrypoint);
    checked_addon_file(&manifest_path, "stored addon manifest")?;
    checked_addon_file(&signature_path, "stored addon signature")?;
    checked_addon_file(&client_path, "stored client package")?;
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder
        .append_path_with_name(&manifest_path, MANIFEST_FILE)
        .map_err(io_error)?;
    builder
        .append_path_with_name(&signature_path, "manifest.sig")
        .map_err(io_error)?;
    builder
        .append_path_with_name(&client_path, entrypoint)
        .map_err(io_error)?;
    let encoder = builder.into_inner().map_err(io_error)?;
    encoder.finish().map_err(io_error)
}

pub fn enabled_client_extensions() -> Result<Vec<AddonManifest>, AddonError> {
    Ok(list()?
        .into_iter()
        .filter(|addon| addon.enabled && addon.manifest.kind == "client_extension")
        .map(|addon| addon.manifest)
        .collect())
}

pub fn ensure_bundled_standard_addons() -> Result<Vec<AddonRecord>, AddonError> {
    let bundled = [
        include_str!("../addon-packages/weather-widget-standard-us-ca/manifest.json"),
        include_str!("../addon-packages/traffic-standard-us-ca/manifest.json"),
        include_str!("../addon-packages/magic-mirror-camera/manifest.json"),
    ];
    let root = checked_addon_storage_dir(true)?.ok_or_else(|| {
        AddonError::Io("Addon storage directory is unavailable".to_string())
    })?;
    let marker = root.join(".standard-addons-seeded");
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AddonError::Invalid(
                "Invalid standard addon seed marker".to_string(),
            ));
        }
        Ok(_) => return migrate_legacy_standard_renderer_fields(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }

    let existing = list()?;
    for bytes in bundled {
        let manifest: AddonManifest = serde_json::from_str(bytes).map_err(io_error)?;
        validate_manifest(&manifest)?;
        if !existing
            .iter()
            .any(|addon| addon.manifest.id == manifest.id)
        {
            install(manifest)?;
        }
    }
    write_addon_file_atomic(&marker, b"seeded\n", "standard-addon-seed")?;
    migrate_legacy_standard_renderer_fields()
}

/// Rewrite the two bundled declarative manifests through the canonical
/// serializer when an older install still stores `renderer_type`. The
/// manifest update path is atomic and preserves the existing lifecycle state.
fn migrate_legacy_standard_renderer_fields() -> Result<Vec<AddonRecord>, AddonError> {
    const STANDARD_IDS: &[&str] = &[
        "weather-widget-standard-us-ca",
        "traffic-standard-us-ca",
        "magic-mirror-camera",
    ];

    for id in STANDARD_IDS {
        let dir = addon_dir(id)?;
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AddonError::Invalid(
                "Invalid standard addon directory".to_string(),
            ));
        }

        let manifest_path = dir.join(MANIFEST_FILE);
        checked_addon_file(&manifest_path, "standard addon manifest")?;
        let manifest_text = fs::read_to_string(&manifest_path).map_err(io_error)?;
        let raw: serde_json::Value = serde_json::from_str(&manifest_text).map_err(io_error)?;
        let has_legacy_renderer = raw
            .get("widget_types")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|widgets| {
                widgets.iter().any(|widget| {
                    widget
                        .as_object()
                        .is_some_and(|object| object.contains_key("renderer_type"))
                })
            });
        if !has_legacy_renderer {
            continue;
        }

        let manifest: AddonManifest = serde_json::from_value(raw).map_err(io_error)?;
        update(manifest)?;
    }

    list()
}

pub fn enabled_widget_registrations() -> Result<Vec<AddonWidgetRegistration>, AddonError> {
    let mut registrations = Vec::new();
    for addon in list()?.into_iter().filter(|addon| addon.enabled) {
        for widget in addon.manifest.widget_types {
            if widget.host_renderer.is_empty() {
                continue;
            }
            registrations.push(AddonWidgetRegistration {
                id: widget.id,
                addon_id: addon.manifest.id.clone(),
                addon_version: addon.manifest.version.clone(),
                name: widget.name,
                description: widget.description,
                host_renderer: widget.host_renderer,
                icon: widget.icon,
                schema: widget.schema,
                default_props: widget.default_props,
            });
        }
    }
    registrations.sort_by(|a, b| a.id.cmp(&b.id).then(a.addon_id.cmp(&b.addon_id)));
    Ok(registrations)
}

pub fn validate_client_extension_manifest(manifest: &AddonManifest) -> Result<(), AddonError> {
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
    if manifest.kind != "client_extension" {
        return Err(AddonError::Invalid(
            "Signed client packages must use kind=client_extension".to_string(),
        ));
    }
    if manifest.server_entrypoint.is_some() {
        return Err(AddonError::Invalid(
            "Client packages cannot contain a server entrypoint".to_string(),
        ));
    }
    let entrypoint = manifest.client_entrypoint.as_deref().ok_or_else(|| {
        AddonError::Invalid("Client packages must declare client_entrypoint".to_string())
    })?;
    validate_relative_client_path(entrypoint)?;
    let digest = manifest.client_sha256.as_deref().ok_or_else(|| {
        AddonError::Invalid("Client packages must declare client_sha256".to_string())
    })?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AddonError::Invalid(
            "client_sha256 must be a 64-character hexadecimal digest".to_string(),
        ));
    }
    let api_version = manifest.client_api_version.as_deref().ok_or_else(|| {
        AddonError::Invalid("Client packages must declare client_api_version".to_string())
    })?;
    validate_token(api_version, "client_api_version")?;
    if api_version != SUPPORTED_CLIENT_API_VERSION {
        return Err(AddonError::Invalid(format!(
            "Unsupported client_api_version '{}'; expected {}",
            api_version, SUPPORTED_CLIENT_API_VERSION
        )));
    }
    for permission in &manifest.permissions {
        if permission != "render_only" && permission != "network_fetch" {
            return Err(AddonError::Invalid(format!(
                "Unsupported client addon permission '{permission}'"
            )));
        }
    }
    let mut seen_capabilities = BTreeSet::new();
    for capability in &manifest.client_capabilities {
        if !CLIENT_CAPABILITIES.contains(&capability.as_str()) {
            return Err(AddonError::Invalid(format!(
                "Unsupported client capability '{capability}'"
            )));
        }
        if !seen_capabilities.insert(capability) {
            return Err(AddonError::Invalid(format!(
                "Duplicate client capability '{capability}'"
            )));
        }
    }
    if manifest.client_capabilities.iter().any(|capability| capability == "local_inference")
        && !manifest.client_capabilities.iter().any(|capability| capability == "client_service")
    {
        return Err(AddonError::Invalid(
            "local_inference requires client_service".to_string(),
        ));
    }
    if manifest
        .client_capabilities
        .iter()
        .any(|capability| capability == "local_inference")
        && !manifest
            .client_capabilities
            .iter()
            .any(|capability| capability == "microphone_capture")
    {
        return Err(AddonError::Invalid(
            "local_inference requires microphone_capture".to_string(),
        ));
    }
    let requests_client_service = manifest
        .client_capabilities
        .iter()
        .any(|capability| capability == "client_service");
    if requests_client_service != manifest.client_service.is_some() {
        return Err(AddonError::Invalid(
            "client_service capability and client_service metadata must be declared together"
                .to_string(),
        ));
    }
    if let Some(service) = manifest.client_service.as_ref() {
        validate_client_service_spec(
            service,
            manifest
                .client_capabilities
                .iter()
                .any(|capability| capability == "local_inference"),
        )?;
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
        if !widget.host_renderer.is_empty()
            && !HOST_RENDERERS.contains(&widget.host_renderer.as_str())
        {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' selected an unsupported fallback renderer",
                widget.id
            )));
        }
        if !widget.default_props.is_null() && !widget.default_props.is_object() {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' default_props must be a JSON object",
                widget.id
            )));
        }
    }
    Ok(())
}

fn validate_client_status_entrypoint(value: &str) -> Result<(), AddonError> {
    let path = Path::new(value);
    if value.is_empty()
        || value == "client/"
        || value.len() > MAX_CLIENT_STATUS_DETAIL_BYTES
        || value.contains('\\')
        || value.chars().any(|character| character.is_control())
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || !value.starts_with("client/")
    {
        return Err(AddonError::Invalid(
            "Invalid addon status entrypoint".to_string(),
        ));
    }
    Ok(())
}

fn validate_relative_client_path(value: &str) -> Result<(), AddonError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || !value.starts_with("client/")
    {
        return Err(AddonError::Invalid(
            "client_entrypoint must be a relative path below client/".to_string(),
        ));
    }
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> AddonError {
    AddonError::Io(error.to_string())
}

fn checked_addon_file(path: &Path, description: &str) -> Result<(), AddonError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AddonError::Invalid(format!("Invalid {description}")));
    }
    Ok(())
}

fn write_addon_file_atomic(
    path: &Path,
    bytes: &[u8],
    temporary_prefix: &str,
) -> Result<(), AddonError> {
    let parent = path
        .parent()
        .ok_or_else(|| AddonError::Io("Invalid addon file path".to_string()))?;
    reject_symlinked_path_ancestors(parent, "Invalid addon file parent")?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(io_error)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon file parent".to_string()));
    }
    let temporary = parent.join(format!(".{temporary_prefix}-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut temporary_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(io_error)?;
        use std::io::Write;
        temporary_file.write_all(bytes).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::symlink_metadata(&temporary)
                .map_err(io_error)?
                .permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&temporary, permissions).map_err(io_error)?;
        }
        fs::rename(&temporary, path).map_err(io_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_record(dir: &Path) -> Result<AddonRecord, AddonError> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let state_path = dir.join(STATE_FILE);
    checked_addon_file(&manifest_path, "addon manifest")?;
    checked_addon_file(&state_path, "addon state")?;
    let manifest_text = fs::read_to_string(&manifest_path).map_err(io_error)?;
    let manifest: AddonManifest = serde_json::from_str(&manifest_text).map_err(io_error)?;
    if manifest.kind == "client_extension" {
        validate_client_extension_manifest(&manifest)?;
        let signature_path = dir.join("manifest.sig");
        checked_addon_file(&signature_path, "addon signature")?;
        let entrypoint = manifest.client_entrypoint.as_deref().ok_or_else(|| {
            AddonError::Invalid("Client package entrypoint is missing".to_string())
        })?;
        let client_path = dir.join(entrypoint);
        checked_addon_file(&client_path, "client package entrypoint")?;
        verify_manifest_signature(&manifest_path, &signature_path)?;
        let expected_digest = manifest.client_sha256.as_deref().ok_or_else(|| {
            AddonError::Invalid("Client package digest is missing".to_string())
        })?;
        let actual_digest = sha256_file(&client_path)?;
        if !expected_digest.eq_ignore_ascii_case(&actual_digest) {
            return Err(AddonError::Invalid(
                "Client package entrypoint SHA-256 does not match manifest".to_string(),
            ));
        }
    } else {
        validate_manifest(&manifest)?;
    }
    let state_text = fs::read_to_string(&state_path).map_err(io_error)?;
    let state: AddonState = serde_json::from_str(&state_text).map_err(io_error)?;
    Ok(AddonRecord {
        manifest,
        enabled: state.enabled,
        installed_at: state.installed_at,
    })
}

pub fn list() -> Result<Vec<AddonRecord>, AddonError> {
    let Some(root) = checked_addon_storage_dir(false)? else {
        return Ok(Vec::new());
    };
    let mut records = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_symlink() {
            return Err(AddonError::Invalid("Invalid addon directory".to_string()));
        }
        if entry.file_name() == ROLLBACK_DIR {
            if !file_type.is_dir() {
                return Err(AddonError::Invalid("Invalid addon rollback directory".to_string()));
            }
            continue;
        }
        if file_type.is_dir() {
            records.push(read_record(&entry.path())?);
        }
    }
    records.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(records)
}

pub fn install(manifest: AddonManifest) -> Result<AddonRecord, AddonError> {
    validate_manifest(&manifest)?;
    let _ = checked_addon_storage_dir(true)?;
    let dir = addon_dir(&manifest.id)?;
    match fs::symlink_metadata(&dir) {
        Ok(_) => return Err(AddonError::AlreadyInstalled),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
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

pub fn update(manifest: AddonManifest) -> Result<AddonRecord, AddonError> {
    validate_manifest(&manifest)?;
    let _ = checked_addon_storage_dir(false)?;
    let dir = addon_dir(&manifest.id)?;
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AddonError::NotFound);
        }
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon directory".to_string()));
    }
    let existing = read_record(&dir)?;
    let manifest_text = serde_json::to_string_pretty(&manifest).map_err(io_error)?;
    write_addon_file_atomic(
        &dir.join(MANIFEST_FILE),
        manifest_text.as_bytes(),
        "manifest-update",
    )?;
    Ok(AddonRecord {
        manifest,
        enabled: existing.enabled,
        installed_at: existing.installed_at,
    })
}

pub fn set_enabled(id: &str, enabled: bool) -> Result<AddonRecord, AddonError> {
    let _ = checked_addon_storage_dir(false)?;
    let dir = addon_dir(id)?;
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AddonError::NotFound);
        }
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon directory".to_string()));
    }
    let mut record = read_record(&dir)?;
    record.enabled = enabled;
    let state = AddonState {
        enabled,
        installed_at: record.installed_at.clone(),
    };
    let state_text = serde_json::to_string_pretty(&state).map_err(io_error)?;
    write_addon_file_atomic(&dir.join(STATE_FILE), state_text.as_bytes(), "state")?;
    Ok(record)
}

pub fn remove(id: &str) -> Result<(), AddonError> {
    let _ = checked_addon_storage_dir(false)?;
    let dir = addon_dir(id)?;
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AddonError::NotFound);
        }
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon directory".to_string()));
    }
    // Removal is administrative cleanup: after validating the requested path,
    // treat the installed directory as opaque and do not read untrusted files.
    // Rollback storage is validated separately before it is removed.
    remove_signed_client_package_rollback(id)?;
    fs::remove_dir_all(dir).map_err(io_error)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedServiceStage {
    pub manifest: AddonManifest,
    pub state: String,
    pub staged_at: String,
}

const MAX_TRUSTED_SERVICE_STAGES: usize = 1_024;

fn trusted_service_signing_public_key() -> PathBuf {
    std::env::var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/screentra-cast/trusted-service-signing.pub"))
}

fn checked_trusted_service_storage_dir(create: bool) -> Result<Option<PathBuf>, AddonError> {
    let Some(data_root) = checked_data_dir(create)? else {
        return Ok(None);
    };
    let root = data_root.join("trusted-services");
    match fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid trusted service storage directory".to_string(),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    return Err(AddonError::Invalid(
                        "Trusted service storage directory permissions are insecure".to_string(),
                    ));
                }
            }
            Ok(Some(root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            let mut directory_builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory_builder.mode(0o700);
            }
            directory_builder.create(&root).map_err(io_error)?;
            let metadata = fs::symlink_metadata(&root).map_err(io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid trusted service storage directory".to_string(),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o777 != 0o700 {
                    return Err(AddonError::Invalid(
                        "Trusted service storage directory permissions are insecure".to_string(),
                    ));
                }
            }
            Ok(Some(root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn validate_trusted_service_file(
    path: &Path,
    maximum_bytes: u64,
    description: &str,
) -> Result<(), AddonError> {
    reject_symlinked_path_ancestors(
        path,
        &format!("Trusted service {description} has a symlinked path ancestor"),
    )?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AddonError::Invalid(format!(
            "Trusted service {description} is not a regular file"
        )));
    }
    if metadata.len() > maximum_bytes {
        return Err(AddonError::Invalid(format!(
            "Trusted service {description} is too large"
        )));
    }
    Ok(())
}

fn trusted_service_signature_verifier_executable() -> &'static str {
    "/usr/bin/openssl"
}

fn verify_trusted_service_manifest_signature(
    manifest_path: &Path,
    signature_path: &Path,
) -> Result<(), AddonError> {
    let public_key = trusted_service_signing_public_key();
    validate_trusted_service_file(&public_key, 64 * 1024, "manifest signing public key")?;
    let result = Command::new(trusted_service_signature_verifier_executable())
        .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
        .arg(&public_key)
        .args(["-sigfile"])
        .arg(signature_path)
        .args(["-in"])
        .arg(manifest_path)
        .output()
        .map_err(io_error)?;
    if !result.status.success() {
        return Err(AddonError::Invalid(
            "Trusted service manifest signature verification failed".to_string(),
        ));
    }
    Ok(())
}

fn trusted_service_image_signing_public_key() -> PathBuf {
    std::env::var("SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/screentra-cast/trusted-service-image-signing.pub"))
}

fn verify_trusted_service_image_signature(
    image_digest: &str,
    signature_path: &Path,
) -> Result<(), AddonError> {
    let public_key = trusted_service_image_signing_public_key();
    validate_trusted_service_file(&public_key, 64 * 1024, "image signing public key")?;
    let digest_path = std::env::temp_dir().join(format!("screentra-trusted-image-digest-{}", Uuid::new_v4()));
    let result = (|| {
        let mut digest_options = fs::OpenOptions::new();
        digest_options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            digest_options.mode(0o600);
        }
        let mut digest_file = digest_options.open(&digest_path).map_err(io_error)?;
        use std::io::Write;
        digest_file
            .write_all(image_digest.as_bytes())
            .map_err(io_error)?;
        let output = Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
            .arg(&public_key)
            .args(["-sigfile"])
            .arg(signature_path)
            .args(["-in"])
            .arg(&digest_path)
            .output()
            .map_err(io_error)?;
        if !output.status.success() {
            return Err(AddonError::Invalid(
                "Trusted service image signature verification failed".to_string(),
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_file(&digest_path);
    result
}

pub(crate) fn validate_trusted_service_manifest(manifest: &AddonManifest) -> Result<(), AddonError> {
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
    if manifest.kind != "trusted_server_service" {
        return Err(AddonError::Invalid(
            "Trusted service manifest kind is invalid".to_string(),
        ));
    }
    let service = manifest.trusted_service.as_ref().ok_or_else(|| {
        AddonError::Invalid("trusted_server_service requires trusted_service metadata".to_string())
    })?;
    validate_trusted_service_spec(service)?;
    if manifest.client_entrypoint.is_some()
        || manifest.server_entrypoint.is_some()
        || manifest.client_sha256.is_some()
        || manifest.client_api_version.is_some()
    {
        return Err(AddonError::Invalid(
            "Trusted service manifests cannot contain executable client/server entrypoints"
                .to_string(),
        ));
    }
    if !manifest.widget_types.is_empty() {
        return Err(AddonError::Invalid(
            "Trusted service manifests cannot declare renderer widgets".to_string(),
        ));
    }
    Ok(())
}

fn create_private_trusted_service_stage_dir(path: &Path) -> Result<(), AddonError> {
    let parent = path
        .parent()
        .ok_or_else(|| AddonError::Invalid("Trusted service stage parent is missing".to_string()))?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(io_error)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(AddonError::Invalid(
            "Trusted service stage parent is not a real directory".to_string(),
        ));
    }
    let mut directory_builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory_builder.mode(0o700);
    }
    directory_builder.create(path).map_err(io_error)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        let _ = fs::remove_dir(path);
        return Err(AddonError::Invalid(
            "Trusted service stage is not a real directory".to_string(),
        ));
    }
    Ok(())
}

fn copy_private_trusted_service_file(source: &Path, target: &Path) -> Result<(), AddonError> {
    let mut input = fs::File::open(source).map_err(io_error)?;
    let mut output_options = fs::OpenOptions::new();
    output_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        output_options.mode(0o600);
    }
    let mut output = output_options
        .open(target)
        .map_err(io_error)?;
    if let Err(error) = io::copy(&mut input, &mut output) {
        let _ = fs::remove_file(target);
        return Err(io_error(error));
    }
    output.flush().map_err(io_error)?;
    let metadata = fs::symlink_metadata(target).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        let _ = fs::remove_file(target);
        return Err(AddonError::Invalid(
            "Trusted service stage file is not regular".to_string(),
        ));
    }
    Ok(())
}

fn write_private_trusted_service_file(path: &Path, bytes: &[u8]) -> Result<(), AddonError> {
    let mut file_options = fs::OpenOptions::new();
    file_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file_options.mode(0o600);
    }
    let mut file = file_options.open(path).map_err(io_error)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.flush()) {
        let _ = fs::remove_file(path);
        return Err(io_error(error));
    }
    Ok(())
}

fn rename_trusted_service_stage_noreplace(
    source: &Path,
    target: &Path,
) -> Result<(), AddonError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
            AddonError::Invalid("Trusted service stage path contains an invalid byte".to_string())
        })?;
        let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
            AddonError::Invalid("Trusted service target path contains an invalid byte".to_string())
        })?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(AddonError::AlreadyInstalled);
        }
        return Err(io_error(error));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, target);
        Err(AddonError::Invalid(
            "Trusted service staging requires atomic no-replace rename support".to_string(),
        ))
    }
}

pub fn stage_trusted_service_manifest(
    manifest_path: &Path,
    signature_path: &Path,
    image_signature_path: &Path,
) -> Result<TrustedServiceStage, AddonError> {
    validate_trusted_service_file(manifest_path, 1024 * 1024, "manifest")?;
    validate_trusted_service_file(signature_path, 64 * 1024, "manifest signature")?;
    verify_trusted_service_manifest_signature(manifest_path, signature_path)?;
    let manifest_bytes = fs::read(manifest_path).map_err(io_error)?;
    let manifest: AddonManifest = serde_json::from_slice(&manifest_bytes).map_err(io_error)?;
    validate_trusted_service_manifest(&manifest)?;
    validate_trusted_service_file(image_signature_path, 64 * 1024, "image signature")?;
    let image_digest = manifest
        .trusted_service
        .as_ref()
        .ok_or_else(|| AddonError::Invalid("Trusted service metadata is missing".to_string()))?
        .image_digest
        .as_str();
    verify_trusted_service_image_signature(image_digest, image_signature_path)?;

    let root = checked_trusted_service_storage_dir(true)?
        .ok_or_else(|| AddonError::Io("Trusted service storage directory is unavailable".to_string()))?;
    let target = root.join(&manifest.id);
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(AddonError::AlreadyInstalled),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let stage = root.join(format!(".stage-{}", Uuid::new_v4()));
    #[cfg(unix)]
    let mut stage_identity = None;
    let result = (|| {
        create_private_trusted_service_stage_dir(&stage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(&stage).map_err(io_error)?;
            stage_identity = Some((metadata.dev(), metadata.ino()));
        }
        copy_private_trusted_service_file(manifest_path, &stage.join(MANIFEST_FILE))?;
        copy_private_trusted_service_file(signature_path, &stage.join("manifest.sig"))?;
        copy_private_trusted_service_file(image_signature_path, &stage.join("image.sig"))?;
        let staged_at = chrono::Utc::now().to_rfc3339();
        write_private_trusted_service_file(
            &stage.join(STATE_FILE),
            &serde_json::to_vec(&serde_json::json!({
                "state": "staged",
                "staged_at": &staged_at,
            }))
            .map_err(io_error)?,
        )?;
        let validated_stage = read_trusted_service_stage(&stage)?;
        rename_trusted_service_stage_noreplace(&stage, &target)?;
        Ok(validated_stage)
    })();
    if result.is_err() {
        #[cfg(unix)]
        if let Some(identity) = stage_identity {
            use std::os::unix::fs::MetadataExt;
            let owned = fs::symlink_metadata(&stage)
                .map(|metadata| {
                    !metadata.file_type().is_symlink()
                        && metadata.is_dir()
                        && (metadata.dev(), metadata.ino()) == identity
                })
                .unwrap_or(false);
            if owned {
                let _ = fs::remove_dir_all(&stage);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::remove_dir_all(&stage);
        }
    }
    result
}

fn read_trusted_service_stage(dir: &Path) -> Result<TrustedServiceStage, AddonError> {
    reject_symlinked_path_ancestors(
        dir,
        "Invalid staged trusted service directory path",
    )?;
    let dir_metadata = fs::symlink_metadata(dir).map_err(io_error)?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        return Err(AddonError::Invalid(
            "Invalid staged trusted service directory".to_string(),
        ));
    }
    let manifest_path = dir.join(MANIFEST_FILE);
    let signature_path = dir.join("manifest.sig");
    let image_signature_path = dir.join("image.sig");
    let state_path = dir.join(STATE_FILE);
    validate_trusted_service_file(&manifest_path, 1024 * 1024, "manifest")?;
    validate_trusted_service_file(&signature_path, 64 * 1024, "manifest signature")?;
    validate_trusted_service_file(&image_signature_path, 64 * 1024, "image signature")?;
    validate_trusted_service_file(&state_path, 64 * 1024, "state")?;
    verify_trusted_service_manifest_signature(&manifest_path, &signature_path)?;
    let manifest_bytes = fs::read(&manifest_path).map_err(io_error)?;
    let manifest: AddonManifest = serde_json::from_slice(&manifest_bytes).map_err(io_error)?;
    validate_trusted_service_manifest(&manifest)?;
    let image_digest = manifest
        .trusted_service
        .as_ref()
        .ok_or_else(|| AddonError::Invalid("Trusted service metadata is missing".to_string()))?
        .image_digest
        .as_str();
    verify_trusted_service_image_signature(image_digest, &image_signature_path)?;
    let state_bytes = fs::read(&state_path).map_err(io_error)?;
    let state: serde_json::Value = serde_json::from_slice(&state_bytes).map_err(io_error)?;
    let state_name = state
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AddonError::Io("Staged trusted service state is invalid".to_string()))?;
    let staged_at = state
        .get("staged_at")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 64
                && chrono::DateTime::parse_from_rfc3339(value).is_ok()
        })
        .ok_or_else(|| AddonError::Io("Staged trusted service timestamp is invalid".to_string()))?;
    if state_name != "staged" {
        return Err(AddonError::Io(
            "Staged trusted service state is invalid".to_string(),
        ));
    }
    Ok(TrustedServiceStage {
        manifest,
        state: state_name.to_string(),
        staged_at: staged_at.to_string(),
    })
}

pub fn get_trusted_service_stage(id: &str) -> Result<TrustedServiceStage, AddonError> {
    validate_id(id)?;
    let root = checked_trusted_service_storage_dir(false)?.ok_or(AddonError::NotFound)?;
    let dir = root.join(id);
    let metadata = fs::symlink_metadata(&dir).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AddonError::NotFound
        } else {
            io_error(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::Invalid(
            "Invalid staged trusted service directory".to_string(),
        ));
    }
    let stage = read_trusted_service_stage(&dir)?;
    if stage.manifest.id != id {
        return Err(AddonError::Invalid(
            "Trusted service stage directory identity does not match manifest".to_string(),
        ));
    }
    Ok(stage)
}

pub fn list_trusted_service_stages() -> Result<Vec<TrustedServiceStage>, AddonError> {
    let Some(root) = checked_trusted_service_storage_dir(false)? else {
        return Ok(Vec::new());
    };
    let mut stages = Vec::new();
    for entry in fs::read_dir(&root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if stages.len() >= MAX_TRUSTED_SERVICE_STAGES {
            return Err(AddonError::Invalid(
                "Too many trusted service stages".to_string(),
            ));
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_symlink() {
            return Err(AddonError::Invalid(
                "Invalid staged trusted service directory".to_string(),
            ));
        }
        if name.starts_with('.') {
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        validate_id(&name).map_err(|_| {
            AddonError::Invalid("Invalid staged trusted service directory".to_string())
        })?;
        let stage = read_trusted_service_stage(&entry.path())?;
        if stage.manifest.id != name {
            return Err(AddonError::Invalid(
                "Trusted service stage directory identity does not match manifest".to_string(),
            ));
        }
        stages.push(stage);
    }
    stages.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(stages)
}

pub fn remove_trusted_service_stage(id: &str) -> Result<(), AddonError> {
    validate_id(id)?;
    let Some(root) = checked_trusted_service_storage_dir(false)? else {
        return Ok(());
    };
    #[cfg(unix)]
    let root_identity = {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(&root).map_err(io_error)?;
        (metadata.dev(), metadata.ino())
    };
    let dir = root.join(id);
    let metadata = match fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AddonError::Invalid(
            "Invalid staged trusted service directory".to_string(),
        ));
    }
    #[cfg(unix)]
    let original_identity = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    match read_trusted_service_stage(&dir) {
        Ok(stage) if stage.manifest.id != id => {
            return Err(AddonError::Invalid(
                "Trusted service stage directory identity does not match manifest".to_string(),
            ));
        }
        Ok(_) | Err(_) => {
            // Explicit admin cleanup remains available for malformed staged
            // metadata; the directory was already verified as non-symlink.
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current = fs::symlink_metadata(&dir).map_err(io_error)?;
        if current.file_type().is_symlink()
            || !current.is_dir()
            || (current.dev(), current.ino()) != original_identity
        {
            return Err(AddonError::Invalid(
                "Trusted service stage identity changed before removal".to_string(),
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let current_root = fs::symlink_metadata(&root).map_err(io_error)?;
        if current_root.file_type().is_symlink()
            || !current_root.is_dir()
            || current_root.permissions().mode() & 0o777 != 0o700
            || (current_root.dev(), current_root.ino()) != root_identity
        {
            return Err(AddonError::Invalid(
                "Trusted service storage root changed before removal".to_string(),
            ));
        }
    }
    fs::remove_dir_all(dir).map_err(io_error)
}

fn addon_signing_public_key() -> PathBuf {
    std::env::var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/screentra-cast/addon-signing.pub"))
}

fn verify_manifest_signature(manifest: &Path, signature: &Path) -> Result<(), AddonError> {
    checked_addon_file(manifest, "addon manifest")?;
    checked_addon_file(signature, "addon signature")?;
    let public_key = addon_signing_public_key();
    match fs::symlink_metadata(&public_key) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AddonError::Invalid(format!(
                "Addon signing public key is missing: {}",
                public_key.display()
            )));
        }
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > 64 * 1024 =>
        {
            return Err(AddonError::Invalid(
                "Addon signing public key is invalid".to_string(),
            ));
        }
        Err(error) => return Err(io_error(error)),
        Ok(_) => {}
    }
    let result = Command::new(trusted_service_signature_verifier_executable())
        .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
        .arg(&public_key)
        .args(["-sigfile"])
        .arg(signature)
        .args(["-in"])
        .arg(manifest)
        .output()
        .map_err(io_error)?;
    if !result.status.success() {
        return Err(AddonError::Invalid(
            "Addon manifest signature verification failed".to_string(),
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, AddonError> {
    let mut file = fs::File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

struct StagedSignedClientPackage {
    directory: PathBuf,
    manifest: AddonManifest,
}

fn stage_signed_client_package(
    archive: &Path,
    root: &Path,
    stage_prefix: &str,
) -> Result<StagedSignedClientPackage, AddonError> {
    reject_symlinked_path_ancestors(root, "Invalid addon storage directory")?;
    let stage = root.join(format!(".{stage_prefix}-{}", Uuid::new_v4()));
    if let Err(error) = crate::safe_archive::extract_tar_gz(archive, &stage, 0) {
        let _ = fs::remove_dir_all(&stage);
        return Err(AddonError::Invalid(error));
    }
    let result = (|| {
        let manifest_path = stage.join(MANIFEST_FILE);
        let signature_path = stage.join("manifest.sig");
        checked_addon_file(&manifest_path, "addon package manifest")?;
        checked_addon_file(&signature_path, "addon package signature")?;
        verify_manifest_signature(&manifest_path, &signature_path)?;
        let manifest_bytes = fs::read(&manifest_path).map_err(io_error)?;
        let manifest: AddonManifest = serde_json::from_slice(&manifest_bytes).map_err(io_error)?;
        validate_client_extension_manifest(&manifest)?;
        let entrypoint = manifest.client_entrypoint.as_deref().ok_or_else(|| {
            AddonError::Invalid("Client package entrypoint is missing".to_string())
        })?;
        let client_file = stage.join(entrypoint);
        checked_addon_file(&client_file, "client package entrypoint")?;
        let actual_digest = sha256_file(&client_file)?;
        if !manifest
            .client_sha256
            .as_deref()
            .unwrap_or_default()
            .eq_ignore_ascii_case(&actual_digest)
        {
            return Err(AddonError::Invalid(
                "Client package entrypoint SHA-256 does not match manifest".to_string(),
            ));
        }
        Ok(StagedSignedClientPackage {
            directory: stage.clone(),
            manifest,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

/// Validate and persist a signed client-extension package. The package is
/// never executed by the server; the client-loader phase will consume the
/// stored manifest and client directory after its own signature verification.
pub fn install_signed_client_package(archive: &Path) -> Result<AddonRecord, AddonError> {
    let root = checked_addon_storage_dir(true)?
        .ok_or_else(|| AddonError::Io("Addon storage directory is unavailable".to_string()))?;
    let staged = stage_signed_client_package(archive, &root, "package-install")?;
    let result = (|| {
        let destination = addon_dir(&staged.manifest.id)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => return Err(AddonError::AlreadyInstalled),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        let installed_at = chrono::Utc::now().to_rfc3339();
        let state = AddonState {
            enabled: true,
            installed_at: installed_at.clone(),
        };
        let state_text = serde_json::to_string_pretty(&state).map_err(io_error)?;
        fs::write(staged.directory.join(STATE_FILE), state_text).map_err(io_error)?;
        fs::rename(&staged.directory, &destination).map_err(io_error)?;
        Ok(AddonRecord {
            manifest: staged.manifest,
            enabled: true,
            installed_at,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staged.directory);
    }
    result
}

fn checked_addon_rollback_root(
    root: &Path,
    create: bool,
) -> Result<Option<PathBuf>, AddonError> {
    reject_symlinked_path_ancestors(root, "Invalid addon storage directory")?;
    let rollback_root = root.join(ROLLBACK_DIR);
    match fs::symlink_metadata(&rollback_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid addon rollback directory".to_string()),
        ),
        Ok(_) => Ok(Some(rollback_root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir(&rollback_root).map_err(io_error)?;
            let metadata = fs::symlink_metadata(&rollback_root).map_err(io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AddonError::Invalid(
                    "Invalid addon rollback directory".to_string(),
                ));
            }
            Ok(Some(rollback_root))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

fn remove_signed_client_package_rollback(id: &str) -> Result<(), AddonError> {
    let root = checked_addon_storage_dir(false)?.ok_or(AddonError::NotFound)?;
    let Some(rollback_root) = checked_addon_rollback_root(&root, false)? else {
        return Ok(());
    };
    let backup = rollback_root.join(id);
    reject_symlinked_path_ancestors(&backup, "Invalid addon rollback path")?;
    match fs::symlink_metadata(&backup) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid addon rollback directory".to_string()),
        ),
        Ok(_) => fs::remove_dir_all(backup).map_err(io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn replace_addon_directory_atomically(
    staged: &Path,
    destination: &Path,
    backup: &Path,
) -> Result<(), AddonError> {
    let parent = destination
        .parent()
        .ok_or_else(|| AddonError::Io("Invalid addon directory path".to_string()))?;
    reject_symlinked_path_ancestors(parent, "Invalid addon storage directory")?;
    let backup_parent = backup
        .parent()
        .ok_or_else(|| AddonError::Io("Invalid addon rollback path".to_string()))?;
    reject_symlinked_path_ancestors(backup_parent, "Invalid addon rollback path")?;
    let backup_parent_metadata = fs::symlink_metadata(backup_parent).map_err(io_error)?;
    if backup_parent_metadata.file_type().is_symlink() || !backup_parent_metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon rollback path".to_string()));
    }
    let destination_metadata = fs::symlink_metadata(destination).map_err(io_error)?;
    if destination_metadata.file_type().is_symlink() || !destination_metadata.is_dir() {
        return Err(AddonError::Invalid("Invalid addon directory".to_string()));
    }
    match fs::symlink_metadata(backup) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(AddonError::Invalid("Invalid addon rollback directory".to_string()));
        }
        Ok(_) => fs::remove_dir_all(backup).map_err(io_error)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    fs::rename(destination, backup).map_err(io_error)?;
    match fs::rename(staged, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::rename(backup, destination);
            Err(io_error(error))
        }
    }
}

fn client_package_version_is_newer(candidate: &str, installed: &str) -> bool {
    fn numeric_components(version: &str) -> Option<Vec<u64>> {
        version
            .split('.')
            .map(|component| component.parse::<u64>().ok())
            .collect()
    }

    match (numeric_components(candidate), numeric_components(installed)) {
        (Some(candidate), Some(installed)) => candidate > installed,
        _ => false,
    }
}

/// Validate and atomically replace an installed signed client-extension
/// package. This contract-only path preserves the installed lifecycle state;
/// it never executes client JavaScript or enables a runtime.
pub fn update_signed_client_package(archive: &Path) -> Result<AddonRecord, AddonError> {
    let root = checked_addon_storage_dir(false)?.ok_or(AddonError::NotFound)?;
    let staged = stage_signed_client_package(archive, &root, "package-update")?;
    let result = (|| {
        let destination = addon_dir(&staged.manifest.id)?;
        let existing = match fs::symlink_metadata(&destination) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(AddonError::Invalid("Invalid addon directory".to_string()));
                }
                read_record(&destination)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AddonError::Invalid(
                    "Signed client package update identity does not match an installed package"
                        .to_string(),
                ));
            }
            Err(error) => return Err(io_error(error)),
        };
        if existing.manifest.id != staged.manifest.id
            || existing.manifest.kind != "client_extension"
        {
            return Err(AddonError::Invalid(
                "Signed client package update identity does not match installed package"
                    .to_string(),
            ));
        }
        if !client_package_version_is_newer(
            &staged.manifest.version,
            &existing.manifest.version,
        ) {
            return Err(AddonError::Invalid(
                "Signed client package update must use a strictly newer version".to_string(),
            ));
        }
        let state = AddonState {
            enabled: existing.enabled,
            installed_at: existing.installed_at.clone(),
        };
        let state_text = serde_json::to_string_pretty(&state).map_err(io_error)?;
        fs::write(staged.directory.join(STATE_FILE), state_text).map_err(io_error)?;
        let rollback_root = checked_addon_rollback_root(&root, true)?.ok_or_else(|| {
            AddonError::Io("Addon rollback storage is unavailable".to_string())
        })?;
        let backup = rollback_root.join(&staged.manifest.id);
        replace_addon_directory_atomically(&staged.directory, &destination, &backup)?;
        Ok(AddonRecord {
            manifest: staged.manifest,
            enabled: existing.enabled,
            installed_at: existing.installed_at,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staged.directory);
    }
    result
}

/// Atomically restore the one retained, signed package for a client extension.
/// This is package evidence only; it never loads or executes client JavaScript.
pub fn rollback_signed_client_package(id: &str) -> Result<AddonRecord, AddonError> {
    validate_id(id).map_err(|_| {
        AddonError::Invalid("Invalid signed client package identity".to_string())
    })?;
    let root = checked_addon_storage_dir(false)?.ok_or(AddonError::NotFound)?;
    let rollback_root = checked_addon_rollback_root(&root, false)?.ok_or(AddonError::NotFound)?;
    let destination = root.join(id);
    let backup = rollback_root.join(id);
    let rollback_error = || AddonError::Invalid("Signed client package rollback failed".to_string());

    let current_metadata = fs::symlink_metadata(&destination).map_err(|_| rollback_error())?;
    let backup_metadata = fs::symlink_metadata(&backup).map_err(|_| AddonError::NotFound)?;
    if current_metadata.file_type().is_symlink()
        || !current_metadata.is_dir()
        || backup_metadata.file_type().is_symlink()
        || !backup_metadata.is_dir()
    {
        return Err(rollback_error());
    }
    let current = read_record(&destination).map_err(|_| rollback_error())?;
    let previous = read_record(&backup).map_err(|_| rollback_error())?;
    if current.manifest.id != id
        || current.manifest.kind != "client_extension"
        || previous.manifest.id != id
        || previous.manifest.kind != "client_extension"
        || current.enabled != previous.enabled
        || current.installed_at != previous.installed_at
    {
        return Err(rollback_error());
    }

    let temporary = root.join(format!(".rollback-swap-{}-{}", id, Uuid::new_v4()));
    reject_symlinked_path_ancestors(&root, "Invalid addon storage directory")
        .map_err(|_| rollback_error())?;
    fs::rename(&destination, &temporary).map_err(|_| rollback_error())?;
    if let Err(_error) = fs::rename(&backup, &destination) {
        let _ = fs::rename(&temporary, &destination);
        return Err(rollback_error());
    }
    if let Err(_error) = fs::rename(&temporary, &backup) {
        let restored = fs::rename(&destination, &backup)
            .and_then(|_| fs::rename(&temporary, &destination));
        if restored.is_err() {
            return Err(rollback_error());
        }
        return Err(rollback_error());
    }
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

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
                host_renderer: "weather".to_string(),
                icon: "🌤️".to_string(),
                default_props: serde_json::json!({"units": "metric"}),
            }],
            permissions: Vec::new(),
            client_capabilities: Vec::new(),
            data_contract: None,
            client_entrypoint: None,
            server_entrypoint: None,
            trusted_service: None,
            client_service: None,
            client_sha256: None,
            client_api_version: None,
        }
    }

    #[test]
    fn host_renderer_wire_name_accepts_legacy_renderer_type_manifests() {
        let legacy = serde_json::json!({
            "id": "example.weather",
            "name": "Example Weather",
            "renderer_type": "weather"
        });
        let parsed: AddonWidgetType = serde_json::from_value(legacy)
            .expect("legacy renderer_type manifests remain readable");
        assert_eq!(parsed.host_renderer, "weather");
        let encoded = serde_json::to_value(parsed).expect("widget type should serialize");
        assert_eq!(encoded["host_renderer"], "weather");
        assert!(encoded.get("renderer_type").is_none());
    }

    fn launch_profile() -> TrustedServiceLaunchProfile {
        TrustedServiceLaunchProfile {
            command: vec!["/service".to_string()],
            gateway_socket_path: "/run/screentra/gateway.sock".to_string(),
            rootless: true,
            non_root: true,
            network_disabled: true,
            read_only_rootfs: true,
            no_new_privileges: true,
            drop_all_capabilities: true,
            seccomp_default: true,
            memory_bytes: 256 * 1024 * 1024,
            cpu_millis: 500,
            pids: 64,
            writable_bytes: 16 * 1024 * 1024,
        }
    }

    #[test]
    fn trusted_service_signature_verification_uses_a_fixed_absolute_executable() {
        let executable = trusted_service_signature_verifier_executable();
        assert!(executable.starts_with('/'));
        assert_eq!(executable, "/usr/bin/openssl");
        assert!(!executable.contains(".."));
    }

    #[test]
    fn validates_explicit_trusted_service_launch_profile() {
        validate_trusted_service_launch_profile(&launch_profile())
            .expect("bounded rootless launch profile should validate");

        let mut shell = launch_profile();
        shell.command = vec!["/bin/sh".to_string(), "-c".to_string(), "run".to_string()];
        assert!(validate_trusted_service_launch_profile(&shell).is_err());

        let mut gateway = launch_profile();
        gateway.gateway_socket_path = "/tmp/host.sock".to_string();
        assert!(validate_trusted_service_launch_profile(&gateway).is_err());

        let mut oversized = launch_profile();
        oversized.memory_bytes = 4 * 1024 * 1024 * 1024 + 1;
        assert!(validate_trusted_service_launch_profile(&oversized).is_err());
    }

    fn client_manifest(client_sha256: &str) -> AddonManifest {
        let mut value = manifest();
        value.kind = "client_extension".to_string();
        value.client_entrypoint = Some("client/index.js".to_string());
        value.client_sha256 = Some(client_sha256.to_string());
        value.client_api_version = Some("1".to_string());
        value.widget_types[0].host_renderer.clear();
        value
    }

    struct SignedClientPackageFixture {
        root: PathBuf,
        addon_dir: PathBuf,
        old_data_root: Option<std::ffi::OsString>,
        old_public_key: Option<std::ffi::OsString>,
    }

    impl Drop for SignedClientPackageFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            unsafe {
                match self.old_data_root.take() {
                    Some(value) => std::env::set_var("SIGNAGE_DATA_DIR", value),
                    None => std::env::remove_var("SIGNAGE_DATA_DIR"),
                }
                match self.old_public_key.take() {
                    Some(value) => std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", value),
                    None => std::env::remove_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY"),
                }
            }
        }
    }

    fn installed_signed_client_package_fixture() -> SignedClientPackageFixture {
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-package-readback-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let package_root = root.join("package-root");
        let client_root = package_root.join("client");
        fs::create_dir_all(&client_root).unwrap();
        let client_bytes = b"window.ScreentraAddon = { apiVersion: '1' };\\n";
        let client_path = client_root.join("index.js");
        fs::write(&client_path, client_bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(client_bytes));
        let manifest = client_manifest(&digest);
        let manifest_path = package_root.join(MANIFEST_FILE);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let signature_path = package_root.join("manifest.sig");
        let private_key = root.join("addon-signing.key");
        let public_key = root.join("addon-signing.pub");
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        let archive = root.join("addon.tar.gz");
        let archive_file = fs::File::create(&archive).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder
            .append_path_with_name(&manifest_path, MANIFEST_FILE)
            .unwrap();
        builder
            .append_path_with_name(&signature_path, "manifest.sig")
            .unwrap();
        builder.append_dir_all("client", &client_root).unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        let old_public_key = std::env::var_os("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY");
        let fixture = SignedClientPackageFixture {
            root,
            addon_dir: data_root.join("addons/example.weather"),
            old_data_root,
            old_public_key,
        };
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
            std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", &public_key);
        }
        install_signed_client_package(&archive).unwrap();
        fixture
    }

    fn signed_client_package_archive(
        root: &Path,
        id: &str,
        version: &str,
        manifest_payload: &[u8],
        archive_payload: &[u8],
    ) -> PathBuf {
        let package_root = root.join(format!("update-package-{}-{}", id, Uuid::new_v4()));
        let client_root = package_root.join("client");
        fs::create_dir_all(&client_root).unwrap();
        let digest = format!("{:x}", Sha256::digest(manifest_payload));
        let mut manifest = client_manifest(&digest);
        manifest.id = id.to_string();
        manifest.version = version.to_string();
        manifest.name = format!("Updated {id}");
        let manifest_path = package_root.join(MANIFEST_FILE);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let client_path = client_root.join("index.js");
        fs::write(&client_path, archive_payload).unwrap();
        let signature_path = package_root.join("manifest.sig");
        let private_key = root.join("addon-signing.key");
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        let archive = root.join(format!("update-{}.tar.gz", Uuid::new_v4()));
        let archive_file = fs::File::create(&archive).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder
            .append_path_with_name(&manifest_path, MANIFEST_FILE)
            .unwrap();
        builder
            .append_path_with_name(&signature_path, "manifest.sig")
            .unwrap();
        builder.append_dir_all("client", &client_root).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        archive
    }

    #[test]
    fn signed_client_package_update_replaces_same_identity_and_preserves_disabled_state() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        set_enabled("example.weather", false).unwrap();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );

        let updated = update_signed_client_package(&archive).unwrap();

        assert_eq!(updated.manifest.id, "example.weather");
        assert_eq!(updated.manifest.version, "0.2.0");
        assert!(!updated.enabled);
        assert_eq!(
            fs::read(fixture.addon_dir.join("client/index.js")).unwrap(),
            new_payload
        );
        let persisted = list()
            .unwrap()
            .into_iter()
            .find(|record| record.manifest.id == "example.weather")
            .unwrap();
        assert!(!persisted.enabled);
        assert_eq!(persisted.manifest.version, "0.2.0");
    }

    #[test]
    fn signed_client_package_rollback_restores_previous_package_and_preserves_state() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        set_enabled("example.weather", false).unwrap();
        let before_update = list()
            .unwrap()
            .into_iter()
            .find(|record| record.manifest.id == "example.weather")
            .unwrap();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let old_payload = b"window.ScreentraAddon = { apiVersion: '1' };\\n";

        let rolled_back = rollback_signed_client_package("example.weather").unwrap();

        assert_eq!(rolled_back.manifest.version, "0.1.0");
        assert!(!rolled_back.enabled);
        assert_eq!(rolled_back.installed_at, before_update.installed_at);
        assert_eq!(
            fs::read(fixture.addon_dir.join("client/index.js")).unwrap(),
            old_payload
        );
        let retained_current = read_record(
            &fixture
                .addon_dir
                .parent()
                .unwrap()
                .join(".rollback/example.weather"),
        )
        .unwrap();
        assert_eq!(retained_current.manifest.version, "0.2.0");
        assert!(!retained_current.enabled);
        assert_eq!(retained_current.installed_at, before_update.installed_at);
    }

    #[test]
    fn signed_client_package_rollback_rejects_tampered_backup_without_changing_current() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let backup = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback/example.weather");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(backup.join(MANIFEST_FILE)).unwrap()).unwrap();
        manifest["name"] = serde_json::Value::String("tampered rollback".to_string());
        fs::write(backup.join(MANIFEST_FILE), serde_json::to_vec(&manifest).unwrap()).unwrap();

        assert!(rollback_signed_client_package("example.weather").is_err());
        assert_eq!(
            fs::read(fixture.addon_dir.join("client/index.js")).unwrap(),
            new_payload
        );
        assert_eq!(read_record(&fixture.addon_dir).unwrap().manifest.version, "0.2.0");
    }

    #[test]
    fn signed_client_package_rollback_rejects_missing_backup_and_malformed_identity() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let old_payload = fs::read(fixture.addon_dir.join("client/index.js")).unwrap();

        assert!(rollback_signed_client_package("example.weather").is_err());
        assert!(rollback_signed_client_package("../example.weather").is_err());
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), old_payload);
    }

    #[cfg(unix)]
    #[test]
    fn signed_client_package_rollback_rejects_symlinked_backup_without_changing_current() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let backup = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback/example.weather");
        let outside = fixture.root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::remove_dir_all(&backup).unwrap();
        symlink(&outside, &backup).unwrap();

        assert!(rollback_signed_client_package("example.weather").is_err());
        assert_eq!(read_record(&fixture.addon_dir).unwrap().manifest.version, "0.2.0");
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
    }

    #[cfg(unix)]
    #[test]
    fn signed_client_package_remove_rejects_symlinked_rollback_without_deleting_current() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let backup = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback/example.weather");
        let outside = fixture.root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::remove_dir_all(&backup).unwrap();
        symlink(&outside, &backup).unwrap();

        assert!(remove("example.weather").is_err());
        assert_eq!(read_record(&fixture.addon_dir).unwrap().manifest.version, "0.2.0");
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
        assert!(backup.is_symlink());
        assert!(outside.is_dir());
    }

    #[test]
    fn signed_client_package_remove_rejects_non_directory_rollback_without_deleting_current() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let backup = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback/example.weather");
        fs::remove_dir_all(&backup).unwrap();
        fs::write(&backup, b"not a rollback directory").unwrap();

        assert!(remove("example.weather").is_err());
        assert_eq!(read_record(&fixture.addon_dir).unwrap().manifest.version, "0.2.0");
        assert_eq!(fs::read(fixture.addon_dir.join("client/index.js")).unwrap(), new_payload);
        assert!(backup.is_file());
    }

    fn assert_remove_cleans_tampered_signed_client_package(
        tampered_path: &str,
        tampered_contents: &[u8],
    ) {
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let rollback = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(ROLLBACK_DIR)
            .join("example.weather");
        assert!(rollback.is_dir());
        fs::write(fixture.addon_dir.join(tampered_path), tampered_contents).unwrap();

        remove("example.weather").unwrap();

        assert!(!fixture.addon_dir.exists());
        assert!(!rollback.exists());
    }

    #[test]
    fn signed_client_package_remove_cleans_tampered_manifest() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        assert_remove_cleans_tampered_signed_client_package(MANIFEST_FILE, b"not json");
    }

    #[test]
    fn signed_client_package_remove_cleans_tampered_signature() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        assert_remove_cleans_tampered_signed_client_package("manifest.sig", b"not a signature");
    }

    #[test]
    fn signed_client_package_remove_cleans_tampered_client_entrypoint() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        assert_remove_cleans_tampered_signed_client_package("client/index.js", b"tampered");
    }

    #[test]
    fn signed_client_package_remove_clears_rollback_and_allows_clean_reinstall() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let update_archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&update_archive).unwrap();
        let rollback_root = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(ROLLBACK_DIR);
        assert!(rollback_root.join("example.weather").is_dir());

        remove("example.weather").unwrap();

        assert!(!fixture.addon_dir.exists());
        assert!(!rollback_root.join("example.weather").exists());
        let reinstall_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.3.0' };\n";
        let reinstall_archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.3.0",
            reinstall_payload,
            reinstall_payload,
        );
        let reinstalled = install_signed_client_package(&reinstall_archive).unwrap();
        assert_eq!(reinstalled.manifest.version, "0.3.0");
        assert!(!rollback_root.join("example.weather").exists());
    }

    #[test]
    fn failed_signed_client_package_swap_restores_current_directory() {
        let root = std::env::temp_dir().join(format!("screentra-addon-swap-{}", Uuid::new_v4()));
        let destination = root.join("example.weather");
        let backup_root = root.join(ROLLBACK_DIR);
        let backup = backup_root.join("example.weather");
        let staged = root.join("missing-stage");
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&backup_root).unwrap();
        fs::write(destination.join("marker"), b"current").unwrap();

        assert!(replace_addon_directory_atomically(&staged, &destination, &backup).is_err());
        assert_eq!(fs::read(destination.join("marker")).unwrap(), b"current");
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_signed_client_package_rollback_state_is_rejected_without_swap() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let new_payload = b"window.ScreentraAddon = { apiVersion: '1', version: '0.2.0' };\n";
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            new_payload,
            new_payload,
        );
        update_signed_client_package(&archive).unwrap();
        let backup = fixture
            .addon_dir
            .parent()
            .unwrap()
            .join(".rollback/example.weather");
        fs::write(
            backup.join(STATE_FILE),
            serde_json::json!({"enabled": true, "installed_at": "wrong"}).to_string(),
        )
        .unwrap();

        assert!(rollback_signed_client_package("example.weather").is_err());
        assert_eq!(read_record(&fixture.addon_dir).unwrap().manifest.version, "0.2.0");
    }

    #[test]
    fn invalid_signed_client_package_update_keeps_existing_package_intact() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        set_enabled("example.weather", false).unwrap();
        let old_manifest = fs::read(fixture.addon_dir.join(MANIFEST_FILE)).unwrap();
        let old_payload = fs::read(fixture.addon_dir.join("client/index.js")).unwrap();
        let archive = signed_client_package_archive(
            &fixture.root,
            "example.weather",
            "0.2.0",
            b"signed payload",
            b"tampered payload",
        );

        assert!(update_signed_client_package(&archive).is_err());
        assert_eq!(
            fs::read(fixture.addon_dir.join(MANIFEST_FILE)).unwrap(),
            old_manifest
        );
        assert_eq!(
            fs::read(fixture.addon_dir.join("client/index.js")).unwrap(),
            old_payload
        );
        assert!(!list()
            .unwrap()
            .into_iter()
            .find(|record| record.manifest.id == "example.weather")
            .unwrap()
            .enabled);
    }

    #[test]
    fn signed_client_package_update_rejects_identity_mismatch() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let old_manifest = fs::read(fixture.addon_dir.join(MANIFEST_FILE)).unwrap();
        let old_payload = fs::read(fixture.addon_dir.join("client/index.js")).unwrap();
        let archive = signed_client_package_archive(
            &fixture.root,
            "different.extension",
            "0.2.0",
            b"different identity",
            b"different identity",
        );

        assert!(update_signed_client_package(&archive).is_err());
        assert_eq!(
            fs::read(fixture.addon_dir.join(MANIFEST_FILE)).unwrap(),
            old_manifest
        );
        assert_eq!(
            fs::read(fixture.addon_dir.join("client/index.js")).unwrap(),
            old_payload
        );
        assert_eq!(
            list()
                .unwrap()
                .into_iter()
                .filter(|record| record.manifest.id == "different.extension")
                .count(),
            0
        );
    }

    fn assert_client_extension_readbacks_fail_closed() {
        let list_error = list().expect_err("tampered client extension must not be listed");
        let enabled_error = enabled_client_extensions()
            .expect_err("tampered client extension must not be enabled");
        let package_error = client_package("example.weather")
            .expect_err("tampered client extension package must not be returned");
        for error in [list_error, enabled_error, package_error] {
            let message = error.to_string();
            assert!(!message.contains("readback-tamper-marker"));
            assert!(!message.contains("window.ScreentraAddon"));
        }
    }

    #[test]
    fn persisted_client_manifest_tampering_fails_closed_on_all_readbacks() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        let manifest_path = fixture.addon_dir.join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["name"] = serde_json::Value::String("readback-tamper-marker".to_string());
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        assert_client_extension_readbacks_fail_closed();
    }

    #[test]
    fn persisted_client_entrypoint_tampering_fails_closed_on_all_readbacks() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let fixture = installed_signed_client_package_fixture();
        fs::write(
            fixture.addon_dir.join("client/index.js"),
            b"window.ScreentraAddon = 'readback-tamper-marker';\\n",
        )
        .unwrap();

        assert_client_extension_readbacks_fail_closed();
    }

    #[test]
    fn magic_mirror_client_capabilities_are_bounded_and_round_trip() {
        let mut value = client_manifest(&"a".repeat(64));
        value.id = "magic-mirror".to_string();
        value.client_capabilities = vec![
            "render_only".to_string(),
            "camera_capture".to_string(),
            "microphone_capture".to_string(),
            "local_inference".to_string(),
            "client_service".to_string(),
        ];
        value.client_service = Some(ClientServiceSpec {
            id: "speech-to-text".to_string(),
            image: "registry.example/whisper".to_string(),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            socket_name: "speech-to-text".to_string(),
            cpu_millis: 500,
            memory_bytes: 256 * 1024 * 1024,
            pids: 32,
            writable_bytes: 64 * 1024 * 1024,
            model_id: Some("whisper-tiny.en".to_string()),
            model_sha256: Some("c".repeat(64)),
        });

        validate_client_extension_manifest(&value)
            .expect("Magic Mirror capability set should validate");
        let encoded = serde_json::to_value(&value).expect("capabilities should serialize");
        assert_eq!(
            encoded["client_capabilities"],
            serde_json::json!([
                "render_only",
                "camera_capture",
                "microphone_capture",
                "local_inference",
                "client_service"
            ])
        );

        value.client_capabilities.push("camera_capture".to_string());
        assert!(validate_client_extension_manifest(&value).is_err());

        value.client_capabilities.pop();
        value.client_capabilities.push("host_shell".to_string());
        assert!(validate_client_extension_manifest(&value).is_err());
    }

    #[test]
    fn magic_mirror_client_service_contract_requires_signed_bounded_resources() {
        let mut value = client_manifest(&"a".repeat(64));
        value.id = "magic-mirror".to_string();
        value.client_capabilities = vec![
            "microphone_capture".to_string(),
            "local_inference".to_string(),
            "client_service".to_string(),
        ];
        value.client_service = Some(ClientServiceSpec {
            id: "speech-to-text".to_string(),
            image: "registry.example/whisper".to_string(),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            socket_name: "speech-to-text".to_string(),
            cpu_millis: 500,
            memory_bytes: 256 * 1024 * 1024,
            pids: 32,
            writable_bytes: 64 * 1024 * 1024,
            model_id: Some("whisper-tiny.en".to_string()),
            model_sha256: Some("c".repeat(64)),
        });
        validate_client_extension_manifest(&value)
            .expect("bounded local speech service contract should validate");

        value.client_service.as_mut().unwrap().memory_bytes = 1;
        assert!(validate_client_extension_manifest(&value).is_err());
    }

    #[test]
    fn magic_mirror_local_stt_fixture_is_client_only_and_contract_validated() {
        let fixture: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/magic-mirror-stt/manifest.json"
        ))
        .expect("Magic Mirror local STT fixture should be valid JSON");

        validate_client_extension_manifest(&fixture)
            .expect("Magic Mirror local STT client extension should validate");
        assert_eq!(fixture.kind, "client_extension");
        assert_eq!(
            fixture.client_capabilities,
            vec![
                "microphone_capture".to_string(),
                "local_inference".to_string(),
                "client_service".to_string()
            ]
        );
        assert_eq!(
            fixture.client_entrypoint.as_deref(),
            Some("client/index.js")
        );
        assert!(fixture.server_entrypoint.is_none());
        assert_eq!(fixture.client_api_version.as_deref(), Some("1"));
        let service = fixture
            .client_service
            .as_ref()
            .expect("local STT fixture should declare client service metadata");
        assert_eq!(service.socket_name, "magic-mirror-stt");
        assert_eq!(service.model_id.as_deref(), Some("whisper-tiny.en"));
        assert_eq!(service.model_sha256.as_deref().map(str::len), Some(64));

        let mut declarative = fixture.clone();
        declarative.kind = "declarative".to_string();
        assert!(validate_manifest(&declarative).is_err());
    }

    #[test]
    fn local_stt_requires_microphone_capture_capability() {
        let fixture: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/magic-mirror-stt/manifest.json"
        ))
        .expect("Magic Mirror local STT fixture should be valid JSON");
        let mut without_microphone = fixture;
        without_microphone
            .client_capabilities
            .retain(|capability| capability != "microphone_capture");

        assert!(validate_client_extension_manifest(&without_microphone).is_err());
    }

    #[test]
    fn local_stt_rejects_invalid_service_identity() {
        let fixture: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/magic-mirror-stt/manifest.json"
        ))
        .expect("Magic Mirror local STT fixture should be valid JSON");
        let mut invalid = fixture;
        invalid.client_service.as_mut().unwrap().socket_name = "../stt".to_string();

        assert!(validate_client_extension_manifest(&invalid).is_err());
    }

    #[test]
    fn declarative_addon_cannot_request_client_capabilities() {
        let mut value = manifest();
        value.client_capabilities = vec!["camera_capture".to_string()];
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn bundled_standard_addon_manifests_validate() {
        for bytes in [
            include_str!("../addon-packages/weather-widget-standard-us-ca/manifest.json"),
            include_str!("../addon-packages/traffic-standard-us-ca/manifest.json"),
        ] {
            let manifest: AddonManifest = serde_json::from_str(bytes).expect("valid addon JSON");
            validate_manifest(&manifest).expect("bundled standard addon must validate");
            assert_eq!(manifest.kind, "declarative");
            assert!(manifest.data_contract.is_some());
        }
        let camera: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/magic-mirror-camera/manifest.json"
        ))
        .expect("valid camera addon JSON");
        validate_manifest(&camera).expect("bundled camera addon must validate");
        assert_eq!(camera.kind, "declarative");
        assert!(camera.data_contract.is_none());
    }

    #[test]
    fn camera_preset_is_metadata_only_and_capture_off_by_default() {
        let manifest: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/magic-mirror-camera/manifest.json"
        ))
        .expect("camera preset JSON should deserialize");

        validate_manifest(&manifest).expect("camera preset must validate");
        let widget = &manifest.widget_types[0];
        assert_eq!(widget.id, "camera");
        assert_eq!(widget.host_renderer, "camera");
        assert_eq!(widget.default_props["capture_enabled"], false);
        assert_eq!(widget.default_props["facing_mode"], "environment");
        assert_eq!(widget.default_props["filter"], "none");
        assert_eq!(widget.schema["additionalProperties"], false);
        assert_eq!(
            widget.schema["properties"]["facing_mode"]["enum"],
            serde_json::json!(["environment", "user", "left", "right"])
        );
        assert_eq!(
            widget.schema["properties"]["filter"]["enum"],
            serde_json::json!(["none", "grayscale", "sepia", "halloween"])
        );
        assert!(manifest.permissions.is_empty());
        assert!(manifest.client_capabilities.is_empty());
        assert!(manifest.client_entrypoint.is_none());
        assert!(manifest.server_entrypoint.is_none());
        assert!(manifest.trusted_service.is_none());
        assert!(manifest.client_service.is_none());
        assert!(manifest.data_contract.is_none());
    }

    #[test]
    fn bundled_camera_preset_seeds_and_supports_full_inert_lifecycle() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-camera-addon-lifecycle-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }

        ensure_bundled_standard_addons().unwrap();
        let seeded = list().unwrap();
        assert_eq!(seeded.len(), 3);
        let camera = seeded
            .iter()
            .find(|addon| addon.manifest.id == "magic-mirror-camera")
            .expect("camera preset should be first-seeded");
        assert!(camera.enabled);
        assert_eq!(camera.manifest.widget_types[0].host_renderer, "camera");
        assert_eq!(camera.manifest.widget_types[0].default_props["capture_enabled"], false);
        assert!(enabled_widget_registrations()
            .unwrap()
            .iter()
            .any(|registration| registration.addon_id == "magic-mirror-camera"));

        set_enabled("magic-mirror-camera", false).unwrap();
        assert!(!enabled_widget_registrations()
            .unwrap()
            .iter()
            .any(|registration| registration.addon_id == "magic-mirror-camera"));

        let mut replacement: AddonManifest = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "id": "magic-mirror-camera",
            "name": "Magic Mirror camera updated",
            "version": "1.1.0",
            "kind": "declarative",
            "widget_types": [{
                "id": "camera",
                "name": "Camera",
                "host_renderer": "camera",
                "schema": {"type": "object"},
                "default_props": {"capture_enabled": false, "facing_mode": "environment", "filter": "grayscale"}
            }]
        })).unwrap();
        replacement.description = "Updated metadata-only camera preset".to_string();
        let updated = update(replacement).unwrap();
        assert_eq!(updated.manifest.version, "1.1.0");
        assert!(!updated.enabled);
        assert_eq!(updated.manifest.widget_types[0].default_props["capture_enabled"], false);

        set_enabled("magic-mirror-camera", true).unwrap();
        assert!(enabled_widget_registrations()
            .unwrap()
            .iter()
            .any(|registration| registration.addon_id == "magic-mirror-camera"));
        remove("magic-mirror-camera").unwrap();
        assert!(!list()
            .unwrap()
            .iter()
            .any(|addon| addon.manifest.id == "magic-mirror-camera"));
        assert!(!root.join("addons/magic-mirror-camera").exists());

        let after_remove = ensure_bundled_standard_addons().unwrap();
        assert_eq!(after_remove.len(), 2);
        assert!(!after_remove
            .iter()
            .any(|addon| addon.manifest.id == "magic-mirror-camera"));

        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }

    #[test]
    fn standard_weather_data_contract_is_accepted() {
        let mut manifest = manifest();
        manifest.id = "weather-widget-standard-us-ca".to_string();
        manifest.name = "Weather widget standard (US & Canada)".to_string();
        manifest.data_contract = Some(AddonDataContract {
            api_version: "1".to_string(),
            region_scope: vec!["US".to_string(), "CA".to_string()],
            endpoints: vec![AddonDataEndpoint {
                path: "/api/weather/forecast".to_string(),
                method: "GET".to_string(),
            }],
        });
        assert!(validate_manifest(&manifest).is_ok());
    }

    #[test]
    fn data_contract_rejects_mutating_or_unknown_endpoints() {
        let mut manifest = manifest();
        manifest.data_contract = Some(AddonDataContract {
            api_version: "1".to_string(),
            region_scope: vec!["US".to_string()],
            endpoints: vec![AddonDataEndpoint {
                path: "/api/admin/database".to_string(),
                method: "POST".to_string(),
            }],
        });
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(error.contains("data contract"));
    }

    #[test]
    fn accepts_declarative_manifest() {
        assert!(validate_manifest(&manifest()).is_ok());
    }

    #[test]
    fn validates_trusted_service_image_contracts() {
        assert!(validate_trusted_service_image_reference("registry.example/service").is_ok());
        for image in [
            "service",
            "/registry/service",
            "registry//service",
            "registry/service:tag",
            "registry.example/.",
            "registry.example/Weather",
            "registry.example/-service",
            "registry.example/service-",
        ] {
            assert!(validate_trusted_service_image_reference(image).is_err(), "{image}");
        }
        assert!(validate_trusted_service_image_digest(&format!("sha256:{}", "a".repeat(64))).is_ok());
        assert!(validate_trusted_service_image_digest(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(validate_trusted_service_image_digest("sha256:short").is_err());
    }

    #[test]
    fn validates_addon_storage_bounds_and_json() {
        assert!(validate_addon_storage_namespace("example.weather").is_ok());
        assert!(validate_addon_storage_namespace("../escape").is_err());
        assert!(validate_addon_storage_key("settings").is_ok());
        assert!(validate_addon_storage_key("nested/key").is_err());
        assert!(validate_addon_storage_value("{\"enabled\":true}").is_ok());
        assert!(validate_addon_storage_value("not-json").is_err());
        assert!(
            validate_addon_storage_value(&"x".repeat(MAX_ADDON_STORAGE_VALUE_BYTES + 1)).is_err()
        );
    }

    #[test]
    fn accepts_signed_client_manifest_metadata() {
        assert!(validate_client_extension_manifest(&client_manifest(&"a".repeat(64))).is_ok());
    }

    #[test]
    fn rejects_incompatible_client_api_version() {
        let mut value = client_manifest(&"a".repeat(64));
        value.client_api_version = Some("2".to_string());
        assert!(validate_client_extension_manifest(&value).is_err());
    }

    #[test]
    fn rejects_client_entrypoint_traversal() {
        let mut value = client_manifest(&"a".repeat(64));
        value.client_entrypoint = Some("client/../server.js".to_string());
        assert!(validate_client_extension_manifest(&value).is_err());
    }
    #[test]
    fn rejects_unsafe_trusted_service_image_before_runtime_gate() {
        let mut value = manifest();
        value.kind = "trusted_server_service".to_string();
        value.trusted_service = Some(TrustedServerServiceSpec {
            image: "registry.example/air-quality:latest".to_string(),
            image_digest: "sha256:".to_string() + &"a".repeat(64),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "air-quality".to_string(),
            api_namespace: "air-quality".to_string(),
            launch_profile: launch_profile(),
            client_extension_id: Some("air-quality.client".to_string()),
        });
        let error = validate_manifest(&value).unwrap_err().to_string();
        assert!(error.contains("trusted service image"));
    }

    #[test]
    fn rejects_invalid_trusted_service_digest_before_runtime_gate() {
        let mut value = manifest();
        value.kind = "trusted_server_service".to_string();
        value.trusted_service = Some(TrustedServerServiceSpec {
            image: "registry.example/air-quality".to_string(),
            image_digest: "sha256:not-a-digest".to_string(),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "air-quality".to_string(),
            api_namespace: "air-quality".to_string(),
            launch_profile: launch_profile(),
            client_extension_id: Some("air-quality.client".to_string()),
        });
        let error = validate_manifest(&value).unwrap_err().to_string();
        assert!(error.contains("trusted service image digest"));
    }

    #[test]
    fn rejects_unknown_trusted_service_capability_before_runtime_gate() {
        let mut value = manifest();
        value.kind = "trusted_server_service".to_string();
        value.trusted_service = Some(TrustedServerServiceSpec {
            image: "registry.example/air-quality".to_string(),
            image_digest: "sha256:".to_string() + &"a".repeat(64),
            capabilities: vec!["read:secrets".to_string()],
            storage_namespace: "air-quality".to_string(),
            api_namespace: "air-quality".to_string(),
            launch_profile: launch_profile(),
            client_extension_id: Some("air-quality.client".to_string()),
        });
        let error = validate_manifest(&value).unwrap_err().to_string();
        assert!(error.contains("trusted service capability"));
    }

    #[test]
    fn rejects_duplicate_trusted_service_capabilities_before_runtime_gate() {
        let mut value = trusted_manifest();
        value
            .trusted_service
            .as_mut()
            .unwrap()
            .capabilities
            .push("storage".to_string());
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_unqualified_trusted_service_image_before_runtime_gate() {
        let mut value = trusted_manifest();
        value.trusted_service.as_mut().unwrap().image = "air-quality".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    fn trusted_manifest() -> AddonManifest {
        let mut value = manifest();
        value.kind = "trusted_server_service".to_string();
        value.permissions = vec!["storage".to_string()];
        value.trusted_service = Some(TrustedServerServiceSpec {
            image: "registry.example/air-quality".to_string(),
            image_digest: "sha256:".to_string() + &"a".repeat(64),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.air-quality".to_string(),
            api_namespace: "example.air-quality".to_string(),
            launch_profile: launch_profile(),
            client_extension_id: Some("example.air-quality.client".to_string()),
        });
        value.widget_types.clear();
        value
    }

    #[cfg(unix)]
    #[test]
    fn trusted_service_root_symlink_is_rejected_before_creation() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-trusted-root-{}", Uuid::new_v4()));
        let target =
            std::env::temp_dir().join(format!("screentra-trusted-target-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("trusted-services")).unwrap();
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        let result = checked_trusted_service_storage_dir(true);
        assert!(result.is_err());
        assert!(target.read_dir().unwrap().next().is_none());
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(target);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_service_root_insecure_permissions_are_rejected() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("screentra-trusted-permissions-{}", Uuid::new_v4()));
        let storage = root.join("trusted-services");
        fs::create_dir_all(&storage).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&storage, fs::Permissions::from_mode(0o755)).unwrap();
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        assert!(matches!(
            checked_trusted_service_storage_dir(false),
            Err(AddonError::Invalid(message)) if message.contains("permissions")
        ));
        if let Some(value) = old_data_root {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_service_stage_symlink_is_rejected_during_listing() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-stage-root-{}", Uuid::new_v4()));
        let target = std::env::temp_dir().join(format!("screentra-stage-target-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("trusted-services")).unwrap();
        fs::set_permissions(
            root.join("trusted-services"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("trusted-services/.stage-example.weather")).unwrap();
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &root); }
        let result = list_trusted_service_stages();
        assert!(result.is_err());
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(target);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_service_file_validation_rejects_symlinked_ancestors() {
        let root = std::env::temp_dir().join(format!("screentra-trusted-file-path-{}", Uuid::new_v4()));
        let real_dir = root.join("real");
        let linked_dir = root.join("linked");
        fs::create_dir_all(&real_dir).unwrap();
        fs::write(real_dir.join("key.pub"), b"key").unwrap();
        symlink(&real_dir, &linked_dir).unwrap();

        let error = validate_trusted_service_file(&linked_dir.join("key.pub"), 1024, "test key")
            .unwrap_err();
        assert!(error.to_string().contains("symlinked path ancestor"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn trusted_service_stage_rename_never_replaces_existing_target() {
        let root = std::env::temp_dir().join(format!("screentra-noreplace-{}", Uuid::new_v4()));
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("marker"), b"source").unwrap();
        fs::write(target.join("marker"), b"target").unwrap();

        let error = rename_trusted_service_stage_noreplace(&source, &target).unwrap_err();
        assert!(matches!(error, AddonError::AlreadyInstalled));
        assert_eq!(fs::read(target.join("marker")).unwrap(), b"target");
        assert!(source.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn signed_trusted_service_manifest_stages_without_activation() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-trusted-service-stage-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("manifest.json");
        let signature_path = root.join("manifest.sig");
        let image_signature_path = root.join("image.sig");
        let private_key = root.join("trusted-service-signing.key");
        let public_key = root.join("trusted-service-signing.pub");
        let image_private_key = root.join("trusted-service-image-signing.key");
        let image_public_key = root.join("trusted-service-image-signing.pub");
        let image_digest_path = root.join("image-digest.txt");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&trusted_manifest()).unwrap(),
        )
        .unwrap();
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        let image_digest = trusted_manifest()
            .trusted_service
            .as_ref()
            .unwrap()
            .image_digest
            .clone();
        fs::write(&image_digest_path, image_digest.as_bytes()).unwrap();
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&image_private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkey", "-in"])
            .arg(&image_private_key)
            .args(["-pubout", "-out"])
            .arg(&image_public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&image_private_key)
            .args(["-in"])
            .arg(&image_digest_path)
            .args(["-out"])
            .arg(&image_signature_path)
            .status()
            .unwrap()
            .success());
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        let old_public_key = std::env::var_os("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY");
        let old_image_public_key = std::env::var_os("SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root.join("data"));
            std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY", &public_key);
            std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY", &image_public_key);
        }
        let staged = stage_trusted_service_manifest(&manifest_path, &signature_path, &image_signature_path).unwrap();
        assert_eq!(staged.state, "staged");
        assert_eq!(staged.manifest.id, "example.weather");
        let staged_dir = root.join("data/trusted-services/example.weather");
        assert!(staged_dir.join("manifest.json").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(root.join("data/trusted-services"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::symlink_metadata(&staged_dir)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            for filename in ["manifest.json", "manifest.sig", "image.sig", "state.json"] {
                assert_eq!(
                    fs::symlink_metadata(staged_dir.join(filename))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        assert!(staged_dir.join("manifest.sig").is_file());
        assert!(staged_dir.join("image.sig").is_file());
        assert!(!staged_dir.join("active").exists());
        assert!(list().unwrap().is_empty());
        let stages = list_trusted_service_stages().unwrap();
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].manifest.id, "example.weather");
        let mismatched_dir = root.join("data/trusted-services/example.other");
        fs::rename(&staged_dir, &mismatched_dir).unwrap();
        let error = list_trusted_service_stages().unwrap_err();
        assert!(error.to_string().contains("directory identity"));
        let error = get_trusted_service_stage("example.other").unwrap_err();
        assert!(error.to_string().contains("directory identity"));
        let error = remove_trusted_service_stage("example.other").unwrap_err();
        assert!(error.to_string().contains("directory identity"));
        fs::rename(&mismatched_dir, &staged_dir).unwrap();
        let image_signature_bytes = fs::read(staged_dir.join("image.sig")).unwrap();
        let manifest_bytes = fs::read(staged_dir.join("manifest.json")).unwrap();
        fs::write(staged_dir.join("image.sig"), b"tampered").unwrap();
        let error = list_trusted_service_stages().unwrap_err();
        assert!(error.to_string().contains("image signature verification failed"));
        fs::write(staged_dir.join("image.sig"), image_signature_bytes).unwrap();
        fs::write(staged_dir.join("manifest.json"), b"tampered").unwrap();
        let error = list_trusted_service_stages().unwrap_err();
        assert!(error.to_string().contains("signature verification failed"));
        fs::write(staged_dir.join("manifest.json"), manifest_bytes).unwrap();
        remove_trusted_service_stage("example.weather").unwrap();
        remove_trusted_service_stage("example.weather").unwrap();
        assert!(list_trusted_service_stages().unwrap().is_empty());
        assert!(!staged_dir.exists());
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        if let Some(value) = old_public_key {
            unsafe { std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY", value); }
        } else {
            unsafe { std::env::remove_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY"); }
        }
        if let Some(value) = old_image_public_key {
            unsafe { std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY", value); }
        } else {
            unsafe { std::env::remove_var("SCREENTRA_CAST_TRUSTED_SERVICE_IMAGE_SIGNING_PUBKEY"); }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn trusted_service_persisted_files_reject_symlinks_and_oversize() {
        let root = std::env::temp_dir().join(format!(
            "screentra-trusted-service-file-bounds-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("artifact");
        fs::write(&path, b"small").unwrap();
        validate_trusted_service_file(&path, 64, "artifact").unwrap();
        fs::write(&path, vec![0u8; 65]).unwrap();
        assert!(validate_trusted_service_file(&path, 64, "artifact").is_err());

        #[cfg(unix)]
        {
            let target = root.join("target");
            fs::write(&target, b"small").unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(validate_trusted_service_file(&path, 64, "artifact").is_err());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tampered_trusted_service_manifest_is_not_staged() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-trusted-service-tamper-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("manifest.json");
        let signature_path = root.join("manifest.sig");
        let image_signature_path = root.join("image.sig");
        let private_key = root.join("trusted-service-signing.key");
        let public_key = root.join("trusted-service-signing.pub");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&trusted_manifest()).unwrap(),
        )
        .unwrap();
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        fs::write(&manifest_path, b"tampered").unwrap();
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        let old_public_key = std::env::var_os("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root.join("data"));
            std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY", &public_key);
        }
        let error = stage_trusted_service_manifest(&manifest_path, &signature_path, &image_signature_path).unwrap_err();
        assert!(error.to_string().contains("signature verification failed"));
        assert!(!root.join("data/trusted-services").exists());
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        if let Some(value) = old_public_key {
            unsafe { std::env::set_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY", value); }
        } else {
            unsafe { std::env::remove_var("SCREENTRA_CAST_TRUSTED_SERVICE_SIGNING_PUBKEY"); }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_trusted_service_until_container_runtime_exists() {
        let mut value = manifest();
        value.kind = "trusted_server_service".to_string();
        value.trusted_service = Some(TrustedServerServiceSpec {
            image: "registry.example/air-quality".to_string(),
            image_digest: "sha256:".to_string() + &"a".repeat(64),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "air-quality".to_string(),
            api_namespace: "air-quality".to_string(),
            launch_profile: launch_profile(),
            client_extension_id: Some("air-quality.client".to_string()),
        });
        let error = validate_manifest(&value).unwrap_err().to_string();
        assert!(error.contains("container execution is not enabled"));
    }
    #[test]
    fn rejects_executable_manifest() {
        let mut value = manifest();
        value.kind = "client_widget".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_unknown_renderer() {
        let mut value = manifest();
        value.widget_types[0].host_renderer = "arbitrary-code".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_path_traversal_id() {
        let mut value = manifest();
        value.id = "../escape".to_string();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn rejects_invalid_client_status_without_writing() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-invalid-status-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }
        let result = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: None,
                addons: vec![ClientAddonStatus {
                    id: "../escape".to_string(),
                    version: None,
                    state: "active".to_string(),
                    entrypoint: None,
                    error: None,
                }],
                capabilities: Vec::new(),
                services: Vec::new(),
                error: None,
            },
        );
        assert!(result.is_err());
        for timestamp in [
            "2026-10-19T12:00:00+00:00",
            "2026-10-19T12:00:00.123Z",
        ] {
            let invalid_timestamp = save_client_status(
                "client-1",
                ClientAddonStatusReport {
                    at: Some(timestamp.to_string()),
                    addons: Vec::new(),
                    capabilities: Vec::new(),
                    services: Vec::new(),
                    error: None,
                },
            );
            assert!(invalid_timestamp.is_err(), "timestamp should be rejected: {timestamp}");
        }
        let duplicate_addons = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: vec![
                    ClientAddonStatus {
                        id: "weather".to_string(),
                        version: Some("1.0.0".to_string()),
                        state: "verified".to_string(),
                        entrypoint: None,
                        error: None,
                    },
                    ClientAddonStatus {
                        id: "weather".to_string(),
                        version: Some("1.0.0".to_string()),
                        state: "verified_cached".to_string(),
                        entrypoint: None,
                        error: None,
                    },
                ],
                capabilities: Vec::new(),
                services: Vec::new(),
                error: None,
            },
        );
        assert!(duplicate_addons.is_err());
        assert!(!data_root.join("addon-client-status").exists());
        if let Some(value) = old_data_root {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn rejects_unsafe_client_status_details_without_writing() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-unsafe-status-detail-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }
        for detail in [
            "password=redacted",
            "credential-token",
            "https://example.invalid",
            "/dev/video0",
            "line\nstatus",
        ] {
            let result = save_client_status(
                "client-1",
                ClientAddonStatusReport {
                    at: Some("2026-10-19T12:00:00Z".to_string()),
                    addons: Vec::new(),
                    capabilities: vec![ClientCapabilityStatus {
                        id: "render_only".to_string(),
                        state: "available".to_string(),
                        detail: Some(detail.to_string()),
                    }],
                    services: Vec::new(),
                    error: None,
                },
            );
            assert!(result.is_err(), "unsafe detail should be rejected: {detail:?}");
        }
        let oversized_detail = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: Vec::new(),
                capabilities: vec![ClientCapabilityStatus {
                    id: "render_only".to_string(),
                    state: "available".to_string(),
                    detail: Some("d".repeat(161)),
                }],
                services: Vec::new(),
                error: None,
            },
        );
        assert!(oversized_detail.is_err());
        let oversized_error = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: Vec::new(),
                capabilities: Vec::new(),
                services: Vec::new(),
                error: Some("e".repeat(97)),
            },
        );
        assert!(oversized_error.is_err());
        for entrypoint in [
            "client/".to_string(),
            "client/widget\\\\js".to_string(),
            format!("client/{}", "x".repeat(154)),
            "client/\u{1}".to_string(),
        ] {
            let invalid_entrypoint = save_client_status(
                "client-1",
                ClientAddonStatusReport {
                    at: Some("2026-10-19T12:00:00Z".to_string()),
                    addons: vec![ClientAddonStatus {
                        id: "weather".to_string(),
                        version: Some("1.0.0".to_string()),
                        state: "verified".to_string(),
                        entrypoint: Some(entrypoint.clone()),
                        error: None,
                    }],
                    capabilities: Vec::new(),
                    services: Vec::new(),
                    error: None,
                },
            );
            assert!(invalid_entrypoint.is_err(), "entrypoint should be rejected: {entrypoint:?}");
        }
        let empty_detail = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: Vec::new(),
                capabilities: vec![ClientCapabilityStatus {
                    id: "render_only".to_string(),
                    state: "available".to_string(),
                    detail: Some(String::new()),
                }],
                services: Vec::new(),
                error: None,
            },
        );
        assert!(empty_detail.is_err());
        let empty_error = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: Vec::new(),
                capabilities: Vec::new(),
                services: Vec::new(),
                error: Some(String::new()),
            },
        );
        assert!(empty_error.is_err());
        let error_state = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-19T12:00:00Z".to_string()),
                addons: Vec::new(),
                capabilities: vec![ClientCapabilityStatus {
                    id: "render_only".to_string(),
                    state: "error".to_string(),
                    detail: None,
                }],
                services: Vec::new(),
                error: None,
            },
        );
        assert!(error_state.is_err());
        let missing_timestamp = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: None,
                addons: Vec::new(),
                capabilities: Vec::new(),
                services: Vec::new(),
                error: None,
            },
        );
        assert!(missing_timestamp.is_err());
        let invalid_timestamp = save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("not-a-rfc3339-timestamp".to_string()),
                addons: Vec::new(),
                capabilities: Vec::new(),
                services: Vec::new(),
                error: None,
            },
        );
        assert!(invalid_timestamp.is_err(), "invalid timestamp should be rejected");
        assert!(!data_root.join("addon-client-status").exists());
        if let Some(value) = old_data_root {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn client_capability_status_round_trip_is_bounded() {
        let report = ClientAddonStatusReport {
            at: Some("2026-10-18T12:00:00Z".to_string()),
            addons: Vec::new(),
            capabilities: vec![ClientCapabilityStatus {
                id: "camera_capture".to_string(),
                state: "available".to_string(),
                detail: None,
            }],
            services: vec![ClientServiceStatus {
                id: "speech-to-text".to_string(),
                version: "1.0.0".to_string(),
                state: "staged".to_string(),
                detail: None,
                restart_count: 0,
                model_id: None,
                model_sha256: None,
            }],
            error: None,
        };
        let encoded = serde_json::to_value(&report).expect("capability status serializes");
        assert_eq!(encoded["capabilities"][0]["id"], "camera_capture");
        assert_eq!(
            serde_json::from_value::<ClientAddonStatusReport>(encoded)
                .expect("capability status deserializes")
                .capabilities[0]
                .state,
            "available"
        );
    }

    #[test]
    fn client_service_model_identity_round_trip_preserves_optional_metadata() {
        let report: ClientAddonStatusReport = serde_json::from_value(serde_json::json!({
            "at": "2026-10-19T12:00:00Z",
            "addons": [],
            "capabilities": [],
            "services": [{
                "id": "speech-to-text",
                "version": "1.0.0",
                "state": "running",
                "model_id": "whisper-tiny.en",
                "model_sha256": "a".repeat(64)
            }]
        }))
        .expect("model identity status should deserialize");
        let encoded = serde_json::to_value(&report).expect("model identity status serializes");
        assert_eq!(encoded["services"][0]["model_id"], "whisper-tiny.en");
        assert_eq!(encoded["services"][0]["model_sha256"], "a".repeat(64));

        let legacy: ClientAddonStatusReport = serde_json::from_value(serde_json::json!({
            "at": "2026-10-19T12:00:00Z",
            "addons": [],
            "capabilities": [],
            "services": [{
                "id": "speech-to-text",
                "version": "1.0.0",
                "state": "running"
            }]
        }))
        .expect("legacy service status should remain readable");
        let legacy_encoded = serde_json::to_value(legacy).expect("legacy status serializes");
        assert!(legacy_encoded["services"][0]["model_id"].is_null());
        assert!(legacy_encoded["services"][0]["model_sha256"].is_null());
    }

    #[test]
    fn rejects_malformed_client_service_model_identity_without_echoing_values() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-model-status-validation-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }

        let cases = [
            (serde_json::json!({"model_id": "whisper-tiny.en"}), "whisper-tiny.en"),
            (serde_json::json!({"model_sha256": "a".repeat(64)}), &"a".repeat(64)),
            (serde_json::json!({
                "model_id": "whisper-tiny.en",
                "model_sha256": "A".repeat(64)
            }), &"A".repeat(64)),
            (serde_json::json!({
                "model_id": "whisper-tiny.en",
                "model_sha256": "a".repeat(63)
            }), &"a".repeat(63)),
            (serde_json::json!({
                "model_id": "whisper-tiny.en",
                "model_sha256": "a".repeat(65)
            }), &"a".repeat(65)),
            (serde_json::json!({
                "model_id": "Whisper-tiny.en",
                "model_sha256": "a".repeat(64)
            }), "Whisper-tiny.en"),
            (serde_json::json!({
                "model_id": "m".repeat(65),
                "model_sha256": "a".repeat(64)
            }), &"m".repeat(65)),
        ];
        for (identity, secret_value) in cases {
            let mut service = serde_json::json!({
                "id": "speech-to-text",
                "version": "1.0.0",
                "state": "running"
            });
            for (key, value) in identity.as_object().unwrap() {
                service[key] = value.clone();
            }
            let report: ClientAddonStatusReport = serde_json::from_value(serde_json::json!({
                "at": "2026-10-19T12:00:00Z",
                "addons": [],
                "capabilities": [],
                "services": [service]
            }))
            .expect("model identity test status should deserialize");
            let error = save_client_status("client-1", report).expect_err("malformed identity");
            assert!(!error.to_string().contains(secret_value));
        }

        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn client_service_model_identity_persists_through_status_storage() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-model-status-round-trip-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe { std::env::set_var("SIGNAGE_DATA_DIR", &data_root); }
        let report: ClientAddonStatusReport = serde_json::from_value(serde_json::json!({
            "at": "2026-10-19T12:00:00Z",
            "addons": [],
            "capabilities": [],
            "services": [{
                "id": "speech-to-text",
                "version": "1.0.0",
                "state": "running",
                "model_id": "whisper-tiny.en",
                "model_sha256": "b".repeat(64)
            }]
        }))
        .expect("model identity report should deserialize");
        save_client_status("client-1", report).expect("model identity report should persist");
        let loaded = load_client_status("client-1")
            .expect("model identity report should load")
            .expect("model identity report should exist");
        assert_eq!(loaded["report"]["services"][0]["model_id"], "whisper-tiny.en");
        assert_eq!(loaded["report"]["services"][0]["model_sha256"], "b".repeat(64));

        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saves_bounded_client_status_snapshot() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-status-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
        }
        save_client_status(
            "client-1",
            ClientAddonStatusReport {
                at: Some("2026-10-15T12:00:00Z".to_string()),
                addons: vec![ClientAddonStatus {
                    id: "example.weather".to_string(),
                    version: Some("1.0.0".to_string()),
                    state: "verified_cached".to_string(),
                    entrypoint: Some("client/index.js".to_string()),
                    error: None,
                }],
                capabilities: vec![ClientCapabilityStatus {
                    id: "render_only".to_string(),
                    state: "available".to_string(),
                    detail: None,
                }],
                services: vec![ClientServiceStatus {
                    id: "speech-to-text".to_string(),
                    version: "1.0.0".to_string(),
                    state: "running".to_string(),
                    detail: Some("local-only".to_string()),
                    restart_count: 1,
                    model_id: None,
                    model_sha256: None,
                }],
                error: None,
            },
        )
        .unwrap();
        let status_dir = data_root.join("addon-client-status");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::symlink_metadata(&status_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
        let entries = fs::read_dir(&status_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(entries[0].path()).unwrap()).unwrap();
        assert_eq!(saved["client_id"], "client-1");
        let loaded = load_client_status("client-1").unwrap().unwrap();
        assert_eq!(loaded["report"]["addons"][0]["state"], "verified_cached");
        assert_eq!(loaded["report"]["capabilities"][0]["id"], "render_only");
        assert_eq!(loaded["report"]["services"][0]["state"], "running");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::symlink_metadata(&status_dir).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&status_dir, permissions).unwrap();
            assert!(load_client_status("client-1").is_err());
            let mut permissions = fs::symlink_metadata(&status_dir).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&status_dir, permissions).unwrap();
        }
        let invalid_report: ClientAddonStatusReport = serde_json::from_value(serde_json::json!({
            "at": "2026-10-19T12:01:00Z",
            "addons": [
                {"id": "weather", "state": "verified"},
                {"id": "weather", "state": "failed"}
            ],
            "capabilities": [],
            "services": []
        }))
        .unwrap();
        assert!(save_client_status("client-1", invalid_report).is_err());
        let retained = load_client_status("client-1").unwrap().unwrap();
        assert_eq!(retained["report"]["addons"][0]["state"], "verified_cached");
        let raw_saved = fs::read_to_string(entries[0].path()).unwrap();
        let duplicated_client_id = raw_saved.replacen(
            "  \"client_id\": \"client-1\",\n",
            "  \"client_id\": \"client-1\",\n  \"client_id\": \"client-1\",\n",
            1,
        );
        assert_ne!(duplicated_client_id, raw_saved);
        fs::write(entries[0].path(), duplicated_client_id).unwrap();
        assert!(load_client_status("client-1").is_err());
        let duplicated_received_at = raw_saved.replacen(
            "  \"received_at\": ",
            "  \"received_at\": \"2026-10-19T12:00:00Z\",\n  \"received_at\": ",
            1,
        );
        assert_ne!(duplicated_received_at, raw_saved);
        fs::write(entries[0].path(), duplicated_received_at).unwrap();
        assert!(load_client_status("client-1").is_err());
        let mut tampered = saved.clone();
        tampered["report"]["capabilities"][0]["detail"] =
            serde_json::Value::String("password=redacted".to_string());
        fs::write(entries[0].path(), serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert!(load_client_status("client-1").is_err());
        let mut unknown_field = saved.clone();
        unknown_field["report"]["unexpected"] = serde_json::Value::Bool(true);
        fs::write(entries[0].path(), serde_json::to_vec(&unknown_field).unwrap()).unwrap();
        assert!(load_client_status("client-1").is_err());
        let mut invalid_timestamp = saved.clone();
        invalid_timestamp["received_at"] =
            serde_json::Value::String("not-a-rfc3339-timestamp".to_string());
        fs::write(
            entries[0].path(),
            serde_json::to_vec(&invalid_timestamp).unwrap(),
        )
        .unwrap();
        assert!(load_client_status("client-1").is_err());
        let mut duplicate_addons = saved.clone();
        duplicate_addons["report"]["addons"] = serde_json::json!([
            {"id": "weather", "state": "verified"},
            {"id": "weather", "state": "failed"}
        ]);
        fs::write(
            entries[0].path(),
            serde_json::to_vec(&duplicate_addons).unwrap(),
        )
        .unwrap();
        assert!(load_client_status("client-1").is_err());
        let mut wrong_identity = saved;
        wrong_identity["client_id"] = serde_json::Value::String("client-2".to_string());
        fs::write(entries[0].path(), serde_json::to_vec(&wrong_identity).unwrap()).unwrap();
        assert!(load_client_status("client-1").is_err());
        if let Some(value) = old_data_root {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn signed_client_package_round_trip_verifies_and_persists() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-package-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let data_root = root.join("data");
        let package_root = root.join("package-root");
        let client_root = package_root.join("client");
        fs::create_dir_all(&client_root).unwrap();
        let client_bytes = b"window.ScreentraAddon = { apiVersion: '1' };\n";
        let client_path = client_root.join("index.js");
        fs::write(&client_path, client_bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(client_bytes));
        let manifest = client_manifest(&digest);
        let manifest_path = package_root.join(MANIFEST_FILE);
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let signature_path = package_root.join("manifest.sig");
        let private_key = root.join("addon-signing.key");
        let public_key = root.join("addon-signing.pub");
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new(trusted_service_signature_verifier_executable())
            .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&manifest_path)
            .args(["-out"])
            .arg(&signature_path)
            .status()
            .unwrap()
            .success());
        let archive = root.join("addon.tar.gz");
        let archive_file = fs::File::create(&archive).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder
            .append_path_with_name(&manifest_path, MANIFEST_FILE)
            .unwrap();
        builder
            .append_path_with_name(&signature_path, "manifest.sig")
            .unwrap();
        builder.append_dir_all("client", &client_root).unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        let old_public_key = std::env::var_os("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_root);
            std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", &public_key);
        }
        let record = install_signed_client_package(&archive).unwrap();
        assert_eq!(record.manifest.kind, "client_extension");
        let repackaged = client_package("example.weather").unwrap();
        let repackaged_path = root.join("repackaged.tar.gz");
        fs::write(&repackaged_path, repackaged).unwrap();
        assert_eq!(
            crate::safe_archive::inspect_tar_gz(&repackaged_path).unwrap(),
            vec![
                "client".to_string(),
                "manifest.json".to_string(),
                "manifest.sig".to_string()
            ]
        );
        let extensions = enabled_client_extensions().unwrap();
        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].id, "example.weather");
        assert_eq!(
            fs::read(data_root.join("addons/example.weather/client/index.js")).unwrap(),
            client_bytes
        );
        let _ = fs::remove_dir_all(&root);
        unsafe {
            match old_data_root {
                Some(value) => std::env::set_var("SIGNAGE_DATA_DIR", value),
                None => std::env::remove_var("SIGNAGE_DATA_DIR"),
            }
            match old_public_key {
                Some(value) => std::env::set_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY", value),
                None => std::env::remove_var("SCREENTRA_CAST_ADDON_SIGNING_PUBKEY"),
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn remove_rejects_symlinked_addon_directory() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-addon-remove-{}", Uuid::new_v4()));
        let addons_root = root.join("addons");
        let target = root.join("target");
        fs::create_dir_all(&addons_root).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(
            target.join(MANIFEST_FILE),
            serde_json::to_vec(&manifest()).unwrap(),
        )
        .unwrap();
        fs::write(
            target.join(STATE_FILE),
            serde_json::to_vec(&AddonState {
                enabled: true,
                installed_at: "now".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        let addon_path = addons_root.join("example.weather");
        symlink(&target, &addon_path).unwrap();
        let old_data_root = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        let error = set_enabled("example.weather", false).unwrap_err();
        assert!(error.to_string().contains("Invalid addon directory"));
        let error = remove("example.weather").unwrap_err();
        assert!(error.to_string().contains("Invalid addon directory"));
        assert!(target.is_dir());
        assert!(addon_path.is_symlink());
        let data_target = root.join("data-target");
        let data_link = root.join("data-link");
        fs::create_dir_all(&data_target).unwrap();
        symlink(&data_target, &data_link).unwrap();
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_link);
        }
        assert!(list().is_err());
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", data_link.join("nested"));
        }
        assert!(list().is_err());
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        if let Some(value) = old_data_root {
            unsafe { std::env::set_var("SIGNAGE_DATA_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_DATA_DIR"); }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn updating_declarative_addon_replaces_manifest_and_preserves_enable_state() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-addon-update-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        install(manifest()).unwrap();
        set_enabled("example.weather", false).unwrap();
        let mut replacement = manifest();
        replacement.version = "0.2.0".to_string();
        replacement.name = "Updated Example Weather".to_string();

        let updated = update(replacement).unwrap();
        assert_eq!(updated.manifest.version, "0.2.0");
        assert_eq!(updated.manifest.name, "Updated Example Weather");
        assert!(!updated.enabled);
        assert_eq!(list().unwrap().len(), 1);

        remove("example.weather").unwrap();
        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }

    #[test]
    fn standard_addon_lifecycle_supports_update_toggle_and_remove() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-standard-addon-lifecycle-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }

        ensure_bundled_standard_addons().unwrap();
        set_enabled("weather-widget-standard-us-ca", false).unwrap();
        let mut replacement: AddonManifest = serde_json::from_str(include_str!(
            "../addon-packages/weather-widget-standard-us-ca/manifest.json"
        ))
        .unwrap();
        replacement.version = "1.1.0".to_string();
        replacement.name = "Weather widget standard (US & Canada) Updated".to_string();
        let updated = update(replacement).unwrap();
        assert_eq!(updated.manifest.version, "1.1.0");
        assert!(!updated.enabled);
        assert!(!enabled_widget_registrations()
            .unwrap()
            .iter()
            .any(|registration| registration.addon_id == "weather-widget-standard-us-ca"));

        set_enabled("weather-widget-standard-us-ca", true).unwrap();
        assert!(enabled_widget_registrations()
            .unwrap()
            .iter()
            .any(|registration| registration.addon_id == "weather-widget-standard-us-ca"));
        remove("weather-widget-standard-us-ca").unwrap();
        assert!(!list()
            .unwrap()
            .iter()
            .any(|addon| addon.manifest.id == "weather-widget-standard-us-ca"));

        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }

    #[test]
    fn bundled_standard_addons_install_once_and_are_listed() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-standard-addon-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }

        let first = ensure_bundled_standard_addons().unwrap();
        assert_eq!(first.len(), 3);
        assert!(first.iter().all(|addon| addon.enabled));
        let second = ensure_bundled_standard_addons().unwrap();
        assert_eq!(second.len(), 3);
        assert_eq!(list().unwrap().len(), 3);
        assert!(list()
            .unwrap()
            .iter()
            .any(|addon| addon.manifest.id == "weather-widget-standard-us-ca"));
        assert!(list()
            .unwrap()
            .iter()
            .any(|addon| addon.manifest.id == "traffic-standard-us-ca"));
        assert!(list()
            .unwrap()
            .iter()
            .any(|addon| addon.manifest.id == "magic-mirror-camera"));
        let registrations = enabled_widget_registrations().unwrap();
        assert_eq!(registrations.len(), 3);
        assert!(registrations
            .iter()
            .any(|registration| registration.id == "weather-standard"));
        assert!(registrations
            .iter()
            .any(|registration| registration.id == "traffic-standard"));
        assert!(registrations
            .iter()
            .any(|registration| registration.id == "camera"));

        let weather_dir = root.join("addons/weather-widget-standard-us-ca");
        fs::remove_dir_all(weather_dir).unwrap();
        let after_remove = ensure_bundled_standard_addons().unwrap();
        assert_eq!(after_remove.len(), 2);
        assert!(after_remove
            .iter()
            .all(|addon| addon.manifest.id != "weather-widget-standard-us-ca"));

        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }

    #[test]
    fn startup_migrates_legacy_standard_renderer_fields() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-cast-standard-addon-migration-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }

        ensure_bundled_standard_addons().unwrap();
        set_enabled("weather-widget-standard-us-ca", false).unwrap();
        let manifest_path = root.join("addons/weather-widget-standard-us-ca/manifest.json");
        let canonical = fs::read_to_string(&manifest_path).unwrap();
        let legacy = canonical.replace("\"host_renderer\"", "\"renderer_type\"");
        assert_ne!(legacy, canonical);
        fs::write(&manifest_path, legacy).unwrap();

        let migrated = ensure_bundled_standard_addons().unwrap();
        let stored = fs::read_to_string(&manifest_path).unwrap();
        assert!(stored.contains("\"host_renderer\""));
        assert!(!stored.contains("\"renderer_type\""));
        let weather = migrated
            .iter()
            .find(|addon| addon.manifest.id == "weather-widget-standard-us-ca")
            .unwrap();
        assert!(!weather.enabled);
        assert_eq!(weather.manifest.widget_types[0].host_renderer, "weather");

        let _ = fs::remove_dir_all(&root);
        unsafe {
            std::env::remove_var("SIGNAGE_DATA_DIR");
        }
    }

    #[test]
    fn storage_round_trip_preserves_enable_state() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
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
        let registrations = enabled_widget_registrations().unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].host_renderer, "weather");
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
