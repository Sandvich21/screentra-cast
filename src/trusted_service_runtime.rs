//! Fail-closed runtime records for trusted server services.
//!
//! This module is deliberately a supervisor boundary, not a container runtime.
//! It records explicit administrative intent and the checks required before a
//! service could ever execute. No function in this module starts a process,
//! pulls an image, creates a credential, opens a port, or connects to a DB.

use crate::addons::{self, AddonError, TrustedServiceStage};
use chrono::Utc;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const RUNTIME_DIR: &str = "trusted-service-runtime";
const RUNTIME_FILE: &str = "runtime.json";
const RUNTIME_RECORD_VERSION: u32 = 1;
const MAX_RUNTIME_RECORD_BYTES: u64 = 128 * 1024;
const MAX_RUNTIME_RECORDS: usize = 1_024;

/// This is a compile-time safety marker for the first runtime phase.
/// It must remain false until a separately reviewed execution backend exists.
pub const EXECUTION_ENABLED: bool = false;

#[derive(Debug)]
pub enum RuntimeError {
    Invalid(String),
    NotFound,
    Conflict(String),
    Blocked(String),
    Io(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::NotFound => write!(formatter, "Trusted service runtime record not found"),
            Self::Conflict(message) => write!(formatter, "{message}"),
            Self::Blocked(message) => write!(formatter, "{message}"),
            Self::Io(message) => write!(
                formatter,
                "Trusted service runtime storage error: {message}"
            ),
        }
    }
}

