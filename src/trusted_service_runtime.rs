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
    Ok(record)
}

fn record_from_stage(stage: &TrustedServiceStage, approver: &str) -> TrustedServiceRuntimeRecord {
    let service = stage
        .manifest
        .trusted_service
        .as_ref()
        .expect("validated trusted service stage");
    let now = Utc::now().to_rfc3339();
    TrustedServiceRuntimeRecord {
        record_version: RUNTIME_RECORD_VERSION,
        addon_id: stage.manifest.id.clone(),
        addon_version: stage.manifest.version.clone(),
        image: service.image.clone(),
        image_digest: service.image_digest.clone(),
        capabilities: service.capabilities.clone(),
        storage_namespace: service.storage_namespace.clone(),
        api_namespace: service.api_namespace.clone(),
        activation_instance: Uuid::new_v4().to_string(),
        state: "approved".to_string(),
        approved_by: Some(approver.to_string()),
        approved_at: Some(now.clone()),
        updated_at: now,
        execution_enabled: EXECUTION_ENABLED,
        gates: activation_gates(),
        last_error: None,
    }
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
    let record = record_from_stage(&stage, approver);
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

/// Immutable launch data passed across the supervisor/runtime boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLaunchRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub image: String,
    pub image_digest: String,
    pub capabilities: Vec<String>,
    pub storage_namespace: String,
    pub api_namespace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHandle {
    pub activation_instance: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    Running,
    Stopped,
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
        validate_capability_binding(record)?;
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
        self.runtime.start(&RuntimeLaunchRequest {
            addon_id: record.addon_id.clone(),
            activation_instance: record.activation_instance.clone(),
            image: record.image.clone(),
            image_digest: record.image_digest.clone(),
            capabilities: record.capabilities.clone(),
            storage_namespace: record.storage_namespace.clone(),
            api_namespace: record.api_namespace.clone(),
        })
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
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeTrustedServiceRuntime {
    root: PathBuf,
    actions: Vec<FakeRuntimeAction>,
    running: BTreeMap<String, RuntimeHandle>,
    known: BTreeSet<String>,
}

#[cfg(test)]
impl FakeTrustedServiceRuntime {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            actions: Vec::new(),
            running: BTreeMap::new(),
            known: BTreeSet::new(),
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
            || !request.image_digest.starts_with("sha256:")
        {
            return Err(RuntimeError::Invalid(
                "Fake runtime launch request is invalid".to_string(),
            ));
        }
        validate_capabilities(&request.capabilities)?;
        addons::validate_addon_storage_namespace(&request.storage_namespace)
            .map_err(RuntimeError::Invalid)?;
        addons::validate_addon_storage_namespace(&request.api_namespace)
            .map_err(RuntimeError::Invalid)?;
        if self.running.contains_key(&request.activation_instance) {
            return Err(RuntimeError::Conflict(
                "Fake runtime activation is already running".to_string(),
            ));
        }
        let handle = RuntimeHandle {
            activation_instance: request.activation_instance.clone(),
        };
        let running_dir = self.root.join("running");
        fs::create_dir_all(&running_dir).map_err(|error| RuntimeError::Io(error.to_string()))?;
        fs::write(
            running_dir.join(&handle.activation_instance),
            request.addon_id.as_bytes(),
        )
        .map_err(|error| RuntimeError::Io(error.to_string()))?;
        self.known.insert(handle.activation_instance.clone());
        self.running
            .insert(handle.activation_instance.clone(), handle.clone());
        self.actions
            .push(FakeRuntimeAction::Start(request.addon_id.clone()));
        Ok(handle)
    }

    fn stop(&mut self, handle: &RuntimeHandle) -> Result<(), RuntimeError> {
        if self.running.remove(&handle.activation_instance).is_none() {
            return Err(RuntimeError::NotFound);
        }
        fs::remove_file(self.root.join("running").join(&handle.activation_instance))
            .map_err(|error| RuntimeError::Io(error.to_string()))?;
        self.actions
            .push(FakeRuntimeAction::Stop(handle.activation_instance.clone()));
        Ok(())
    }

    fn status(&self, handle: &RuntimeHandle) -> Result<RuntimeStatus, RuntimeError> {
        if self.running.contains_key(&handle.activation_instance) {
            Ok(RuntimeStatus::Running)
        } else if self.known.contains(&handle.activation_instance) {
            Ok(RuntimeStatus::Stopped)
        } else {
            Err(RuntimeError::NotFound)
        }
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
        };
        let handle = harness
            .supervisor
            .runtime_mut()
            .start(&request)
            .expect("fake runtime start");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Running
        );
        assert!(harness.root.join("running").join("activation-1").is_file());
        harness
            .supervisor
            .runtime_mut()
            .stop(&handle)
            .expect("fake runtime stop");
        assert_eq!(
            harness.supervisor.runtime().status(&handle).unwrap(),
            RuntimeStatus::Stopped
        );
        assert!(!harness.root.join("running").join("activation-1").exists());
        assert_eq!(harness.supervisor.runtime().actions().len(), 2);
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
