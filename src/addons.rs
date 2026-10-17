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
const SUPPORTED_CLIENT_API_VERSION: &str = "1";
const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 80;
const MAX_DESCRIPTION_LEN: usize = 500;
const DECLARATIVE_RENDERERS: &[&str] = &[
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
    "news",
    "rectangle",
];

pub const MAX_ADDON_STORAGE_KEY_BYTES: usize = 128;
pub const MAX_ADDON_STORAGE_VALUE_BYTES: usize = 64 * 1024;

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
    #[serde(default)]
    pub trusted_service: Option<TrustedServerServiceSpec>,
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
    #[serde(default)]
    pub renderer_type: String,
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
pub struct ClientAddonStatusReport {
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub addons: Vec<ClientAddonStatus>,
    #[serde(default)]
    pub error: Option<String>,
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
        Ok(_) => Ok(Some(dir)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(&dir).map_err(io_error)?;
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

pub fn save_client_status(
    client_id: &str,
    report: ClientAddonStatusReport,
) -> Result<(), AddonError> {
    let path = client_status_path(client_id)?;
    if report.at.as_deref().map_or(false, |value| value.len() > 64) {
        return Err(AddonError::Invalid(
            "Addon status timestamp is too long".to_string(),
        ));
    }
    if report.addons.len() > 64 {
        return Err(AddonError::Invalid(
            "Addon status contains too many entries".to_string(),
        ));
    }
    for addon in &report.addons {
        validate_id(&addon.id)?;
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
            validate_relative_client_path(entrypoint)?;
        }
        if addon
            .error
            .as_deref()
            .map_or(false, |value| value.len() > 256)
        {
            return Err(AddonError::Invalid(
                "Addon status error is too long".to_string(),
            ));
        }
    }
    if report
        .error
        .as_deref()
        .map_or(false, |value| value.len() > 256)
    {
        return Err(AddonError::Invalid(
            "Addon status error is too long".to_string(),
        ));
    }
    let payload = serde_json::json!({
        "client_id": client_id,
        "received_at": chrono::Utc::now().to_rfc3339(),
        "report": report,
    });
    let bytes = serde_json::to_vec_pretty(&payload).map_err(io_error)?;
    if bytes.len() > 256 * 1024 {
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
    if bytes.len() > 256 * 1024 {
        return Err(AddonError::Invalid(
            "Stored addon status is too large".to_string(),
        ));
    }
    serde_json::from_slice(&bytes)
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
        if !DECLARATIVE_RENDERERS.contains(&widget.renderer_type.as_str()) {
            return Err(AddonError::Invalid(format!(
                "Widget type '{}' must select a supported renderer_type",
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
    pub renderer_type: String,
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

pub fn enabled_widget_registrations() -> Result<Vec<AddonWidgetRegistration>, AddonError> {
    let mut registrations = Vec::new();
    for addon in list()?.into_iter().filter(|addon| addon.enabled) {
        for widget in addon.manifest.widget_types {
            if widget.renderer_type.is_empty() {
                continue;
            }
            registrations.push(AddonWidgetRegistration {
                id: widget.id,
                addon_id: addon.manifest.id.clone(),
                addon_version: addon.manifest.version.clone(),
                name: widget.name,
                description: widget.description,
                renderer_type: widget.renderer_type,
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
        if !widget.renderer_type.is_empty()
            && !DECLARATIVE_RENDERERS.contains(&widget.renderer_type.as_str())
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
    let manifest_text = fs::read_to_string(manifest_path).map_err(io_error)?;
    let manifest: AddonManifest = serde_json::from_str(&manifest_text).map_err(io_error)?;
    if manifest.kind == "client_extension" {
        validate_client_extension_manifest(&manifest)?;
    } else {
        validate_manifest(&manifest)?;
    }
    let state_text = fs::read_to_string(dir.join(STATE_FILE)).map_err(io_error)?;
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
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            AddonError::Invalid("Invalid trusted service storage directory".to_string()),
        ),
        Ok(_) => Ok(Some(root)),
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

fn verify_trusted_service_manifest_signature(
    manifest_path: &Path,
    signature_path: &Path,
) -> Result<(), AddonError> {
    let public_key = trusted_service_signing_public_key();
    validate_trusted_service_file(&public_key, 64 * 1024, "manifest signing public key")?;
    let result = Command::new("openssl")
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
        let output = Command::new("openssl")
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
    fs::create_dir(path).map_err(io_error)?;
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        let _ = fs::remove_dir(path);
        return Err(AddonError::Invalid(
            "Trusted service stage is not a real directory".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        if let Err(error) = fs::set_permissions(path, permissions) {
            let _ = fs::remove_dir(path);
            return Err(io_error(error));
        }
    }
    Ok(())
}

fn copy_private_trusted_service_file(source: &Path, target: &Path) -> Result<(), AddonError> {
    let mut input = fs::File::open(source).map_err(io_error)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(io_error)?;
    if let Err(error) = io::copy(&mut input, &mut output) {
        let _ = fs::remove_file(target);
        return Err(io_error(error));
    }
    output.flush().map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(target).map_err(io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            let _ = fs::remove_file(target);
            return Err(AddonError::Invalid(
                "Trusted service stage file is not regular".to_string(),
            ));
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        if let Err(error) = fs::set_permissions(target, permissions) {
            let _ = fs::remove_file(target);
            return Err(io_error(error));
        }
    }
    Ok(())
}

fn write_private_trusted_service_file(path: &Path, bytes: &[u8]) -> Result<(), AddonError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.flush()) {
        let _ = fs::remove_file(path);
        return Err(io_error(error));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path).map_err(io_error)?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        if let Err(error) = fs::set_permissions(path, permissions) {
            let _ = fs::remove_file(path);
            return Err(io_error(error));
        }
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
    let result = Command::new("openssl")
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

/// Validate and persist a signed client-extension package. The package is
/// never executed by the server; the client-loader phase will consume the
/// stored manifest and client directory after its own signature verification.
pub fn install_signed_client_package(archive: &Path) -> Result<AddonRecord, AddonError> {
    let root = checked_addon_storage_dir(true)?
        .ok_or_else(|| AddonError::Io("Addon storage directory is unavailable".to_string()))?;
    let stage = root.join(format!(".package-install-{}", Uuid::new_v4()));
    if let Err(error) = crate::safe_archive::extract_tar_gz(archive, &stage, 0) {
        let _ = fs::remove_dir_all(&stage);
        return Err(AddonError::Invalid(error));
    }
    let result = (|| {
        let manifest_path = stage.join(MANIFEST_FILE);
        let signature_path = stage.join("manifest.sig");
        if !manifest_path.is_file() || !signature_path.is_file() {
            return Err(AddonError::Invalid(
                "Addon package must contain manifest.json and manifest.sig at its root".to_string(),
            ));
        }
        verify_manifest_signature(&manifest_path, &signature_path)?;
        let manifest_bytes = fs::read(&manifest_path).map_err(io_error)?;
        let manifest: AddonManifest = serde_json::from_slice(&manifest_bytes).map_err(io_error)?;
        validate_client_extension_manifest(&manifest)?;
        let entrypoint = manifest.client_entrypoint.as_deref().ok_or_else(|| {
            AddonError::Invalid("Client package entrypoint is missing".to_string())
        })?;
        let client_file = stage.join(entrypoint);
        if !client_file.is_file() {
            return Err(AddonError::Invalid(format!(
                "Client package entrypoint is missing: {entrypoint}"
            )));
        }
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
        let destination = addon_dir(&manifest.id)?;
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
        fs::write(stage.join(STATE_FILE), state_text).map_err(io_error)?;
        fs::rename(&stage, &destination).map_err(io_error)?;
        Ok(AddonRecord {
            manifest,
            enabled: true,
            installed_at,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Mutex, OnceLock};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

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
                renderer_type: "weather".to_string(),
                icon: "🌤️".to_string(),
                default_props: serde_json::json!({"units": "metric"}),
            }],
            permissions: Vec::new(),
            client_entrypoint: None,
            server_entrypoint: None,
            trusted_service: None,
            client_sha256: None,
            client_api_version: None,
        }
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
        value.widget_types[0].renderer_type.clear();
        value
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
        let _guard = STORAGE_TEST_LOCK
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
    fn trusted_service_stage_symlink_is_rejected_during_listing() {
        let _guard = STORAGE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!("screentra-stage-root-{}", Uuid::new_v4()));
        let target = std::env::temp_dir().join(format!("screentra-stage-target-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("trusted-services")).unwrap();
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
        let _guard = STORAGE_TEST_LOCK
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
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
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
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&image_private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&image_private_key)
            .args(["-pubout", "-out"])
            .arg(&image_public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
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
        let _guard = STORAGE_TEST_LOCK
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
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
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
        value.widget_types[0].renderer_type = "arbitrary-code".to_string();
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
        let _guard = STORAGE_TEST_LOCK
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
                error: None,
            },
        );
        assert!(result.is_err());
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
    fn saves_bounded_client_status_snapshot() {
        let _guard = STORAGE_TEST_LOCK
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
                error: None,
            },
        )
        .unwrap();
        let status_dir = data_root.join("addon-client-status");
        let entries = fs::read_dir(status_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(entries[0].path()).unwrap()).unwrap();
        assert_eq!(saved["client_id"], "client-1");
        let loaded = load_client_status("client-1").unwrap().unwrap();
        assert_eq!(loaded["report"]["addons"][0]["state"], "verified_cached");
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
        let _guard = STORAGE_TEST_LOCK
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
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "ED25519", "-out"])
            .arg(&private_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&private_key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
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
        let registrations = enabled_widget_registrations().unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].renderer_type, "weather");
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