impl From<AddonError> for RuntimeError {
    fn from(error: AddonError) -> Self {
        match error {
            AddonError::NotFound => Self::NotFound,
            AddonError::Invalid(message) => Self::Invalid(message),
            AddonError::AlreadyInstalled => Self::Conflict(error.to_string()),
            AddonError::Io(message) => Self::Io(message),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivationGate {
    pub name: String,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedServiceRuntimeRecord {
    pub record_version: u32,
    pub addon_id: String,
    pub addon_version: String,
    pub image: String,
    pub image_digest: String,
    pub capabilities: Vec<String>,
    pub storage_namespace: String,
    pub api_namespace: String,
    pub launch_profile: addons::TrustedServiceLaunchProfile,
    pub activation_instance: String,
    pub state: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    pub updated_at: String,
    pub execution_enabled: bool,
    pub gates: Vec<ActivationGate>,
    pub last_error: Option<String>,
}

fn storage_root() -> PathBuf {
    let root = std::env::var("SIGNAGE_DATA_DIR").unwrap_or_else(|_| "/home/board/data".to_string());
    PathBuf::from(root).join(RUNTIME_DIR)
}

fn record_dir(addon_id: &str) -> Result<PathBuf, RuntimeError> {
    addons::validate_addon_storage_namespace(addon_id).map_err(RuntimeError::Invalid)?;
    Ok(storage_root().join(addon_id))
}

fn record_path(addon_id: &str) -> Result<PathBuf, RuntimeError> {
    Ok(record_dir(addon_id)?.join(RUNTIME_FILE))
}

fn ensure_runtime_directory(addon_id: &str) -> Result<PathBuf, RuntimeError> {
    let root = storage_root();
    fs::create_dir_all(&root).map_err(|error| RuntimeError::Io(error.to_string()))?;
    let root_metadata =
        fs::symlink_metadata(&root).map_err(|error| RuntimeError::Io(error.to_string()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(RuntimeError::Invalid(
            "Invalid trusted service runtime root".to_string(),
        ));
    }
    let directory = record_dir(addon_id)?;
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => Err(
            RuntimeError::Invalid("Invalid trusted service runtime directory".to_string()),
        ),
        Ok(_) => Ok(directory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&directory).map_err(|error| RuntimeError::Io(error.to_string()))?;
            Ok(directory)
        }
        Err(error) => Err(RuntimeError::Io(error.to_string())),
    }
}

fn validate_approver(approver: &str) -> Result<(), RuntimeError> {
    if approver.is_empty()
        || approver.len() > 128
        || approver.chars().any(|character| character.is_control())
    {
        return Err(RuntimeError::Invalid(
            "Invalid trusted service approver".to_string(),
        ));
    }
    Ok(())
}

fn activation_gates() -> Vec<ActivationGate> {
    vec![
        ActivationGate {
            name: "manifest_signature".to_string(),
            passed: true,
            reason: "staged manifest was re-verified before approval".to_string(),
        },
        ActivationGate {
            name: "image_signature".to_string(),
            passed: true,
            reason: "staged image digest signature was re-verified before approval".to_string(),
        },
        ActivationGate {
            name: "administrator_approval".to_string(),
            passed: true,
            reason: "explicit administrator approval recorded".to_string(),
        },
        ActivationGate {
            name: "execution_backend".to_string(),
            passed: false,
            reason: "trusted service execution backend is not enabled".to_string(),
        },
        ActivationGate {
            name: "credential_broker".to_string(),
            passed: false,
            reason: "service credential broker is not implemented".to_string(),
        },
        ActivationGate {
            name: "database_boundary".to_string(),
            passed: false,
            reason: "service database contract and SQLite adapter exist, but service gateway integration is not enabled"
                .to_string(),
        },
        ActivationGate {
            name: "controlled_gateway".to_string(),
            passed: false,
            reason: "controlled internal gateway is not enabled".to_string(),
        },
        ActivationGate {
            name: "resource_policy".to_string(),
            passed: false,
            reason: "container resource and network policy is not implemented".to_string(),
        },
    ]
}

fn write_atomic(path: &Path, value: &TrustedServiceRuntimeRecord) -> Result<(), RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| RuntimeError::Io("Runtime record parent is missing".to_string()))?;
    fs::create_dir_all(parent).map_err(|error| RuntimeError::Io(error.to_string()))?;
    let temporary = parent.join(format!(".runtime-{}.tmp", Uuid::new_v4()));
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|error| RuntimeError::Io(error.to_string()))?;
    let result = (|| {
        fs::write(&temporary, bytes).map_err(|error| RuntimeError::Io(error.to_string()))?;
        fs::rename(&temporary, path).map_err(|error| RuntimeError::Io(error.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_record(path: &Path) -> Result<TrustedServiceRuntimeRecord, RuntimeError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RuntimeError::NotFound
        } else {
            RuntimeError::Io(error.to_string())
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RuntimeError::Invalid(
            "Trusted service runtime record is not a regular file".to_string(),
        ));
    }
    if metadata.len() > MAX_RUNTIME_RECORD_BYTES {
        return Err(RuntimeError::Invalid(
            "Trusted service runtime record is too large".to_string(),
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RuntimeError::NotFound
        } else {
            RuntimeError::Io(error.to_string())
        }
    })?;
    let record: TrustedServiceRuntimeRecord = serde_json::from_slice(&bytes).map_err(|_| {
        RuntimeError::Invalid("Trusted service runtime record is invalid".to_string())
    })?;
    if record.record_version != RUNTIME_RECORD_VERSION
        || record.addon_id.is_empty()
        || record.activation_instance.is_empty()
        || !matches!(record.state.as_str(), "approved" | "disabled")
        || record.execution_enabled
    {
        return Err(RuntimeError::Invalid(
            "Trusted service runtime record is invalid".to_string(),
        ));
    }
    validate_runtime_record_contract(&record)?;
    Ok(record)
}

fn record_from_stage(
    stage: &TrustedServiceStage,
    approver: &str,
) -> Result<TrustedServiceRuntimeRecord, RuntimeError> {
    let service = stage.manifest.trusted_service.as_ref().ok_or_else(|| {
        RuntimeError::Invalid("Trusted service stage lacks service metadata".to_string())
    })?;
    let now = Utc::now().to_rfc3339();
    Ok(TrustedServiceRuntimeRecord {
        record_version: RUNTIME_RECORD_VERSION,
        addon_id: stage.manifest.id.clone(),
        addon_version: stage.manifest.version.clone(),
        image: service.image.clone(),
        image_digest: service.image_digest.clone(),
        capabilities: service.capabilities.clone(),
        storage_namespace: service.storage_namespace.clone(),
        api_namespace: service.api_namespace.clone(),
        launch_profile: service.launch_profile.clone(),
        activation_instance: Uuid::new_v4().to_string(),
        state: "approved".to_string(),
        approved_by: Some(approver.to_string()),
        approved_at: Some(now.clone()),
        updated_at: now,
        execution_enabled: EXECUTION_ENABLED,
        gates: activation_gates(),
        last_error: None,
    })
}

/// Approve a staged service without activating it.
pub fn approve(
    addon_id: &str,
    approver: &str,
) -> Result<TrustedServiceRuntimeRecord, RuntimeError> {
    validate_approver(approver)?;
    let stage = addons::get_trusted_service_stage(addon_id)?;
    ensure_runtime_directory(addon_id)?;
    let path = record_path(addon_id)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(RuntimeError::Invalid(
                "Invalid trusted service runtime record".to_string(),
            ));
        }
        Ok(_) => {
            let existing = read_record(&path)?;
            if existing.state == "approved" {
                return Ok(existing);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RuntimeError::Io(error.to_string())),
    }
    let record = record_from_stage(&stage, approver)?;
    write_atomic(&path, &record)?;
    Ok(record)
}

/// Disable an approval record. No process stop is attempted in this phase.
pub fn disable(addon_id: &str) -> Result<TrustedServiceRuntimeRecord, RuntimeError> {
    let path = record_path(addon_id)?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RuntimeError::NotFound
        } else {
            RuntimeError::Io(error.to_string())
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RuntimeError::Invalid(
            "Invalid trusted service runtime record".to_string(),
        ));
    }
    let mut record = read_record(&path)?;
    record.state = "disabled".to_string();
    record.execution_enabled = false;
    record.updated_at = Utc::now().to_rfc3339();
    write_atomic(&path, &record)?;
    Ok(record)
}

pub fn list() -> Result<Vec<TrustedServiceRuntimeRecord>, RuntimeError> {
    let root = storage_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let root_metadata =
        fs::symlink_metadata(&root).map_err(|error| RuntimeError::Io(error.to_string()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(RuntimeError::Invalid(
            "Invalid trusted service runtime root".to_string(),
        ));
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| RuntimeError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| RuntimeError::Io(error.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| RuntimeError::Io(error.to_string()))?
            .is_dir()
        {
            return Err(RuntimeError::Invalid(
                "Invalid trusted service runtime directory".to_string(),
            ));
        }
        if records.len() >= MAX_RUNTIME_RECORDS {
            return Err(RuntimeError::Invalid(
                "Too many trusted service runtime records".to_string(),
            ));
        }
        let record = read_record(&entry.path().join(RUNTIME_FILE))?;
        if record.addon_id != name {
            return Err(RuntimeError::Invalid(
                "Trusted service runtime identity mismatch".to_string(),
            ));
        }
        records.push(record);
    }
    records.sort_by(|left, right| left.addon_id.cmp(&right.addon_id));
    Ok(records)
}

/// Remove only the inert runtime record associated with staged metadata.
pub fn remove(addon_id: &str) -> Result<(), RuntimeError> {
    let directory = record_dir(addon_id)?;
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RuntimeError::Invalid(
                    "Invalid trusted service runtime directory".to_string(),
                ));
            }
            fs::remove_dir_all(directory).map_err(|error| RuntimeError::Io(error.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(RuntimeError::Io(error.to_string())),
    }
}

pub const TRUSTED_SERVICE_CAPABILITIES: &[&str] = &[
    "storage",
    "read:clients",
    "read:layouts",
    "client_extension",
];

fn validate_capabilities(capabilities: &[String]) -> Result<(), RuntimeError> {
    if capabilities.len() > TRUSTED_SERVICE_CAPABILITIES.len() {
        return Err(RuntimeError::Invalid(
            "Trusted service declares too many capabilities".to_string(),
        ));
    }
    let mut seen = BTreeSet::new();
    for capability in capabilities {
        if !TRUSTED_SERVICE_CAPABILITIES.contains(&capability.as_str()) {
            return Err(RuntimeError::Invalid(
                "Trusted service declares an unsupported capability".to_string(),
            ));
        }
        if !seen.insert(capability) {
            return Err(RuntimeError::Invalid(
                "Trusted service declares a duplicate capability".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_capability_binding(record: &TrustedServiceRuntimeRecord) -> Result<(), RuntimeError> {
    validate_capabilities(&record.capabilities)?;
    addons::validate_addon_storage_namespace(&record.storage_namespace)
        .map_err(RuntimeError::Invalid)?;
    addons::validate_addon_storage_namespace(&record.api_namespace)
        .map_err(RuntimeError::Invalid)?;
    Ok(())
}

fn validate_runtime_record_contract(
    record: &TrustedServiceRuntimeRecord,
) -> Result<(), RuntimeError> {
    if record.execution_enabled != EXECUTION_ENABLED {
        return Err(RuntimeError::Invalid(
            "Trusted service execution flag does not match the runtime boundary".to_string(),
        ));
    }
    addons::validate_addon_storage_namespace(&record.addon_id).map_err(RuntimeError::Invalid)?;
    addons::validate_addon_storage_namespace(&record.activation_instance)
        .map_err(RuntimeError::Invalid)?;
    if record.storage_namespace != record.addon_id || record.api_namespace != record.addon_id {
        return Err(RuntimeError::Invalid(
            "Trusted service runtime namespaces are not bound to the addon".to_string(),
        ));
    }
    validate_capability_binding(record)?;
    addons::validate_trusted_service_launch_profile(&record.launch_profile)
        .map_err(|error| RuntimeError::Invalid(error.to_string()))?;
    addons::validate_trusted_service_image_reference(&record.image)
        .map_err(RuntimeError::Invalid)?;
    addons::validate_trusted_service_image_digest(&record.image_digest)
        .map_err(RuntimeError::Invalid)?;
    crate::trusted_service_oci::validate_oci_activation_gate_records(&record.gates)
        .map_err(RuntimeError::Invalid)?;
    crate::trusted_service_oci::validate_oci_approval_metadata(record)
        .map_err(RuntimeError::Invalid)?;
    Ok(())
}

/// Immutable launch data passed across the supervisor/runtime boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLaunchRequest {
    addon_id: String,
    activation_instance: String,
    image: String,
    image_digest: String,
    capabilities: Vec<String>,
    storage_namespace: String,
    api_namespace: String,
    launch_profile: addons::TrustedServiceLaunchProfile,
}

impl RuntimeLaunchRequest {
    fn from_approved_record(record: &TrustedServiceRuntimeRecord) -> Result<Self, RuntimeError> {
        validate_runtime_record_contract(record)?;
        if record.execution_enabled != EXECUTION_ENABLED {
            return Err(RuntimeError::Invalid(
                "Trusted service execution flag does not match the runtime boundary".to_string(),
            ));
        }
        if record.state != "approved" {
            return Err(RuntimeError::Blocked(
                "Trusted service is not approved for activation".to_string(),
            ));
        }
        Ok(Self {
            addon_id: record.addon_id.clone(),
            activation_instance: record.activation_instance.clone(),
            image: record.image.clone(),
            image_digest: record.image_digest.clone(),
            capabilities: record.capabilities.clone(),
            storage_namespace: record.storage_namespace.clone(),
            api_namespace: record.api_namespace.clone(),
            launch_profile: record.launch_profile.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHandle {
    addon_id: String,
    activation_instance: String,
}

impl RuntimeHandle {
    pub fn addon_id(&self) -> &str {
        &self.addon_id
    }

    pub fn activation_instance(&self) -> &str {
        &self.activation_instance
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Running,
    Stopped,
    Crashed,
    TimedOut,
    Replaced,
}

pub trait TrustedServiceRuntime {
    fn start(&mut self, request: &RuntimeLaunchRequest) -> Result<RuntimeHandle, RuntimeError>;
    fn stop(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError>;
    fn status(&self, handle: &RuntimeHandle) -> Result<RuntimeStatus, RuntimeError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPlan {
    pub addon_id: String,
    pub activation_instance: String,
    pub can_start: bool,
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationReplacementPlan {
    pub old_activation_instance: String,
    pub new_activation_instance: String,
    pub can_start: bool,
    pub blocked_by: Vec<String>,
}

pub struct TrustedServiceSupervisor<R> {
    runtime: R,
}

impl<R: TrustedServiceRuntime> TrustedServiceSupervisor<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    pub fn activation_plan(
        record: &TrustedServiceRuntimeRecord,
    ) -> Result<ActivationPlan, RuntimeError> {
        validate_runtime_record_contract(record)?;
        if record.state != "approved" {
            return Err(RuntimeError::Blocked(
                "Trusted service is not approved for activation".to_string(),
            ));
        }
        let mut blocked_by = record
            .gates
            .iter()
            .filter(|gate| !gate.passed)
            .map(|gate| gate.name.clone())
            .collect::<Vec<_>>();
        if !EXECUTION_ENABLED && !blocked_by.iter().any(|gate| gate == "execution_backend") {
            blocked_by.push("execution_backend".to_string());
        }
        Ok(ActivationPlan {
            addon_id: record.addon_id.clone(),
            activation_instance: record.activation_instance.clone(),
            can_start: blocked_by.is_empty(),
            blocked_by,
        })
    }

    pub fn activation_replacement_plan(
        old_handle: &RuntimeHandle,
        record: &TrustedServiceRuntimeRecord,
    ) -> Result<ActivationReplacementPlan, RuntimeError> {
        if old_handle.activation_instance.is_empty() {
            return Err(RuntimeError::Invalid(
                "Old activation identity is empty".to_string(),
            ));
        }
        addons::validate_addon_storage_namespace(&old_handle.addon_id)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&old_handle.activation_instance)
            .map_err(RuntimeError::Invalid)?;
        if old_handle.addon_id != record.addon_id {
            return Err(RuntimeError::Conflict(
                "Activation replacement handle is bound to a different addon".to_string(),
            ));
        }
        if old_handle.activation_instance == record.activation_instance {
            return Err(RuntimeError::Conflict(
                "Activation replacement requires a new activation identity".to_string(),
            ));
        }
        let plan = Self::activation_plan(record)?;
        Ok(ActivationReplacementPlan {
            old_activation_instance: old_handle.activation_instance.clone(),
            new_activation_instance: plan.activation_instance,
            can_start: plan.can_start,
            blocked_by: plan.blocked_by,
        })
    }

    pub fn replacement_plan(
        &self,
        old_handle: &RuntimeHandle,
        record: &TrustedServiceRuntimeRecord,
    ) -> Result<ActivationReplacementPlan, RuntimeError> {
        let status = self.runtime.status(old_handle)?;
        if status != RuntimeStatus::Running {
            return Err(RuntimeError::Blocked(
                "Activation replacement requires a running old activation".to_string(),
            ));
        }
        Self::activation_replacement_plan(old_handle, record)
    }

    pub fn activate(
        &mut self,
        record: &TrustedServiceRuntimeRecord,
    ) -> Result<RuntimeHandle, RuntimeError> {
        let plan = Self::activation_plan(record)?;
        if !plan.can_start {
            return Err(RuntimeError::Blocked(format!(
                "Trusted service activation is blocked by: {}",
                plan.blocked_by.join(", ")
            )));
        }
        self.runtime
            .start(&RuntimeLaunchRequest::from_approved_record(record)?)
    }

    pub fn deactivate(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        self.runtime.stop(handle)
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeRuntimeAction {
    Start(String),
    Stop(String),
    Crash(String),
    Timeout(String),
    Replace(String),
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeTrustedServiceRuntime {
    root: PathBuf,
    actions: Vec<FakeRuntimeAction>,
    running: BTreeMap<String, RuntimeHandle>,
    known: BTreeMap<String, RuntimeHandle>,
    terminal: BTreeMap<String, (RuntimeHandle, RuntimeStatus)>,
}

#[cfg(test)]
fn running_marker_path(root: &Path, handle: &RuntimeHandle) -> PathBuf {
    root.join("running")
        .join(&handle.addon_id)
        .join(&handle.activation_instance)
}

#[cfg(test)]
impl FakeTrustedServiceRuntime {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            actions: Vec::new(),
            running: BTreeMap::new(),
            known: BTreeMap::new(),
            terminal: BTreeMap::new(),
        }
    }

    pub fn actions(&self) -> &[FakeRuntimeAction] {
        &self.actions
    }
}

#[cfg(test)]
impl TrustedServiceRuntime for FakeTrustedServiceRuntime {
    fn start(&mut self, request: &RuntimeLaunchRequest) -> Result<RuntimeHandle, RuntimeError> {
        if request.addon_id.is_empty()
            || request.activation_instance.is_empty()
            || addons::validate_trusted_service_image_reference(&request.image).is_err()
            || addons::validate_trusted_service_image_digest(&request.image_digest).is_err()
        {
            return Err(RuntimeError::Invalid(
                "Fake runtime launch request is invalid".to_string(),
            ));
        }
        validate_capabilities(&request.capabilities)?;
        addons::validate_trusted_service_launch_profile(&request.launch_profile)
            .map_err(|error| RuntimeError::Invalid(error.to_string()))?;
        addons::validate_addon_storage_namespace(&request.addon_id)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&request.activation_instance)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&request.storage_namespace)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&request.api_namespace)
            .map_err(RuntimeError::Invalid)?;
        if self.known.contains_key(&request.activation_instance) {
            return Err(RuntimeError::Conflict(
                "Fake runtime activation identity has already been used".to_string(),
            ));
        }
        let handle = RuntimeHandle {
            addon_id: request.addon_id.clone(),
            activation_instance: request.activation_instance.clone(),
        };
        let marker = running_marker_path(&self.root, &handle);
        let marker_parent = marker.parent().ok_or_else(|| {
            RuntimeError::Invalid("Fake runtime marker path is invalid".to_string())
        })?;
        fs::create_dir_all(marker_parent)
            .map_err(|error| RuntimeError::Io(error.to_string()))?;
        fs::write(&marker, request.addon_id.as_bytes())
            .map_err(|error| RuntimeError::Io(error.to_string()))?;
        self.known
            .insert(handle.activation_instance.clone(), handle.clone());
        self.terminal.remove(&handle.activation_instance);
        self.running
            .insert(handle.activation_instance.clone(), handle.clone());
        self.actions
            .push(FakeRuntimeAction::Start(request.addon_id.clone()));
        Ok(handle)
    }

    fn stop(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        addons::validate_addon_storage_namespace(&handle.addon_id)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&handle.activation_instance)
            .map_err(RuntimeError::Invalid)?;
        let Some(running_handle) = self.running.get(&handle.activation_instance) else {
            return Err(RuntimeError::NotFound);
        };
        if running_handle.addon_id != handle.addon_id {
            return Err(RuntimeError::Conflict(
                "Runtime handle is not bound to the running addon".to_string(),
            ));
        }
        fs::remove_file(running_marker_path(&self.root, handle))
            .map_err(|error| RuntimeError::Io(error.to_string()))?;
        self.running.remove(&handle.activation_instance);
        self.terminal.insert(
            handle.activation_instance.clone(),
            (handle.clone(), RuntimeStatus::Stopped),
        );
        self.actions
            .push(FakeRuntimeAction::Stop(handle.activation_instance.clone()));
        Ok(())
    }

    fn status(&self, handle: &RuntimeHandle) -> Result<RuntimeStatus, RuntimeError> {
        addons::validate_addon_storage_namespace(&handle.addon_id)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&handle.activation_instance)
            .map_err(RuntimeError::Invalid)?;
        if let Some(running_handle) = self.running.get(&handle.activation_instance) {
            if running_handle.addon_id != handle.addon_id {
                return Err(RuntimeError::Conflict(
                    "Runtime handle is not bound to the running addon".to_string(),
                ));
            }
            Ok(RuntimeStatus::Running)
        } else if let Some((terminal_handle, status)) = self.terminal.get(&handle.activation_instance) {
            if terminal_handle.addon_id != handle.addon_id {
                return Err(RuntimeError::Conflict(
                    "Runtime handle is not bound to the terminal addon".to_string(),
                ));
            }
            Ok(*status)
        } else if let Some(known_handle) = self.known.get(&handle.activation_instance) {
            if known_handle.addon_id != handle.addon_id {
                return Err(RuntimeError::Conflict(
                    "Runtime handle is not bound to the known addon".to_string(),
                ));
            }
            Ok(RuntimeStatus::Stopped)
        } else {
            Err(RuntimeError::NotFound)
        }
    }
}

#[cfg(test)]
impl FakeTrustedServiceRuntime {
    fn mark_terminal(
        &mut self,
        handle: &RuntimeHandle,
        status: RuntimeStatus,
        action: FakeRuntimeAction,
    ) -> Result<(), RuntimeError> {
        let Some(running_handle) = self.running.get(&handle.activation_instance) else {
            return Err(RuntimeError::NotFound);
        };
        if running_handle.addon_id != handle.addon_id {
            return Err(RuntimeError::Conflict(
                "Runtime handle is not bound to the running addon".to_string(),
            ));
        }
        fs::remove_file(running_marker_path(&self.root, handle))
            .map_err(|error| RuntimeError::Io(error.to_string()))?;
        self.running.remove(&handle.activation_instance);
        self.terminal.insert(
            handle.activation_instance.clone(),
            (handle.clone(), status),
        );
        self.actions.push(action);
        Ok(())
    }

    fn mark_crashed(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        self.mark_terminal(
            handle,
            RuntimeStatus::Crashed,
            FakeRuntimeAction::Crash(handle.activation_instance.clone()),
        )
    }

    fn mark_timed_out(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        self.mark_terminal(
            handle,
            RuntimeStatus::TimedOut,
            FakeRuntimeAction::Timeout(handle.activation_instance.clone()),
        )
    }

    fn mark_replaced(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        self.mark_terminal(
            handle,
            RuntimeStatus::Replaced,
            FakeRuntimeAction::Replace(handle.activation_instance.clone()),
        )
    }
}

#[cfg(test)]
struct DisposableRuntimeHarness {
    root: PathBuf,
    supervisor: TrustedServiceSupervisor<FakeTrustedServiceRuntime>,
}

#[cfg(test)]
impl DisposableRuntimeHarness {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("screentra-runtime-harness-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create disposable runtime root");
        let runtime_root = root.clone();
        Self {
            root,
            supervisor: TrustedServiceSupervisor::new(FakeTrustedServiceRuntime::new(runtime_root)),
        }
    }
}

#[cfg(test)]
impl Drop for DisposableRuntimeHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn activation_gates_are_fail_closed() {
        let _guard = TEST_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        assert!(!EXECUTION_ENABLED);
        let gates = activation_gates();
        assert!(gates
            .iter()
            .any(|gate| gate.name == "execution_backend" && !gate.passed));
        assert!(gates
            .iter()
            .any(|gate| gate.name == "credential_broker" && !gate.passed));
        assert!(gates
            .iter()
            .any(|gate| gate.name == "database_boundary" && !gate.passed));
        assert!(gates
            .iter()
            .any(|gate| gate.name == "controlled_gateway" && !gate.passed));
    }

    #[test]
    fn launch_request_is_derived_only_from_an_approved_record() {
        let record = test_record();
        let request = RuntimeLaunchRequest::from_approved_record(&record)
            .expect("approved record should produce a launch request");
        assert_eq!(request.addon_id, record.addon_id);
        assert_eq!(request.activation_instance, record.activation_instance);
        assert_eq!(request.launch_profile, record.launch_profile);

        let mut disabled = record;
        disabled.state = "disabled".to_string();
        assert!(matches!(
            RuntimeLaunchRequest::from_approved_record(&disabled),
            Err(RuntimeError::Blocked(_))
        ));

        let mut mismatched_execution = test_record();
        mismatched_execution.execution_enabled = true;
        assert!(matches!(
            RuntimeLaunchRequest::from_approved_record(&mismatched_execution),
            Err(RuntimeError::Invalid(_))
        ));
    }

    #[test]
    fn runtime_record_round_trip_rejects_execution_enabled_records() {
        let root =
            std::env::temp_dir().join(format!("screentra-runtime-record-{}", Uuid::new_v4()));
        let path = root.join("example.service").join(RUNTIME_FILE);
        let record = test_record();
        write_atomic(&path, &record).unwrap();
        assert_eq!(read_record(&path).unwrap(), record);
        let mut unsafe_record = record;
        unsafe_record.execution_enabled = true;
        write_atomic(&path, &unsafe_record).unwrap();
        assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn persisted_runtime_records_reject_invalid_launch_contract_fields() {
        let root = std::env::temp_dir().join(format!(
            "screentra-runtime-record-contract-{}",
            Uuid::new_v4()
        ));
        let path = root.join("example.service").join(RUNTIME_FILE);
        let mut invalid = test_record();
        invalid.image = "service".to_string();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_atomic(&path, &invalid).unwrap();
        assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));

        invalid = test_record();
        invalid.capabilities = vec!["read:secrets".to_string()];
        write_atomic(&path, &invalid).unwrap();
        assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));

        invalid = test_record();
        invalid.gates.clear();
        write_atomic(&path, &invalid).unwrap();
        assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn persisted_record_rejects_tampered_launch_profile() {
        let mut record = test_record();
        record.launch_profile.gateway_socket_path = "/tmp/host.sock".to_string();
        assert!(validate_runtime_record_contract(&record).is_err());

        let mut record = test_record();
        record.launch_profile.command = vec!["/bin/sh".to_string(), "-c".to_string()];
        assert!(validate_runtime_record_contract(&record).is_err());
    }

    #[test]
    fn runtime_record_size_and_file_type_are_bounded() {
        let root =
            std::env::temp_dir().join(format!("screentra-runtime-bounds-{}", Uuid::new_v4()));
        let path = root.join("example.service").join(RUNTIME_FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'{'; (MAX_RUNTIME_RECORD_BYTES as usize) + 1]).unwrap();
        assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));

        #[cfg(unix)]
        {
            let target = root.join("target.json");
            fs::write(&target, serde_json::to_vec(&test_record()).unwrap()).unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(matches!(read_record(&path), Err(RuntimeError::Invalid(_))));
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fake_runtime_records_start_and_stop_without_host_processes() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-1".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        assert_eq!(request.launch_profile, test_launch_profile());
        let mut malformed_profile = request.clone();
        malformed_profile.launch_profile.gateway_socket_path = "/tmp/host.sock".to_string();
        assert!(matches!(
            harness.supervisor.runtime_mut().start(&malformed_profile),
            Err(RuntimeError::Invalid(_))
        ));
        assert!(!harness
            .root
            .join("running")
            .join("example.service")
            .join("activation-1").exists());
        let mut malformed_digest = request.clone();
        malformed_digest.image_digest = "sha256:ABC".to_string();
        assert!(matches!(
            harness.supervisor.runtime_mut().start(&malformed_digest),
            Err(RuntimeError::Invalid(_))
        ));
        let mut malformed_image = request.clone();
        malformed_image.image = "service".to_string();
        assert!(matches!(
            harness.supervisor.runtime_mut().start(&malformed_image),
            Err(RuntimeError::Invalid(_))
        ));
        let mut traversal_instance = request.clone();
        traversal_instance.activation_instance = "../escape".to_string();
        assert!(matches!(
            harness.supervisor.runtime_mut().start(&traversal_instance),
            Err(RuntimeError::Invalid(_))
        ));
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Running
        );
        assert!(harness
            .root
            .join("running")
            .join("example.service")
            .join("activation-1").is_file());
        harness
            .supervisor
            .runtime_mut()
            .stop(&handle)
            .expect("fake runtime stop");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Stopped
        );
        assert!(!harness
            .root
            .join("running")
            .join("example.service")
            .join("activation-1").exists());
        assert_eq!(harness.supervisor.runtime().actions().len(), 2);
    }

    #[test]
    fn fake_runtime_crash_and_timeout_are_terminal_and_do_not_restart() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-failure".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        harness
            .supervisor
            .runtime_mut()
            .mark_crashed(&handle)
            .expect("fake runtime crash");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Crashed
        );
        assert!(!harness
            .root
            .join("running")
            .join("example.service")
            .join("activation-failure")
            .exists());
        assert!(matches!(
            harness.supervisor.deactivate(&handle),
            Err(RuntimeError::NotFound)
        ));
        assert_eq!(harness.supervisor.runtime().actions().len(), 2);

        let timeout_handle = harness
            .supervisor
            .runtime_mut()
            .start(&RuntimeLaunchRequest {
                activation_instance: "activation-timeout".to_string(),
                ..request
            })
            .expect("second fake runtime start");
        harness
            .supervisor
            .runtime_mut()
            .mark_timed_out(&timeout_handle)
            .expect("fake runtime timeout");
        assert_eq!(
            harness
                .supervisor
                .runtime()
                .status(&timeout_handle)
                .unwrap(),
            RuntimeStatus::TimedOut
        );
    }

    #[test]
    fn fake_runtime_cleanup_is_scoped_to_addon_and_activation() {
        let root = std::env::temp_dir().join(format!("screentra-runtime-scope-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create disposable runtime root");
        let mut first = FakeTrustedServiceRuntime::new(root.clone());
        let mut second = FakeTrustedServiceRuntime::new(root.clone());
        let first_request = RuntimeLaunchRequest {
            addon_id: "first.service".to_string(),
            activation_instance: "shared-activation".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "first.service".to_string(),
            api_namespace: "first.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let second_request = RuntimeLaunchRequest {
            addon_id: "second.service".to_string(),
            storage_namespace: "second.service".to_string(),
            api_namespace: "second.service".to_string(),
            ..first_request.clone()
        };
        let first_handle = first.start(&first_request).expect("first runtime start");
        let second_handle = second
            .start(&second_request)
            .expect("second runtime start");
        let first_marker = root
            .join("running")
            .join("first.service")
            .join("shared-activation");
        let second_marker = root
            .join("running")
            .join("second.service")
            .join("shared-activation");
        assert!(first_marker.is_file());
        assert!(second_marker.is_file());
        first.stop(&first_handle).expect("first runtime stop");
        assert!(!first_marker.exists());
        assert!(second_marker.is_file());
        second.stop(&second_handle).expect("second runtime stop");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn runtime_cleanup_failure_preserves_running_state() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "cleanup-failure".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        fs::remove_file(running_marker_path(&harness.root, &handle))
            .expect("remove marker to simulate cleanup failure");
        assert!(matches!(
            harness.supervisor.runtime_mut().stop(&handle),
            Err(RuntimeError::Io(_))
        ));
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Running
        );
        assert_eq!(harness.supervisor.runtime().actions().len(), 1);
    }

    #[test]
    fn runtime_handles_bind_status_and_stop_to_addon_identity() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-bound".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        assert_eq!(handle.addon_id(), "example.service");
        assert_eq!(handle.activation_instance(), "activation-bound");
        let forged = RuntimeHandle {
            addon_id: "other.service".to_string(),
            activation_instance: handle.activation_instance.clone(),
        };
        assert!(matches!(
            harness.supervisor.runtime().status(&forged),
            Err(RuntimeError::Conflict(_))
        ));
        assert!(matches!(
            harness.supervisor.deactivate(&forged),
            Err(RuntimeError::Conflict(_))
        ));
        assert!(harness
            .root
            .join("running")
            .join("example.service")
            .join("activation-bound")
            .exists());
        harness
            .supervisor
            .deactivate(&handle)
            .expect("bound runtime handle should stop");
        assert!(matches!(
            harness.supervisor.runtime().status(&forged),
            Err(RuntimeError::Conflict(_))
        ));
        assert_eq!(harness.supervisor.runtime().actions().len(), 2);
    }

    #[test]
    fn malformed_stage_metadata_returns_error_instead_of_panicking() {
        let stage = TrustedServiceStage {
            manifest: addons::AddonManifest {
                schema_version: 1,
                id: "example.service".to_string(),
                name: "Example".to_string(),
                description: String::new(),
                version: "1.0.0".to_string(),
                kind: "trusted_server_service".to_string(),
                min_server_version: None,
                min_client_version: None,
                widget_types: Vec::new(),
                permissions: Vec::new(),
                client_entrypoint: None,
                server_entrypoint: None,
                trusted_service: None,
                client_sha256: None,
                client_api_version: None,
            },
            state: "staged".to_string(),
            staged_at: Utc::now().to_rfc3339(),
        };
        assert!(matches!(
            record_from_stage(&stage, "admin"),
            Err(RuntimeError::Invalid(_))
        ));
    }

    #[test]
    fn fake_runtime_replacement_is_terminal_and_does_not_restart() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-replaced".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        harness
            .supervisor
            .runtime_mut()
            .mark_replaced(&handle)
            .expect("fake runtime replacement");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Replaced
        );
        assert!(!running_marker_path(&harness.root, &handle).exists());
        assert!(matches!(
            harness.supervisor.deactivate(&handle),
            Err(RuntimeError::NotFound)
        ));
        assert_eq!(harness.supervisor.runtime().actions().len(), 2);
    }

    #[test]
    fn supervisor_replacement_requires_a_running_old_handle() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-old".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        let mut replacement = test_record();
        replacement.activation_instance = "activation-new".to_string();
        harness
            .supervisor
            .deactivate(&handle)
            .expect("old activation stop");
        assert!(matches!(
            harness.supervisor.replacement_plan(&handle, &replacement),
            Err(RuntimeError::Blocked(_))
        ));
    }

    #[test]
    fn replacement_plan_requires_a_new_activation_identity_before_stop() {
        let record = test_record();
        let same = RuntimeHandle {
            addon_id: record.addon_id.clone(),
            activation_instance: record.activation_instance.clone(),
        };
        assert!(matches!(
            TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_replacement_plan(
                &same, &record
            ),
            Err(RuntimeError::Conflict(_))
        ));
        let wrong_addon = RuntimeHandle {
            addon_id: "other.service".to_string(),
            activation_instance: "activation-old".to_string(),
        };
        assert!(matches!(
            TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_replacement_plan(
                &wrong_addon,
                &record,
            ),
            Err(RuntimeError::Conflict(_))
        ));

        let mut replacement = record.clone();
        replacement.activation_instance = "activation-replacement".to_string();
        let plan =
            TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_replacement_plan(
                &same,
                &replacement,
            )
            .expect("replacement should produce a blocked-or-ready plan");
        assert_eq!(plan.old_activation_instance, "activation-1");
        assert_eq!(plan.new_activation_instance, "activation-replacement");
        assert!(!plan.can_start);
    }

    #[test]
    fn supervisor_blocks_activation_until_every_execution_gate_is_ready() {
        let mut harness = DisposableRuntimeHarness::new();
        let record = test_record();
        let error = harness
            .supervisor
            .activate(&record)
            .expect_err("disabled execution must fail closed");
        assert!(matches!(error, RuntimeError::Blocked(_)));
        assert!(harness.supervisor.runtime().actions().is_empty());

        let mut all_gates_passed = test_record();
        all_gates_passed
            .gates
            .iter_mut()
            .for_each(|gate| gate.passed = true);
        let plan = TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_plan(
            &all_gates_passed,
        )
        .expect("valid gate records should produce a blocked plan");
        assert!(!plan.can_start);
        assert!(plan
            .blocked_by
            .iter()
            .any(|gate| gate == "execution_backend"));
        assert!(matches!(
            harness.supervisor.activate(&all_gates_passed),
            Err(RuntimeError::Blocked(_))
        ));
        assert!(harness.supervisor.runtime().actions().is_empty());
    }

    fn test_launch_profile() -> addons::TrustedServiceLaunchProfile {
        addons::TrustedServiceLaunchProfile {
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

    fn test_record() -> TrustedServiceRuntimeRecord {
        TrustedServiceRuntimeRecord {
            record_version: RUNTIME_RECORD_VERSION,
            addon_id: "example.service".to_string(),
            addon_version: "1.0.0".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
            activation_instance: "activation-1".to_string(),
            state: "approved".to_string(),
            approved_by: Some("admin".to_string()),
            approved_at: Some(Utc::now().to_rfc3339()),
            updated_at: Utc::now().to_rfc3339(),
            execution_enabled: false,
            gates: activation_gates(),
            last_error: None,
        }
    }

    #[test]
    fn supervisor_rejects_unbound_capabilities_before_runtime_start() {
        let mut harness = DisposableRuntimeHarness::new();
        let mut record = test_record();
        record.capabilities.push("read:secrets".to_string());
        let error = harness
            .supervisor
            .activate(&record)
            .expect_err("unknown capabilities must fail closed");
        assert!(matches!(error, RuntimeError::Invalid(_)));
        assert!(harness.supervisor.runtime().actions().is_empty());
    }

    #[test]
    fn disabled_activation_records_cannot_plan_or_start() {
        let mut harness = DisposableRuntimeHarness::new();
        let mut record = test_record();
        record.state = "disabled".to_string();
        let plan_error =
            TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_plan(&record)
                .expect_err("disabled records must not produce an activation plan");
        assert!(matches!(plan_error, RuntimeError::Blocked(_)));
        let start_error = harness
            .supervisor
            .activate(&record)
            .expect_err("disabled records must not start");
        assert!(matches!(start_error, RuntimeError::Blocked(_)));
        assert!(harness.supervisor.runtime().actions().is_empty());
    }

    #[test]
    fn stopped_activation_handles_cannot_be_stopped_again() {
        let mut harness = DisposableRuntimeHarness::new();
        let request = RuntimeLaunchRequest {
            addon_id: "example.service".to_string(),
            activation_instance: "activation-stop-once".to_string(),
            image: "registry.example/service".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.service".to_string(),
            api_namespace: "example.service".to_string(),
            launch_profile: test_launch_profile(),
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        harness
            .supervisor
            .deactivate(&handle)
            .expect("first stop should succeed");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Stopped
        );
        assert!(matches!(
            harness.supervisor.deactivate(&handle),
            Err(RuntimeError::NotFound)
        ));
        assert!(matches!(
            harness.supervisor.runtime_mut().start(&request),
            Err(RuntimeError::Conflict(_))
        ));
    }

    #[test]
    fn activation_plan_is_not_a_runtime_start_request() {
        let record = test_record();
        let plan = TrustedServiceSupervisor::<FakeTrustedServiceRuntime>::activation_plan(&record)
            .expect("approved records can produce a plan");
        assert!(!plan.can_start);
        assert!(plan
            .blocked_by
            .iter()
            .any(|gate| gate == "execution_backend"));
    }
}
