use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::addons;
use crate::trusted_service_database::{
    DatabaseOperation, DatabaseRequest, TrustedServiceDatabaseContract,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    Server,
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonContainerSpec {
    pub image: String,
    pub image_digest: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProtocol {
    Tcp,
    Udp,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkAllowRule {
    pub name: String,
    pub ip: String,
    #[serde(default)]
    pub mac: Option<String>,
    pub protocol: NetworkProtocol,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    HostApiRead,
    AddonStorage,
    CameraCapture,
    MicrophoneCapture,
    SpeakerOutput,
    LocalInference,
    NetworkEgress,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonCapabilityRequest {
    pub target: ExecutionTarget,
    pub kind: CapabilityKind,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub network: Option<NetworkAllowRule>,
}

impl AddonCapabilityRequest {
    pub fn host_api_read(target: ExecutionTarget, resource: &str) -> Self {
        Self {
            target,
            kind: CapabilityKind::HostApiRead,
            scope: Some(resource.to_string()),
            network: None,
        }
    }

    pub fn camera(target: ExecutionTarget) -> Self {
        Self::simple(target, CapabilityKind::CameraCapture)
    }

    pub fn microphone(target: ExecutionTarget) -> Self {
        Self::simple(target, CapabilityKind::MicrophoneCapture)
    }

    pub fn speaker(target: ExecutionTarget) -> Self {
        Self::simple(target, CapabilityKind::SpeakerOutput)
    }

    pub fn local_inference_with_model(target: ExecutionTarget, model_digest: &str) -> Self {
        Self {
            target,
            kind: CapabilityKind::LocalInference,
            scope: Some(model_digest.to_string()),
            network: None,
        }
    }

    pub fn storage(target: ExecutionTarget) -> Self {
        Self::simple(target, CapabilityKind::AddonStorage)
    }

    pub fn network(target: ExecutionTarget, network: NetworkAllowRule) -> Self {
        Self {
            target,
            kind: CapabilityKind::NetworkEgress,
            scope: None,
            network: Some(network),
        }
    }

    fn simple(target: ExecutionTarget, kind: CapabilityKind) -> Self {
        Self {
            target,
            kind,
            scope: None,
            network: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonExecutionRequest {
    pub targets: Vec<ExecutionTarget>,
    #[serde(default)]
    pub server: Option<AddonContainerSpec>,
    #[serde(default)]
    pub client: Option<AddonContainerSpec>,
    #[serde(default)]
    pub capabilities: Vec<AddonCapabilityRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityContractError {
    InvalidTargets,
    MissingContainerSpec(ExecutionTarget),
    UnexpectedContainerSpec(ExecutionTarget),
    InvalidContainerSpec(String),
    CapabilityTargetMismatch,
    DuplicateCapability,
    InvalidHostApiScope,
    InvalidModelDigest,
    MissingNetworkRule,
    UnboundedNetworkRule,
    InvalidNetworkRule,
}

impl AddonExecutionRequest {
    pub fn validate(&self) -> Result<(), CapabilityContractError> {
        if self.targets.is_empty() || self.targets.len() > 2 {
            return Err(CapabilityContractError::InvalidTargets);
        }
        let targets: BTreeSet<_> = self.targets.iter().copied().collect();
        if targets.len() != self.targets.len() {
            return Err(CapabilityContractError::InvalidTargets);
        }
        self.validate_container(ExecutionTarget::Server, &targets, self.server.as_ref())?;
        self.validate_container(ExecutionTarget::Client, &targets, self.client.as_ref())?;

        let mut seen = BTreeSet::new();
        for request in &self.capabilities {
            if !targets.contains(&request.target) {
                return Err(CapabilityContractError::CapabilityTargetMismatch);
            }
            if matches!(
                request.kind,
                CapabilityKind::CameraCapture
                    | CapabilityKind::MicrophoneCapture
                    | CapabilityKind::SpeakerOutput
                    | CapabilityKind::LocalInference
            ) && request.target != ExecutionTarget::Client
            {
                return Err(CapabilityContractError::CapabilityTargetMismatch);
            }
            let key = format!(
                "{:?}:{:?}:{}:{}",
                request.target,
                request.kind,
                request.scope.as_deref().unwrap_or_default(),
                request
                    .network
                    .as_ref()
                    .map(|rule| rule.name.as_str())
                    .unwrap_or_default()
            );
            if !seen.insert(key) {
                return Err(CapabilityContractError::DuplicateCapability);
            }
            self.validate_capability(request)?;
        }
        Ok(())
    }

    fn validate_container(
        &self,
        target: ExecutionTarget,
        targets: &BTreeSet<ExecutionTarget>,
        spec: Option<&AddonContainerSpec>,
    ) -> Result<(), CapabilityContractError> {
        if targets.contains(&target) != spec.is_some() {
            return if targets.contains(&target) {
                Err(CapabilityContractError::MissingContainerSpec(target))
            } else {
                Err(CapabilityContractError::UnexpectedContainerSpec(target))
            };
        }
        if let Some(spec) = spec {
            addons::validate_trusted_service_image_reference(&spec.image)
                .map_err(CapabilityContractError::InvalidContainerSpec)?;
            addons::validate_trusted_service_image_digest(&spec.image_digest)
                .map_err(CapabilityContractError::InvalidContainerSpec)?;
            if !(64 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&spec.memory_bytes)
                || !(1..=16_000).contains(&spec.cpu_millis)
                || !(1..=4_096).contains(&spec.pids)
                || spec.writable_bytes > 1024 * 1024 * 1024
            {
                return Err(CapabilityContractError::InvalidContainerSpec(
                    "container resource limits are outside the bounded range".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn validate_capability(
        &self,
        request: &AddonCapabilityRequest,
    ) -> Result<(), CapabilityContractError> {
        match request.kind {
            CapabilityKind::HostApiRead => {
                let scope = request
                    .scope
                    .as_deref()
                    .ok_or(CapabilityContractError::InvalidHostApiScope)?;
                if scope.is_empty()
                    || scope.len() > 64
                    || !scope.chars().all(|character| {
                        character.is_ascii_alphanumeric() || "._-".contains(character)
                    })
                {
                    return Err(CapabilityContractError::InvalidHostApiScope);
                }
                if request.network.is_some() {
                    return Err(CapabilityContractError::InvalidHostApiScope);
                }
            }
            CapabilityKind::NetworkEgress => {
                if request.scope.is_some() {
                    return Err(CapabilityContractError::InvalidNetworkRule);
                }
                let rule = request
                    .network
                    .as_ref()
                    .ok_or(CapabilityContractError::MissingNetworkRule)?;
                validate_network_rule(rule)?;
            }
            CapabilityKind::LocalInference => {
                let model_digest = request
                    .scope
                    .as_deref()
                    .ok_or(CapabilityContractError::InvalidModelDigest)?;
                if !is_sha256_digest(model_digest) || request.network.is_some() {
                    return Err(CapabilityContractError::InvalidModelDigest);
                }
            }
            _ => {
                if request.scope.is_some() || request.network.is_some() {
                    return Err(CapabilityContractError::CapabilityTargetMismatch);
                }
            }
        }
        Ok(())
    }
}

fn validate_network_rule(rule: &NetworkAllowRule) -> Result<(), CapabilityContractError> {
    if rule.name.is_empty()
        || rule.name.len() > 64
        || !rule
            .name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err(CapabilityContractError::InvalidNetworkRule);
    }
    if rule.ip.parse::<IpAddr>().is_err()
        || rule.ports.is_empty()
        || rule.ports.len() > 32
        || rule.ports.contains(&0)
        || rule.protocol == NetworkProtocol::Any
    {
        return if rule.protocol == NetworkProtocol::Any && rule.ports.is_empty() {
            Err(CapabilityContractError::UnboundedNetworkRule)
        } else {
            Err(CapabilityContractError::InvalidNetworkRule)
        };
    }
    let mut ports = BTreeSet::new();
    if rule.ports.iter().any(|port| !ports.insert(*port)) {
        return Err(CapabilityContractError::InvalidNetworkRule);
    }
    if let Some(mac) = rule.mac.as_deref() {
        let valid = mac.len() == 17
            && mac.chars().enumerate().all(|(index, character)| {
                if matches!(index, 2 | 5 | 8 | 11 | 14) {
                    character == ':'
                } else {
                    character.is_ascii_hexdigit()
                }
            });
        if !valid {
            return Err(CapabilityContractError::InvalidNetworkRule);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddonApprovalRecord {
    pub schema_version: u32,
    pub addon_id: String,
    pub addon_version: String,
    pub artifact_digest: String,
    pub request: AddonExecutionRequest,
    pub capability_approved: Vec<bool>,
    pub target_approved: Vec<bool>,
    pub state: ApprovalState,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub approved_at: Option<String>,
    #[serde(default)]
    pub revoked_by: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalError {
    InvalidAddonId,
    OciCapabilityBoundaryUnavailable(CapabilityKind),
    UnsupportedHostApiScope,
    InvalidHostApiResponse,
    HostApiResponseLimitExceeded,
    HostApiOperationMismatch,
    HostApiIdentityMismatch,
    StorageCapabilityUnavailable,
    InvalidStorageBoundary,
    CameraCapabilityUnavailable,
    InvalidCameraBoundary,
    MicrophoneCapabilityUnavailable,
    InvalidMicrophoneBoundary,
    SpeakerCapabilityUnavailable,
    InvalidSpeakerBoundary,
    LocalInferenceCapabilityUnavailable,
    InvalidLocalInferenceBoundary,
    ClientStorageCapabilityUnavailable,
    InvalidClientStorageBoundary,
    InvalidNetworkBoundary,
    InvalidActivationInstance,
    InvalidVersion,
    InvalidArtifactDigest,
    InvalidRequest(CapabilityContractError),
    CapabilityApprovalCountMismatch,
    TargetApprovalCountMismatch,
    InvalidCapabilityIndex,
    InvalidTarget,
    CapabilityNotApproved,
    TargetNotApproved,
    ApprovalBindingMismatch,
    InvalidApprover,
    InvalidTimestamp,
    ApprovalAlreadyFinalized,
    NotApproved,
}

impl AddonApprovalRecord {
    pub fn pending(
        addon_id: &str,
        addon_version: &str,
        artifact_digest: &str,
        request: AddonExecutionRequest,
    ) -> Result<Self, ApprovalError> {
        validate_approval_identity(addon_id, addon_version, artifact_digest)?;
        request.validate().map_err(ApprovalError::InvalidRequest)?;
        Ok(Self {
            schema_version: 2,
            addon_id: addon_id.to_string(),
            addon_version: addon_version.to_string(),
            artifact_digest: artifact_digest.to_string(),
            capability_approved: vec![false; request.capabilities.len()],
            target_approved: vec![false; request.targets.len()],
            request,
            state: ApprovalState::Pending,
            approved_by: None,
            approved_at: None,
            revoked_by: None,
            revoked_at: None,
        })
    }

    pub fn validate(&self) -> Result<(), ApprovalError> {
        if self.schema_version != 2 {
            return Err(ApprovalError::InvalidRequest(
                CapabilityContractError::InvalidTargets,
            ));
        }
        validate_approval_identity(&self.addon_id, &self.addon_version, &self.artifact_digest)?;
        self.request
            .validate()
            .map_err(ApprovalError::InvalidRequest)?;
        if self.capability_approved.len() != self.request.capabilities.len() {
            return Err(ApprovalError::CapabilityApprovalCountMismatch);
        }
        if self.target_approved.len() != self.request.targets.len() {
            return Err(ApprovalError::TargetApprovalCountMismatch);
        }
        if matches!(self.state, ApprovalState::Approved) {
            if self.approved_by.is_none() || self.approved_at.is_none() {
                return Err(ApprovalError::NotApproved);
            }
            if self.target_approved.iter().all(|approved| !approved) {
                return Err(ApprovalError::TargetNotApproved);
            }
        }
        Ok(())
    }

    pub fn set_capability_approved(
        &mut self,
        index: usize,
        approved: bool,
    ) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Pending {
            return Err(ApprovalError::ApprovalAlreadyFinalized);
        }
        let entry = self
            .capability_approved
            .get_mut(index)
            .ok_or(ApprovalError::InvalidCapabilityIndex)?;
        *entry = approved;
        Ok(())
    }

    pub fn set_target_approved(
        &mut self,
        target: ExecutionTarget,
        approved: bool,
    ) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Pending {
            return Err(ApprovalError::ApprovalAlreadyFinalized);
        }
        let index = self
            .request
            .targets
            .iter()
            .position(|candidate| *candidate == target)
            .ok_or(ApprovalError::InvalidTarget)?;
        self.target_approved[index] = approved;
        Ok(())
    }

    pub fn approve_target(
        &mut self,
        target: ExecutionTarget,
        approver: &str,
        timestamp: &str,
    ) -> Result<(), ApprovalError> {
        if self.state == ApprovalState::Revoked {
            return Err(ApprovalError::ApprovalAlreadyFinalized);
        }
        let index = self
            .request
            .targets
            .iter()
            .position(|candidate| *candidate == target)
            .ok_or(ApprovalError::InvalidTarget)?;
        if self
            .request
            .capabilities
            .iter()
            .enumerate()
            .any(|(capability_index, capability)| {
                capability.target == target && !self.capability_approved[capability_index]
            })
        {
            return Err(ApprovalError::CapabilityNotApproved);
        }
        validate_approver(approver)?;
        validate_timestamp(timestamp)?;
        self.target_approved[index] = true;
        self.approved_by = Some(approver.to_string());
        self.approved_at = Some(timestamp.to_string());
        self.state = ApprovalState::Approved;
        self.validate()
    }

    pub fn revoke_target(
        &mut self,
        target: ExecutionTarget,
        revoker: &str,
        timestamp: &str,
    ) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Approved {
            return Err(ApprovalError::NotApproved);
        }
        let index = self
            .request
            .targets
            .iter()
            .position(|candidate| *candidate == target)
            .ok_or(ApprovalError::InvalidTarget)?;
        validate_approver(revoker)?;
        validate_timestamp(timestamp)?;
        self.target_approved[index] = false;
        self.revoked_by = Some(revoker.to_string());
        self.revoked_at = Some(timestamp.to_string());
        if self.target_approved.iter().all(|approved| !approved) {
            self.state = ApprovalState::Revoked;
        }
        Ok(())
    }

    pub fn approve(&mut self, approver: &str, timestamp: &str) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Pending {
            return Err(ApprovalError::ApprovalAlreadyFinalized);
        }
        if self.capability_approved.iter().any(|approved| !approved) {
            return Err(ApprovalError::CapabilityNotApproved);
        }
        validate_approver(approver)?;
        validate_timestamp(timestamp)?;
        self.target_approved.fill(true);
        self.approved_by = Some(approver.to_string());
        self.approved_at = Some(timestamp.to_string());
        self.state = ApprovalState::Approved;
        self.validate()
    }

    pub fn revoke(&mut self, revoker: &str, timestamp: &str) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Approved {
            return Err(ApprovalError::NotApproved);
        }
        validate_approver(revoker)?;
        validate_timestamp(timestamp)?;
        self.state = ApprovalState::Revoked;
        self.target_approved.fill(false);
        self.revoked_by = Some(revoker.to_string());
        self.revoked_at = Some(timestamp.to_string());
        Ok(())
    }

    pub fn validate_binding(
        &self,
        addon_id: &str,
        addon_version: &str,
        artifact_digest: &str,
    ) -> Result<(), ApprovalError> {
        if self.addon_id != addon_id
            || self.addon_version != addon_version
            || self.artifact_digest != artifact_digest
        {
            return Err(ApprovalError::ApprovalBindingMismatch);
        }
        Ok(())
    }

    pub fn can_start(&self) -> Result<(), ApprovalError> {
        self.validate()?;
        if self.capability_approved.iter().any(|approved| !approved) {
            return Err(ApprovalError::CapabilityNotApproved);
        }
        if self.state != ApprovalState::Approved {
            return Err(ApprovalError::NotApproved);
        }
        if self.target_approved.iter().any(|approved| !approved) {
            return Err(ApprovalError::TargetNotApproved);
        }
        Ok(())
    }

    pub fn can_start_for_target(&self, target: ExecutionTarget) -> Result<(), ApprovalError> {
        self.validate()?;
        if self.state != ApprovalState::Approved {
            return Err(ApprovalError::NotApproved);
        }
        let target_index = self
            .request
            .targets
            .iter()
            .position(|candidate| *candidate == target)
            .ok_or(ApprovalError::InvalidTarget)?;
        if !self.target_approved[target_index] {
            return Err(ApprovalError::TargetNotApproved);
        }
        if self
            .request
            .capabilities
            .iter()
            .enumerate()
            .any(|(index, capability)| {
                capability.target == target && !self.capability_approved[index]
            })
        {
            return Err(ApprovalError::CapabilityNotApproved);
        }
        Ok(())
    }
}

/// Pure server-side admission output. Building this plan performs no IO,
/// image pull, process start, listener bind, or credential issuance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerExecutionPlan {
    pub addon_id: String,
    pub activation_instance: String,
    pub addon_version: String,
    pub artifact_digest: String,
    pub image: String,
    pub image_digest: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
    pub capabilities: Vec<AddonCapabilityRequest>,
    pub host_api_scopes: Vec<String>,
    pub network_allow_rules: Vec<NetworkAllowRule>,
}

impl ServerExecutionPlan {
    pub fn from_approved(
        record: &AddonApprovalRecord,
        activation_instance: &str,
    ) -> Result<Self, ApprovalError> {
        record.validate()?;
        addons::validate_addon_storage_namespace(activation_instance)
            .map_err(|_| ApprovalError::InvalidActivationInstance)?;
        if !record.request.targets.contains(&ExecutionTarget::Server) {
            return Err(ApprovalError::TargetNotApproved);
        }
        record.can_start_for_target(ExecutionTarget::Server)?;
        let container = record
            .request
            .server
            .as_ref()
            .ok_or(ApprovalError::InvalidRequest(
                CapabilityContractError::MissingContainerSpec(ExecutionTarget::Server),
            ))?;
        let capabilities = record
            .request
            .capabilities
            .iter()
            .filter(|capability| capability.target == ExecutionTarget::Server)
            .cloned()
            .collect::<Vec<_>>();
        let host_api_scopes = capabilities
            .iter()
            .filter_map(|capability| {
                (capability.kind == CapabilityKind::HostApiRead)
                    .then(|| capability.scope.clone())
                    .flatten()
            })
            .collect::<Vec<_>>();
        let network_allow_rules = capabilities
            .iter()
            .filter_map(|capability| capability.network.clone())
            .collect::<Vec<_>>();
        Ok(Self {
            addon_id: record.addon_id.clone(),
            activation_instance: activation_instance.to_string(),
            addon_version: record.addon_version.clone(),
            artifact_digest: record.artifact_digest.clone(),
            image: container.image.clone(),
            image_digest: container.image_digest.clone(),
            memory_bytes: container.memory_bytes,
            cpu_millis: container.cpu_millis,
            pids: container.pids,
            writable_bytes: container.writable_bytes,
            capabilities,
            host_api_scopes,
            network_allow_rules,
        })
    }
}

/// Pure client-side admission output. Building this plan performs no IO,
/// image pull, process start, device access, or client rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientExecutionPlan {
    pub addon_id: String,
    pub activation_instance: String,
    pub addon_version: String,
    pub artifact_digest: String,
    pub image: String,
    pub image_digest: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
    pub capabilities: Vec<AddonCapabilityRequest>,
    pub host_api_scopes: Vec<String>,
    pub network_allow_rules: Vec<NetworkAllowRule>,
}

impl ClientExecutionPlan {
    pub fn from_approved(
        record: &AddonApprovalRecord,
        activation_instance: &str,
    ) -> Result<Self, ApprovalError> {
        record.validate()?;
        addons::validate_addon_storage_namespace(activation_instance)
            .map_err(|_| ApprovalError::InvalidActivationInstance)?;
        if !record.request.targets.contains(&ExecutionTarget::Client) {
            return Err(ApprovalError::TargetNotApproved);
        }
        record.can_start_for_target(ExecutionTarget::Client)?;
        let container = record
            .request
            .client
            .as_ref()
            .ok_or(ApprovalError::InvalidRequest(
                CapabilityContractError::MissingContainerSpec(ExecutionTarget::Client),
            ))?;
        let capabilities = record
            .request
            .capabilities
            .iter()
            .filter(|capability| capability.target == ExecutionTarget::Client)
            .cloned()
            .collect::<Vec<_>>();
        let host_api_scopes = capabilities
            .iter()
            .filter_map(|capability| {
                (capability.kind == CapabilityKind::HostApiRead)
                    .then(|| capability.scope.clone())
                    .flatten()
            })
            .collect::<Vec<_>>();
        let network_allow_rules = capabilities
            .iter()
            .filter_map(|capability| capability.network.clone())
            .collect::<Vec<_>>();
        Ok(Self {
            addon_id: record.addon_id.clone(),
            activation_instance: activation_instance.to_string(),
            addon_version: record.addon_version.clone(),
            artifact_digest: record.artifact_digest.clone(),
            image: container.image.clone(),
            image_digest: container.image_digest.clone(),
            memory_bytes: container.memory_bytes,
            cpu_millis: container.cpu_millis,
            pids: container.pids,
            writable_bytes: container.writable_bytes,
            capabilities,
            host_api_scopes,
            network_allow_rules,
        })
    }
}

/// Contract-only OCI policy for a server add-on. It is never a launch handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOciLaunchSpec {
    pub addon_id: String,
    pub activation_instance: String,
    pub image_ref: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
    pub rootless: bool,
    pub non_root: bool,
    pub network_disabled: bool,
    pub read_only_rootfs: bool,
    pub no_new_privileges: bool,
    pub drop_all_capabilities: bool,
    pub seccomp_default: bool,
}

impl ServerOciLaunchSpec {
    pub fn from_server_plan(plan: &ServerExecutionPlan) -> Result<Self, ApprovalError> {
        if let Some(capability) = plan.capabilities.iter().find(|capability| {
            matches!(
                capability.kind,
                CapabilityKind::HostApiRead | CapabilityKind::NetworkEgress
            )
        }) {
            return Err(ApprovalError::OciCapabilityBoundaryUnavailable(
                capability.kind,
            ));
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            image_ref: format!("{}@{}", plan.image, plan.image_digest),
            memory_bytes: plan.memory_bytes,
            cpu_millis: plan.cpu_millis,
            pids: plan.pids,
            writable_bytes: plan.writable_bytes,
            rootless: true,
            non_root: true,
            network_disabled: true,
            read_only_rootfs: true,
            no_new_privileges: true,
            drop_all_capabilities: true,
            seccomp_default: true,
        })
    }
}

/// Contract-only OCI policy for a client add-on. It is never a launch handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientOciLaunchSpec {
    pub addon_id: String,
    pub activation_instance: String,
    pub image_ref: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
    pub rootless: bool,
    pub non_root: bool,
    pub network_disabled: bool,
    pub read_only_rootfs: bool,
    pub no_new_privileges: bool,
    pub drop_all_capabilities: bool,
    pub seccomp_default: bool,
}

impl ClientOciLaunchSpec {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if let Some(capability) = plan.capabilities.first() {
            return Err(ApprovalError::OciCapabilityBoundaryUnavailable(
                capability.kind,
            ));
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            image_ref: format!("{}@{}", plan.image, plan.image_digest),
            memory_bytes: plan.memory_bytes,
            cpu_millis: plan.cpu_millis,
            pids: plan.pids,
            writable_bytes: plan.writable_bytes,
            rootless: true,
            non_root: true,
            network_disabled: true,
            read_only_rootfs: true,
            no_new_privileges: true,
            drop_all_capabilities: true,
            seccomp_default: true,
        })
    }
}

/// Contract-only client camera stream boundary. It never opens a device or
/// returns captured bytes; a future broker must enforce this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCameraContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub opt_in: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCameraStreamRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl ClientCameraContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::CameraCapture)
        {
            return Err(ApprovalError::CameraCapabilityUnavailable);
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            opt_in: true,
            max_width: 1920,
            max_height: 1080,
            max_fps: 30,
        })
    }

    pub fn request(
        &self,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<ClientCameraStreamRequest, ApprovalError> {
        if !self.opt_in
            || width == 0
            || height == 0
            || fps == 0
            || width > self.max_width
            || height > self.max_height
            || fps > self.max_fps
        {
            return Err(ApprovalError::InvalidCameraBoundary);
        }
        Ok(ClientCameraStreamRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            width,
            height,
            fps,
        })
    }

    pub fn validate_request(
        &self,
        request: &ClientCameraStreamRequest,
    ) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidCameraBoundary);
        }
        self.request(request.width, request.height, request.fps)
            .map(|_| ())
    }
}

/// Contract-only client microphone stream boundary. Audio remains local to the
/// future client broker and cannot use a server command transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMicrophoneContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub opt_in: bool,
    pub server_command_transport: bool,
    pub max_sample_rate_hz: u32,
    pub max_channels: u8,
    pub max_frame_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMicrophoneStreamRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub frame_ms: u32,
}

impl ClientMicrophoneContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::MicrophoneCapture)
        {
            return Err(ApprovalError::MicrophoneCapabilityUnavailable);
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            opt_in: true,
            server_command_transport: false,
            max_sample_rate_hz: 48_000,
            max_channels: 2,
            max_frame_ms: 20,
        })
    }

    pub fn request(
        &self,
        sample_rate_hz: u32,
        channels: u8,
        frame_ms: u32,
    ) -> Result<ClientMicrophoneStreamRequest, ApprovalError> {
        if !self.opt_in
            || self.server_command_transport
            || sample_rate_hz == 0
            || channels == 0
            || frame_ms == 0
            || sample_rate_hz > self.max_sample_rate_hz
            || channels > self.max_channels
            || frame_ms > self.max_frame_ms
        {
            return Err(ApprovalError::InvalidMicrophoneBoundary);
        }
        Ok(ClientMicrophoneStreamRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            sample_rate_hz,
            channels,
            frame_ms,
        })
    }

    pub fn validate_request(
        &self,
        request: &ClientMicrophoneStreamRequest,
    ) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidMicrophoneBoundary);
        }
        self.request(request.sample_rate_hz, request.channels, request.frame_ms)
            .map(|_| ())
    }
}

/// Contract-only client speaker stream boundary. Audio output remains local to
/// the future client broker and cannot use a server command transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSpeakerContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub opt_in: bool,
    pub server_command_transport: bool,
    pub max_sample_rate_hz: u32,
    pub max_channels: u8,
    pub max_chunk_ms: u32,
    pub max_buffer_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSpeakerStreamRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub chunk_ms: u32,
}

impl ClientSpeakerContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::SpeakerOutput)
        {
            return Err(ApprovalError::SpeakerCapabilityUnavailable);
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            opt_in: true,
            server_command_transport: false,
            max_sample_rate_hz: 48_000,
            max_channels: 2,
            max_chunk_ms: 20,
            max_buffer_bytes: 64 * 1024,
        })
    }

    pub fn request(
        &self,
        sample_rate_hz: u32,
        channels: u8,
        chunk_ms: u32,
    ) -> Result<ClientSpeakerStreamRequest, ApprovalError> {
        if !self.opt_in
            || self.server_command_transport
            || sample_rate_hz == 0
            || channels == 0
            || chunk_ms == 0
            || sample_rate_hz > self.max_sample_rate_hz
            || channels > self.max_channels
            || chunk_ms > self.max_chunk_ms
        {
            return Err(ApprovalError::InvalidSpeakerBoundary);
        }
        Ok(ClientSpeakerStreamRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            sample_rate_hz,
            channels,
            chunk_ms,
        })
    }

    pub fn validate_request(
        &self,
        request: &ClientSpeakerStreamRequest,
    ) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidSpeakerBoundary);
        }
        self.request(request.sample_rate_hz, request.channels, request.chunk_ms)
            .map(|_| ())
    }
}

/// Contract-only client local-inference boundary. It carries bounded metadata
/// only; model loading, inference, and transport remain disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInferenceContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub opt_in: bool,
    pub local_only: bool,
    pub server_command_transport: bool,
    pub model_digest: String,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_output_tokens: u32,
    pub max_duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInferenceRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub model_digest: String,
    pub input_bytes: usize,
    pub max_output_tokens: u32,
    pub max_duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInferenceResult {
    pub addon_id: String,
    pub activation_instance: String,
    pub model_digest: String,
    pub output_bytes: usize,
    pub output_tokens: u32,
    pub duration_ms: u32,
}

impl ClientInferenceContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::LocalInference)
        {
            return Err(ApprovalError::LocalInferenceCapabilityUnavailable);
        }
        let model_digest = plan
            .capabilities
            .iter()
            .find(|capability| capability.kind == CapabilityKind::LocalInference)
            .and_then(|capability| capability.scope.clone())
            .filter(|digest| is_sha256_digest(digest))
            .ok_or(ApprovalError::InvalidLocalInferenceBoundary)?;
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            opt_in: true,
            local_only: true,
            server_command_transport: false,
            model_digest,
            memory_bytes: plan.memory_bytes,
            cpu_millis: plan.cpu_millis,
            pids: plan.pids,
            writable_bytes: plan.writable_bytes,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 64 * 1024,
            max_output_tokens: 2_048,
            max_duration_ms: 5_000,
        })
    }

    pub fn request(
        &self,
        model_digest: &str,
        input_bytes: usize,
        max_output_tokens: u32,
        max_duration_ms: u32,
    ) -> Result<ClientInferenceRequest, ApprovalError> {
        if !self.opt_in
            || !self.local_only
            || self.server_command_transport
            || model_digest != self.model_digest
            || !is_sha256_digest(model_digest)
            || input_bytes > self.max_input_bytes
            || max_output_tokens == 0
            || max_output_tokens > self.max_output_tokens
            || max_duration_ms == 0
            || max_duration_ms > self.max_duration_ms
        {
            return Err(ApprovalError::InvalidLocalInferenceBoundary);
        }
        Ok(ClientInferenceRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            model_digest: model_digest.to_string(),
            input_bytes,
            max_output_tokens,
            max_duration_ms,
        })
    }

    pub fn validate_request(&self, request: &ClientInferenceRequest) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidLocalInferenceBoundary);
        }
        self.request(
            &request.model_digest,
            request.input_bytes,
            request.max_output_tokens,
            request.max_duration_ms,
        )
        .map(|_| ())
    }

    pub fn result(
        &self,
        output_bytes: usize,
        output_tokens: u32,
        duration_ms: u32,
    ) -> Result<ClientInferenceResult, ApprovalError> {
        if !self.opt_in
            || !self.local_only
            || self.server_command_transport
            || output_bytes > self.max_output_bytes
            || output_tokens > self.max_output_tokens
            || duration_ms == 0
            || duration_ms > self.max_duration_ms
        {
            return Err(ApprovalError::InvalidLocalInferenceBoundary);
        }
        Ok(ClientInferenceResult {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            model_digest: self.model_digest.clone(),
            output_bytes,
            output_tokens,
            duration_ms,
        })
    }

    pub fn validate_result(&self, result: &ClientInferenceResult) -> Result<(), ApprovalError> {
        if result.addon_id != self.addon_id
            || result.activation_instance != self.activation_instance
            || result.model_digest != self.model_digest
        {
            return Err(ApprovalError::InvalidLocalInferenceBoundary);
        }
        self.result(
            result.output_bytes,
            result.output_tokens,
            result.duration_ms,
        )
        .map(|_| ())
    }
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

/// Contract-only client private key/value storage boundary. It exposes no path,
/// file handle, database handle, SQL, or server transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPrivateStorageContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub private_local: bool,
    pub server_command_transport: bool,
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
    pub max_list_items: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientStorageOperation {
    Get { key: String },
    Put { key: String, value_bytes: usize },
    Delete { key: String },
    List { prefix: String, limit: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientStorageResult {
    Get { found: bool, value_bytes: usize },
    Put { stored_bytes: usize },
    Delete { removed: bool },
    List { item_count: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStorageResponse {
    pub addon_id: String,
    pub activation_instance: String,
    pub result: ClientStorageResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStorageRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub operation: ClientStorageOperation,
}

impl ClientPrivateStorageContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::AddonStorage)
        {
            return Err(ApprovalError::ClientStorageCapabilityUnavailable);
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            private_local: true,
            server_command_transport: false,
            max_key_bytes: 128,
            max_value_bytes: 64 * 1024,
            max_list_items: 1_000,
        })
    }

    pub fn request(&self, operation: ClientStorageOperation) -> ClientStorageRequest {
        ClientStorageRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            operation,
        }
    }

    pub fn response(
        &self,
        result: ClientStorageResult,
    ) -> Result<ClientStorageResponse, ApprovalError> {
        match &result {
            ClientStorageResult::Get { value_bytes, .. }
            | ClientStorageResult::Put {
                stored_bytes: value_bytes,
            } => {
                if *value_bytes > self.max_value_bytes {
                    return Err(ApprovalError::InvalidClientStorageBoundary);
                }
            }
            ClientStorageResult::Delete { .. } => {}
            ClientStorageResult::List { item_count } => {
                if *item_count > self.max_list_items {
                    return Err(ApprovalError::InvalidClientStorageBoundary);
                }
            }
        }
        Ok(ClientStorageResponse {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            result,
        })
    }

    pub fn validate_response(&self, response: &ClientStorageResponse) -> Result<(), ApprovalError> {
        if response.addon_id != self.addon_id
            || response.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidClientStorageBoundary);
        }
        self.response(response.result.clone()).map(|_| ())
    }

    pub fn validate_response_for_request(
        &self,
        request: &ClientStorageRequest,
        response: &ClientStorageResponse,
    ) -> Result<(), ApprovalError> {
        self.validate_request(request)?;
        let operation_matches = matches!(
            (&request.operation, &response.result),
            (
                ClientStorageOperation::Get { .. },
                ClientStorageResult::Get { .. }
            ) | (
                ClientStorageOperation::Put { .. },
                ClientStorageResult::Put { .. }
            ) | (
                ClientStorageOperation::Delete { .. },
                ClientStorageResult::Delete { .. }
            ) | (
                ClientStorageOperation::List { .. },
                ClientStorageResult::List { .. }
            )
        );
        if !operation_matches {
            return Err(ApprovalError::InvalidClientStorageBoundary);
        }
        self.validate_response(response)
    }

    pub fn validate_request(&self, request: &ClientStorageRequest) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
            || !self.private_local
            || self.server_command_transport
        {
            return Err(ApprovalError::InvalidClientStorageBoundary);
        }
        match &request.operation {
            ClientStorageOperation::Get { key } | ClientStorageOperation::Delete { key } => {
                validate_client_storage_key(key, self.max_key_bytes)?;
            }
            ClientStorageOperation::Put { key, value_bytes } => {
                validate_client_storage_key(key, self.max_key_bytes)?;
                if *value_bytes > self.max_value_bytes {
                    return Err(ApprovalError::InvalidClientStorageBoundary);
                }
            }
            ClientStorageOperation::List { prefix, limit } => {
                if !prefix.is_empty() {
                    validate_client_storage_key(prefix, self.max_key_bytes)?;
                }
                if *limit == 0 || *limit > self.max_list_items {
                    return Err(ApprovalError::InvalidClientStorageBoundary);
                }
            }
        }
        Ok(())
    }
}

fn validate_client_storage_key(value: &str, maximum_bytes: usize) -> Result<(), ApprovalError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(ApprovalError::InvalidClientStorageBoundary);
    }
    Ok(())
}

/// Contract-only network boundary. It performs no socket, route, or firewall changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerNetworkBoundary {
    pub addon_id: String,
    pub activation_instance: String,
    pub deny_by_default: bool,
    pub rules: Vec<NetworkAllowRule>,
}

impl ServerNetworkBoundary {
    pub fn from_server_plan(plan: &ServerExecutionPlan) -> Result<Self, ApprovalError> {
        for rule in &plan.network_allow_rules {
            validate_network_rule(rule).map_err(|_| ApprovalError::InvalidNetworkBoundary)?;
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            deny_by_default: true,
            rules: plan.network_allow_rules.clone(),
        })
    }

    pub fn allows(
        &self,
        ip: IpAddr,
        protocol: NetworkProtocol,
        port: u16,
        mac: Option<&str>,
    ) -> bool {
        if !self.deny_by_default {
            return false;
        }
        self.rules.iter().any(|rule| {
            rule.ip.parse::<IpAddr>().ok() == Some(ip)
                && rule.protocol == protocol
                && rule.ports.contains(&port)
                && match rule.mac.as_deref() {
                    Some(expected) => mac == Some(expected),
                    None => true,
                }
        })
    }
}

/// Contract-only client network boundary. It performs no socket, route, or
/// firewall changes and remains unavailable to a future launcher until enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientNetworkBoundary {
    pub addon_id: String,
    pub activation_instance: String,
    pub deny_by_default: bool,
    pub rules: Vec<NetworkAllowRule>,
}

impl ClientNetworkBoundary {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        for rule in &plan.network_allow_rules {
            validate_network_rule(rule).map_err(|_| ApprovalError::InvalidNetworkBoundary)?;
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            deny_by_default: true,
            rules: plan.network_allow_rules.clone(),
        })
    }

    pub fn allows(
        &self,
        ip: IpAddr,
        protocol: NetworkProtocol,
        port: u16,
        mac: Option<&str>,
    ) -> bool {
        if !self.deny_by_default {
            return false;
        }
        self.rules.iter().any(|rule| {
            rule.ip.parse::<IpAddr>().ok() == Some(ip)
                && rule.protocol == protocol
                && rule.ports.contains(&port)
                && mac == rule.mac.as_deref()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientNetworkRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub device_name: String,
    pub ip: String,
    pub mac: Option<String>,
    pub protocol: NetworkProtocol,
    pub port: u16,
}

impl ClientNetworkBoundary {
    pub fn request(
        &self,
        device_name: &str,
        ip: &str,
        protocol: NetworkProtocol,
        port: u16,
        mac: Option<&str>,
    ) -> Result<ClientNetworkRequest, ApprovalError> {
        let ip = ip
            .parse::<IpAddr>()
            .map_err(|_| ApprovalError::InvalidNetworkBoundary)?;
        if !self.allows_device(device_name, ip, protocol, port, mac) {
            return Err(ApprovalError::InvalidNetworkBoundary);
        }
        Ok(ClientNetworkRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            device_name: device_name.to_string(),
            ip: ip.to_string(),
            mac: mac.map(str::to_string),
            protocol,
            port,
        })
    }

    pub fn validate_request(&self, request: &ClientNetworkRequest) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::InvalidNetworkBoundary);
        }
        let ip = request
            .ip
            .parse::<IpAddr>()
            .map_err(|_| ApprovalError::InvalidNetworkBoundary)?;
        if self.allows_device(
            &request.device_name,
            ip,
            request.protocol,
            request.port,
            request.mac.as_deref(),
        ) {
            Ok(())
        } else {
            Err(ApprovalError::InvalidNetworkBoundary)
        }
    }

    fn allows_device(
        &self,
        device_name: &str,
        ip: IpAddr,
        protocol: NetworkProtocol,
        port: u16,
        mac: Option<&str>,
    ) -> bool {
        if !self.deny_by_default {
            return false;
        }
        self.rules.iter().any(|rule| {
            rule.name == device_name
                && rule.ip.parse::<IpAddr>().ok() == Some(ip)
                && rule.protocol == protocol
                && rule.ports.contains(&port)
                && mac == rule.mac.as_deref()
        })
    }
}

/// Contract-only private storage boundary for an approved server add-on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStorageContract {
    pub database: TrustedServiceDatabaseContract,
}

impl ServerStorageContract {
    pub fn from_server_plan(plan: &ServerExecutionPlan) -> Result<Self, ApprovalError> {
        if !plan
            .capabilities
            .iter()
            .any(|capability| capability.kind == CapabilityKind::AddonStorage)
        {
            return Err(ApprovalError::StorageCapabilityUnavailable);
        }
        let database = TrustedServiceDatabaseContract::core_storage_api(
            &plan.addon_id,
            &plan.activation_instance,
        );
        database
            .validate()
            .map_err(|_| ApprovalError::InvalidStorageBoundary)?;
        Ok(Self { database })
    }

    pub fn request(&self, operation: DatabaseOperation) -> DatabaseRequest {
        DatabaseRequest {
            addon_id: self.database.addon_id.clone(),
            activation_instance: self.database.activation_instance.clone(),
            operation,
        }
    }

    pub fn validate_request(&self, request: &DatabaseRequest) -> Result<(), ApprovalError> {
        request
            .validate_against(&self.database)
            .map_err(|_| ApprovalError::InvalidStorageBoundary)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientHostApiOperation {
    GetClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientHostApiRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub operation: ClientHostApiOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientHostApiResponse {
    ClientInfo {
        client_id: String,
        name: String,
        version: String,
        display_count: u32,
    },
}

impl ClientHostApiResponse {
    pub fn client_info(
        client_id: &str,
        name: &str,
        version: &str,
        display_count: u32,
    ) -> Result<Self, ApprovalError> {
        validate_host_api_text(client_id, 128)?;
        validate_host_api_text(name, 160)?;
        validate_host_api_text(version, 64)?;
        if display_count == 0 || display_count > 16 {
            return Err(ApprovalError::InvalidHostApiResponse);
        }
        Ok(Self::ClientInfo {
            client_id: client_id.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            display_count,
        })
    }
}

/// Typed, read-only host API boundary for approved client add-ons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHostApiContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub operations: Vec<ClientHostApiOperation>,
    pub read_only: bool,
    pub allow_database: bool,
    pub allow_credentials: bool,
    pub allow_os_control: bool,
    pub max_response_bytes: usize,
}

impl ClientHostApiContract {
    pub fn from_client_plan(plan: &ClientExecutionPlan) -> Result<Self, ApprovalError> {
        let mut operations = Vec::new();
        for scope in &plan.host_api_scopes {
            match scope.as_str() {
                "client_info" => {
                    if !operations.contains(&ClientHostApiOperation::GetClientInfo) {
                        operations.push(ClientHostApiOperation::GetClientInfo);
                    }
                }
                _ => return Err(ApprovalError::UnsupportedHostApiScope),
            }
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            operations,
            read_only: true,
            allow_database: false,
            allow_credentials: false,
            allow_os_control: false,
            max_response_bytes: 16 * 1024,
        })
    }

    pub fn request(
        &self,
        operation: ClientHostApiOperation,
    ) -> Result<ClientHostApiRequest, ApprovalError> {
        if !self.operations.contains(&operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        Ok(ClientHostApiRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            operation,
        })
    }

    pub fn validate_request(&self, request: &ClientHostApiRequest) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::HostApiIdentityMismatch);
        }
        if !self.operations.contains(&request.operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        Ok(())
    }

    pub fn validate_response(
        &self,
        operation: ClientHostApiOperation,
        response: &ClientHostApiResponse,
    ) -> Result<(), ApprovalError> {
        if !self.operations.contains(&operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        if operation != ClientHostApiOperation::GetClientInfo
            || !matches!(response, ClientHostApiResponse::ClientInfo { .. })
        {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        let encoded =
            serde_json::to_vec(response).map_err(|_| ApprovalError::InvalidHostApiResponse)?;
        if encoded.len() > self.max_response_bytes {
            return Err(ApprovalError::HostApiResponseLimitExceeded);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerHostApiOperation {
    ListClients,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerHostApiRequest {
    pub addon_id: String,
    pub activation_instance: String,
    pub operation: ServerHostApiOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerClientSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub version: Option<String>,
    pub last_seen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerHostApiResponse {
    Clients { items: Vec<ServerClientSummary> },
}

impl ServerHostApiResponse {
    pub fn clients(items: Vec<ServerClientSummary>) -> Result<Self, ApprovalError> {
        for item in &items {
            validate_host_api_text(&item.id, 128)?;
            validate_host_api_text(&item.name, 160)?;
            validate_host_api_text(&item.status, 64)?;
            if let Some(version) = &item.version {
                validate_host_api_text(version, 64)?;
            }
            validate_host_api_text(&item.last_seen, 64)?;
        }
        Ok(Self::Clients { items })
    }
}

fn validate_host_api_text(value: &str, maximum_bytes: usize) -> Result<(), ApprovalError> {
    if value.is_empty() || value.len() > maximum_bytes || value.chars().any(char::is_control) {
        return Err(ApprovalError::InvalidHostApiResponse);
    }
    Ok(())
}

/// Typed, read-only host API boundary for approved server add-ons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHostApiContract {
    pub addon_id: String,
    pub activation_instance: String,
    pub operations: Vec<ServerHostApiOperation>,
    pub read_only: bool,
    pub allow_database: bool,
    pub allow_credentials: bool,
    pub allow_os_control: bool,
    pub max_items: u32,
    pub max_response_bytes: usize,
}

impl ServerHostApiContract {
    pub fn from_server_plan(plan: &ServerExecutionPlan) -> Result<Self, ApprovalError> {
        let mut operations = Vec::new();
        for scope in &plan.host_api_scopes {
            match scope.as_str() {
                "clients" => {
                    if !operations.contains(&ServerHostApiOperation::ListClients) {
                        operations.push(ServerHostApiOperation::ListClients);
                    }
                }
                _ => return Err(ApprovalError::UnsupportedHostApiScope),
            }
        }
        Ok(Self {
            addon_id: plan.addon_id.clone(),
            activation_instance: plan.activation_instance.clone(),
            operations,
            read_only: true,
            allow_database: false,
            allow_credentials: false,
            allow_os_control: false,
            max_items: 100,
            max_response_bytes: 64 * 1024,
        })
    }

    pub fn request(
        &self,
        operation: ServerHostApiOperation,
    ) -> Result<ServerHostApiRequest, ApprovalError> {
        if !self.operations.contains(&operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        Ok(ServerHostApiRequest {
            addon_id: self.addon_id.clone(),
            activation_instance: self.activation_instance.clone(),
            operation,
        })
    }

    pub fn validate_request(&self, request: &ServerHostApiRequest) -> Result<(), ApprovalError> {
        if request.addon_id != self.addon_id
            || request.activation_instance != self.activation_instance
        {
            return Err(ApprovalError::HostApiIdentityMismatch);
        }
        if !self.operations.contains(&request.operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        Ok(())
    }

    pub fn validate_response(
        &self,
        operation: ServerHostApiOperation,
        response: &ServerHostApiResponse,
    ) -> Result<(), ApprovalError> {
        if !self.operations.contains(&operation) {
            return Err(ApprovalError::HostApiOperationMismatch);
        }
        match (operation, response) {
            (ServerHostApiOperation::ListClients, ServerHostApiResponse::Clients { items }) => {
                if items.len() > self.max_items as usize {
                    return Err(ApprovalError::HostApiResponseLimitExceeded);
                }
                let encoded = serde_json::to_vec(response)
                    .map_err(|_| ApprovalError::InvalidHostApiResponse)?;
                if encoded.len() > self.max_response_bytes {
                    return Err(ApprovalError::HostApiResponseLimitExceeded);
                }
                Ok(())
            }
        }
    }
}

fn validate_approval_identity(
    addon_id: &str,
    addon_version: &str,
    artifact_digest: &str,
) -> Result<(), ApprovalError> {
    addons::validate_addon_storage_namespace(addon_id)
        .map_err(|_| ApprovalError::InvalidAddonId)?;
    if addon_version.is_empty()
        || addon_version.len() > 64
        || !addon_version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err(ApprovalError::InvalidVersion);
    }
    if artifact_digest.len() != "sha256:".len() + 64
        || !artifact_digest.starts_with("sha256:")
        || !artifact_digest["sha256:".len()..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(ApprovalError::InvalidArtifactDigest);
    }
    Ok(())
}

fn validate_approver(value: &str) -> Result<(), ApprovalError> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(|character| character.is_control())
    {
        return Err(ApprovalError::InvalidApprover);
    }
    Ok(())
}

fn validate_timestamp(value: &str) -> Result<(), ApprovalError> {
    let bytes = value.as_bytes();
    let valid_shape = value.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z';
    if !valid_shape
        || !value.chars().enumerate().all(|(index, character)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || character.is_ascii_digit()
        })
    {
        return Err(ApprovalError::InvalidTimestamp);
    }
    Ok(())
}

const APPROVAL_ROOT: &str = "addon-approvals";
const APPROVAL_FILE_SUFFIX: &str = ".approval.json";
const MAX_APPROVAL_BYTES: u64 = 256 * 1024;

pub struct AddonApprovalStore;

impl AddonApprovalStore {
    pub fn save(record: &AddonApprovalRecord) -> Result<(), String> {
        record
            .validate()
            .map_err(|error| format!("Invalid approval record: {error:?}"))?;
        let (root, root_metadata) = checked_approval_root(true)?;
        let path = approval_path(&root, &record.addon_id);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err("Invalid approval record file".to_string());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{MetadataExt, PermissionsExt};
                    if metadata.nlink() != 1 || metadata.permissions().mode() & 0o777 != 0o600 {
                        return Err("Approval record permissions are insecure".to_string());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Failed to inspect approval record: {error}")),
        }
        let temp = root.join(format!(".approval-{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|_| "Failed to encode approval record".to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("Failed to create approval staging file: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&temp);
                return Err(format!("Failed to protect approval staging file: {error}"));
            }
        }
        let result = (|| {
            file.write_all(&bytes)
                .map_err(|error| format!("Failed to write approval record: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Failed to sync approval record: {error}"))?;
            validate_approval_root_identity(&root, &root_metadata)?;
            fs::rename(&temp, &path)
                .map_err(|error| format!("Failed to publish approval record: {error}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub fn load(addon_id: &str) -> Result<AddonApprovalRecord, String> {
        addons::validate_addon_storage_namespace(addon_id)
            .map_err(|_| "Invalid approval addon ID".to_string())?;
        let (root, _) = checked_approval_root(false)?;
        let path = approval_path(&root, addon_id);
        let metadata = validate_existing_approval_file(&path)?;
        if metadata.len() > MAX_APPROVAL_BYTES {
            return Err("Approval record is too large".to_string());
        }
        let mut file = fs::File::open(&path)
            .map_err(|error| format!("Failed to read approval record: {error}"))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("Failed to read approval record: {error}"))?;
        let record: AddonApprovalRecord =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid approval record".to_string())?;
        record
            .validate()
            .map_err(|error| format!("Invalid approval record: {error:?}"))?;
        if record.addon_id != addon_id {
            return Err("Approval record identity mismatch".to_string());
        }
        record
            .validate_binding(
                &record.addon_id,
                &record.addon_version,
                &record.artifact_digest,
            )
            .map_err(|error| format!("Invalid approval binding: {error:?}"))?;
        Ok(record)
    }

    pub fn list() -> Result<Vec<AddonApprovalRecord>, String> {
        let (root, _) = match checked_approval_root(false) {
            Ok(value) => value,
            Err(error) if error == "Approval record not found" => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in fs::read_dir(&root)
            .map_err(|error| format!("Failed to list approval records: {error}"))?
        {
            if records.len() >= 256 {
                return Err("Too many approval records".to_string());
            }
            let entry =
                entry.map_err(|error| format!("Failed to inspect approval record: {error}"))?;
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Invalid approval record filename".to_string())?;
            if !name.ends_with(APPROVAL_FILE_SUFFIX) {
                return Err("Unexpected approval directory entry".to_string());
            }
            let addon_id = name
                .strip_suffix(APPROVAL_FILE_SUFFIX)
                .ok_or_else(|| "Invalid approval record filename".to_string())?;
            addons::validate_addon_storage_namespace(addon_id)
                .map_err(|_| "Invalid approval record filename".to_string())?;
            let record = Self::load(addon_id)?;
            if record.addon_id != addon_id || path != approval_path(&root, addon_id) {
                return Err("Approval record identity mismatch".to_string());
            }
            records.push(record);
        }
        records.sort_by(|left, right| left.addon_id.cmp(&right.addon_id));
        Ok(records)
    }
}
fn checked_approval_root(create: bool) -> Result<(PathBuf, fs::Metadata), String> {
    let data_root = addons::checked_addon_data_root(create)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Addon data directory is unavailable".to_string())?;
    let root = data_root.join(APPROVAL_ROOT);
    match fs::symlink_metadata(&root) {
        Ok(metadata) => {
            validate_private_approval_root(&metadata)?;
            Ok((root, metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir(&root)
                .map_err(|error| format!("Failed to create approval directory: {error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("Failed to protect approval directory: {error}"))?;
            }
            let metadata = fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
            validate_private_approval_root(&metadata)?;
            Ok((root, metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("Approval record not found".to_string())
        }
        Err(error) => Err(format!("Failed to inspect approval directory: {error}")),
    }
}

fn validate_private_approval_root(metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Invalid approval directory".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err("Approval directory permissions are insecure".to_string());
        }
    }
    Ok(())
}

fn validate_approval_root_identity(root: &Path, expected: &fs::Metadata) -> Result<(), String> {
    let current = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    validate_private_approval_root(&current)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (current.dev(), current.ino()) != (expected.dev(), expected.ino()) {
            return Err("Approval directory changed".to_string());
        }
    }
    Ok(())
}

fn approval_path(root: &Path, addon_id: &str) -> PathBuf {
    root.join(format!("{addon_id}{APPROVAL_FILE_SUFFIX}"))
}

fn validate_existing_approval_file(path: &Path) -> Result<fs::Metadata, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err("Invalid approval record file".to_string());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                if metadata.nlink() != 1 || metadata.permissions().mode() & 0o777 != 0o600 {
                    return Err("Approval record permissions are insecure".to_string());
                }
            }
            Ok(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("Approval record not found".to_string())
        }
        Err(error) => Err(format!("Failed to inspect approval record: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_request_accepts_server_client_and_dual_targets_with_scoped_access() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server, ExecutionTarget::Client],
            server: Some(container("registry.example/server", 'a')),
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![
                AddonCapabilityRequest::host_api_read(ExecutionTarget::Server, "clients"),
                AddonCapabilityRequest::camera(ExecutionTarget::Client),
                AddonCapabilityRequest::microphone(ExecutionTarget::Client),
                AddonCapabilityRequest::speaker(ExecutionTarget::Client),
                AddonCapabilityRequest::network(
                    ExecutionTarget::Client,
                    NetworkAllowRule {
                        name: "smart-device".to_string(),
                        ip: "192.168.1.40".to_string(),
                        mac: Some("AA:BB:CC:DD:EE:FF".to_string()),
                        protocol: NetworkProtocol::Tcp,
                        ports: vec![443, 8443],
                    },
                ),
            ],
        };

        assert_eq!(request.validate(), Ok(()));
    }

    #[test]
    fn execution_request_rejects_unscoped_or_wrong_target_capabilities() {
        let mut request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::camera(ExecutionTarget::Server)],
        };

        assert_eq!(
            request.validate(),
            Err(CapabilityContractError::CapabilityTargetMismatch)
        );

        request.capabilities = vec![AddonCapabilityRequest::network(
            ExecutionTarget::Server,
            NetworkAllowRule {
                name: "unrestricted".to_string(),
                ip: "0.0.0.0/0".to_string(),
                mac: None,
                protocol: NetworkProtocol::Any,
                ports: vec![],
            },
        )];
        assert_eq!(
            request.validate(),
            Err(CapabilityContractError::UnboundedNetworkRule)
        );
    }

    #[test]
    fn execution_request_requires_matching_container_specs_and_rejects_duplicates() {
        let mut request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: None,
            capabilities: vec![
                AddonCapabilityRequest::speaker(ExecutionTarget::Client),
                AddonCapabilityRequest::speaker(ExecutionTarget::Client),
            ],
        };

        assert_eq!(
            request.validate(),
            Err(CapabilityContractError::MissingContainerSpec(
                ExecutionTarget::Client
            ))
        );

        request.client = Some(container("registry.example/client", 'b'));
        assert_eq!(
            request.validate(),
            Err(CapabilityContractError::DuplicateCapability)
        );
    }

    #[test]
    fn approved_server_plan_contains_only_server_scoped_capabilities() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server, ExecutionTarget::Client],
            server: Some(container("registry.example/server", 'a')),
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![
                AddonCapabilityRequest::host_api_read(ExecutionTarget::Server, "clients"),
                AddonCapabilityRequest::network(
                    ExecutionTarget::Server,
                    NetworkAllowRule {
                        name: "weather-api".to_string(),
                        ip: "192.0.2.10".to_string(),
                        mac: None,
                        protocol: NetworkProtocol::Tcp,
                        ports: vec![443],
                    },
                ),
                AddonCapabilityRequest::camera(ExecutionTarget::Client),
            ],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.dual-plan",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.set_capability_approved(1, true).unwrap();
        approval
            .approve_target(ExecutionTarget::Server, "admin", "2026-10-20T14:30:00Z")
            .unwrap();

        let plan =
            ServerExecutionPlan::from_approved(&approval, "example-dual-plan-activation").unwrap();
        assert_eq!(plan.addon_id, "example.dual-plan");
        assert_eq!(plan.activation_instance, "example-dual-plan-activation");
        assert_eq!(plan.image, "registry.example/server");
        assert_eq!(plan.image_digest, format!("sha256:{}", "a".repeat(64)));
        assert_eq!(plan.capabilities.len(), 2);
        assert!(plan
            .capabilities
            .iter()
            .all(|capability| capability.target == ExecutionTarget::Server));
        assert_eq!(plan.network_allow_rules.len(), 1);
        assert_eq!(plan.host_api_scopes, vec!["clients".to_string()]);
        assert_eq!(
            ServerExecutionPlan::from_approved(
                &AddonApprovalRecord {
                    target_approved: vec![false, false],
                    ..approval
                },
                "example-dual-plan-activation",
            ),
            Err(ApprovalError::TargetNotApproved)
        );
    }

    #[test]
    fn server_plan_rejects_invalid_activation_identity() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.server-plan",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval
            .approve_target(ExecutionTarget::Server, "admin", "2026-10-20T14:30:00Z")
            .unwrap();

        assert_eq!(
            ServerExecutionPlan::from_approved(&approval, "../escape"),
            Err(ApprovalError::InvalidActivationInstance)
        );
    }

    #[test]
    fn server_plan_rejects_client_only_approval() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::camera(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-only",
            "1.0.0",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval
            .approve_target(ExecutionTarget::Client, "admin", "2026-10-20T14:30:00Z")
            .unwrap();

        assert_eq!(
            ServerExecutionPlan::from_approved(&approval, "example-client-only-activation"),
            Err(ApprovalError::TargetNotApproved)
        );
    }

    #[test]
    fn server_and_client_targets_are_approved_and_revoked_independently() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server, ExecutionTarget::Client],
            server: Some(container("registry.example/server", 'a')),
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![
                AddonCapabilityRequest::host_api_read(ExecutionTarget::Server, "clients"),
                AddonCapabilityRequest::microphone(ExecutionTarget::Client),
            ],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.dual",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.set_capability_approved(1, true).unwrap();

        approval
            .approve_target(ExecutionTarget::Server, "admin", "2026-10-20T14:30:00Z")
            .unwrap();
        assert_eq!(
            approval.can_start_for_target(ExecutionTarget::Server),
            Ok(())
        );
        assert_eq!(
            approval.can_start_for_target(ExecutionTarget::Client),
            Err(ApprovalError::TargetNotApproved)
        );

        approval
            .approve_target(ExecutionTarget::Client, "admin", "2026-10-20T14:31:00Z")
            .unwrap();
        assert_eq!(approval.can_start(), Ok(()));

        approval
            .revoke_target(ExecutionTarget::Server, "admin", "2026-10-20T14:32:00Z")
            .unwrap();
        assert_eq!(
            approval.can_start_for_target(ExecutionTarget::Server),
            Err(ApprovalError::TargetNotApproved)
        );
        assert_eq!(
            approval.can_start_for_target(ExecutionTarget::Client),
            Ok(())
        );
    }

    #[test]
    fn approval_requires_every_requested_capability_before_start_is_allowed() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![
                AddonCapabilityRequest::microphone(ExecutionTarget::Client),
                AddonCapabilityRequest::speaker(ExecutionTarget::Client),
            ],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.local-ai",
            "1.0.0",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            request,
        )
        .unwrap();

        approval.set_capability_approved(0, true).unwrap();
        assert_eq!(
            approval.can_start(),
            Err(ApprovalError::CapabilityNotApproved)
        );
        approval.set_capability_approved(1, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        assert_eq!(approval.can_start(), Ok(()));
    }

    #[test]
    fn client_camera_contract_is_opt_in_bounded_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::camera(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.camera-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "camera-contract-1").unwrap();
        let contract = ClientCameraContract::from_client_plan(&plan).unwrap();

        assert!(contract.opt_in);
        assert_eq!(contract.max_width, 1920);
        assert_eq!(contract.max_height, 1080);
        assert_eq!(contract.max_fps, 30);
        let stream = contract.request(1280, 720, 30).unwrap();
        assert_eq!(contract.validate_request(&stream), Ok(()));
        assert_eq!(stream.addon_id, "example.camera-contract");
        assert_eq!(stream.activation_instance, "camera-contract-1");

        let mut mismatched = stream.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidCameraBoundary)
        );
        assert_eq!(
            contract.request(3840, 2160, 60),
            Err(ApprovalError::InvalidCameraBoundary)
        );
    }

    #[test]
    fn client_microphone_contract_is_local_bounded_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::microphone(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.microphone-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "microphone-contract-1").unwrap();
        let contract = ClientMicrophoneContract::from_client_plan(&plan).unwrap();

        assert!(contract.opt_in);
        assert!(!contract.server_command_transport);
        assert_eq!(contract.max_sample_rate_hz, 48_000);
        assert_eq!(contract.max_channels, 2);
        assert_eq!(contract.max_frame_ms, 20);
        let stream = contract.request(16_000, 1, 20).unwrap();
        assert_eq!(contract.validate_request(&stream), Ok(()));
        assert_eq!(stream.addon_id, "example.microphone-contract");
        assert_eq!(stream.activation_instance, "microphone-contract-1");

        let mut mismatched = stream.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidMicrophoneBoundary)
        );
        assert_eq!(
            contract.request(96_000, 2, 20),
            Err(ApprovalError::InvalidMicrophoneBoundary)
        );
    }

    #[test]
    fn client_speaker_contract_is_local_bounded_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::speaker(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.speaker-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "speaker-contract-1").unwrap();
        let contract = ClientSpeakerContract::from_client_plan(&plan).unwrap();

        assert!(contract.opt_in);
        assert!(!contract.server_command_transport);
        assert_eq!(contract.max_sample_rate_hz, 48_000);
        assert_eq!(contract.max_channels, 2);
        assert_eq!(contract.max_chunk_ms, 20);
        assert_eq!(contract.max_buffer_bytes, 64 * 1024);
        let stream = contract.request(48_000, 2, 20).unwrap();
        assert_eq!(contract.validate_request(&stream), Ok(()));
        assert_eq!(stream.addon_id, "example.speaker-contract");
        assert_eq!(stream.activation_instance, "speaker-contract-1");

        let mut mismatched = stream.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidSpeakerBoundary)
        );
        assert_eq!(
            contract.request(48_000, 2, 21),
            Err(ApprovalError::InvalidSpeakerBoundary)
        );
    }

    #[test]
    fn client_local_inference_contract_is_local_bounded_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::local_inference_with_model(
                ExecutionTarget::Client,
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.local-inference-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan =
            ClientExecutionPlan::from_approved(&approval, "local-inference-contract-1").unwrap();
        let contract = ClientInferenceContract::from_client_plan(&plan).unwrap();

        assert!(contract.opt_in);
        assert!(contract.local_only);
        assert!(!contract.server_command_transport);
        assert_eq!(contract.max_input_bytes, 64 * 1024);
        assert_eq!(contract.max_output_tokens, 2_048);
        assert_eq!(contract.max_duration_ms, 5_000);
        assert_eq!(contract.max_output_bytes, 64 * 1024);
        assert_eq!(contract.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(contract.cpu_millis, 1_000);
        assert_eq!(contract.pids, 128);
        assert_eq!(contract.writable_bytes, 128 * 1024 * 1024);
        assert_eq!(
            contract.model_digest,
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        let inference = contract
            .request(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                1024,
                512,
                1_000,
            )
            .unwrap();
        assert_eq!(contract.validate_request(&inference), Ok(()));
        let result = contract.result(4096, 256, 250).unwrap();
        assert_eq!(contract.validate_result(&result), Ok(()));
        assert_eq!(result.addon_id, "example.local-inference-contract");
        assert_eq!(result.activation_instance, "local-inference-contract-1");
        assert_eq!(
            result.model_digest,
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(result.output_bytes, 4096);
        assert_eq!(result.output_tokens, 256);
        assert_eq!(result.duration_ms, 250);
        assert_eq!(
            contract.result(64 * 1024 + 1, 256, 250),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
        let mut mismatched_result = result.clone();
        mismatched_result.model_digest =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        assert_eq!(
            contract.validate_result(&mismatched_result),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
        assert_eq!(inference.addon_id, "example.local-inference-contract");
        assert_eq!(inference.activation_instance, "local-inference-contract-1");

        let mut mismatched = inference.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
        assert_eq!(
            contract.request(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                1024,
                512,
                1_000,
            ),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
        assert_eq!(
            contract.request("not-a-digest", 1024, 512, 1_000),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
        assert_eq!(
            contract.request(
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                65 * 1024,
                512,
                1_000,
            ),
            Err(ApprovalError::InvalidLocalInferenceBoundary)
        );
    }

    #[test]
    fn client_private_storage_contract_is_local_bounded_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::storage(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-storage",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-storage-1").unwrap();
        let contract = ClientPrivateStorageContract::from_client_plan(&plan).unwrap();

        assert!(contract.private_local);
        assert!(!contract.server_command_transport);
        assert_eq!(contract.max_key_bytes, 128);
        assert_eq!(contract.max_value_bytes, 64 * 1024);
        assert_eq!(contract.max_list_items, 1_000);
        let get = contract.request(ClientStorageOperation::Get {
            key: "profile".to_string(),
        });
        assert_eq!(contract.validate_request(&get), Ok(()));
        let response = contract
            .response(ClientStorageResult::Get {
                found: true,
                value_bytes: 1024,
            })
            .unwrap();
        assert_eq!(contract.validate_response(&response), Ok(()));
        assert_eq!(
            contract.validate_response_for_request(&get, &response),
            Ok(())
        );
        let put_request = contract.request(ClientStorageOperation::Put {
            key: "profile".to_string(),
            value_bytes: 1024,
        });
        assert_eq!(
            contract.validate_response_for_request(&put_request, &response),
            Err(ApprovalError::InvalidClientStorageBoundary)
        );
        assert_eq!(response.addon_id, "example.client-storage");
        assert_eq!(response.activation_instance, "client-storage-1");
        assert_eq!(
            contract.response(ClientStorageResult::Put {
                stored_bytes: 64 * 1024 + 1,
            }),
            Err(ApprovalError::InvalidClientStorageBoundary)
        );
        let request = contract.request(ClientStorageOperation::Put {
            key: "settings".to_string(),
            value_bytes: 1024,
        });
        assert_eq!(contract.validate_request(&request), Ok(()));
        assert_eq!(request.addon_id, "example.client-storage");
        assert_eq!(request.activation_instance, "client-storage-1");

        let mut mismatched = request.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidClientStorageBoundary)
        );
        assert_eq!(
            contract.validate_request(&contract.request(ClientStorageOperation::Put {
                key: "../escape".to_string(),
                value_bytes: 1,
            })),
            Err(ApprovalError::InvalidClientStorageBoundary)
        );
    }

    #[test]
    fn client_host_api_contract_is_read_only_typed_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Client,
                "client_info",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-host-api",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-host-api-1").unwrap();
        let contract = ClientHostApiContract::from_client_plan(&plan).unwrap();

        assert!(contract.read_only);
        assert!(!contract.allow_database);
        assert!(!contract.allow_credentials);
        assert!(!contract.allow_os_control);
        let request = contract
            .request(ClientHostApiOperation::GetClientInfo)
            .unwrap();
        assert_eq!(contract.validate_request(&request), Ok(()));
        assert_eq!(
            contract.validate_response(
                ClientHostApiOperation::GetClientInfo,
                &ClientHostApiResponse::client_info("client-1", "Mirror", "1.2.3", 1,).unwrap(),
            ),
            Ok(())
        );

        let mut mismatched = request.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::HostApiIdentityMismatch)
        );
    }

    #[test]
    fn client_plan_contains_only_client_scoped_capabilities() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server, ExecutionTarget::Client],
            server: Some(container("registry.example/server", 'a')),
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![
                AddonCapabilityRequest::storage(ExecutionTarget::Server),
                AddonCapabilityRequest::camera(ExecutionTarget::Client),
            ],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-plan",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.set_capability_approved(1, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-plan-1").unwrap();

        assert_eq!(plan.addon_id, "example.client-plan");
        assert_eq!(plan.activation_instance, "client-plan-1");
        assert_eq!(plan.image, "registry.example/client");
        assert_eq!(plan.image_digest, format!("sha256:{}", "b".repeat(64)));
        assert_eq!(plan.capabilities.len(), 1);
        assert_eq!(plan.capabilities[0].target, ExecutionTarget::Client);
        assert_eq!(plan.capabilities[0].kind, CapabilityKind::CameraCapture);
    }

    #[test]
    fn client_oci_spec_is_hardened_and_inert_without_broker_capabilities() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: Vec::new(),
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-oci",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-oci-1").unwrap();
        let spec = ClientOciLaunchSpec::from_client_plan(&plan).unwrap();

        assert_eq!(
            spec.image_ref,
            format!("registry.example/client@sha256:{}", "b".repeat(64))
        );
        assert!(spec.rootless);
        assert!(spec.non_root);
        assert!(spec.network_disabled);
        assert!(spec.read_only_rootfs);
        assert!(spec.no_new_privileges);
        assert!(spec.drop_all_capabilities);
        assert!(spec.seccomp_default);
    }

    #[test]
    fn client_oci_spec_rejects_hardware_capabilities_until_broker_exists() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::camera(ExecutionTarget::Client)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-camera",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-camera-1").unwrap();

        assert_eq!(
            ClientOciLaunchSpec::from_client_plan(&plan),
            Err(ApprovalError::OciCapabilityBoundaryUnavailable(
                CapabilityKind::CameraCapture
            ))
        );
    }

    #[test]
    fn server_storage_contract_is_private_and_identity_bound() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::storage(ExecutionTarget::Server)],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.storage-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "storage-contract-1").unwrap();
        let contract = ServerStorageContract::from_server_plan(&plan).unwrap();

        assert_eq!(contract.database.addon_id, "example.storage-contract");
        assert_eq!(contract.database.activation_instance, "storage-contract-1");
        assert_eq!(
            contract.database.storage_namespace,
            "example.storage-contract"
        );
        assert!(!contract.database.allow_raw_sql);
        assert!(!contract.database.allow_core_tables);
        assert!(!contract.database.allow_migrations);
        assert!(!contract.database.allow_sqlite_file_mount);
        assert!(contract.database.audit_required);
        assert!(contract
            .database
            .binds_identity("example.storage-contract", "storage-contract-1"));
        assert!(!contract
            .database
            .binds_identity("example.storage-contract", "other-activation"));

        let request = contract.request(crate::trusted_service_database::DatabaseOperation::Put {
            key: "settings".to_string(),
            value_json: "{\"enabled\":true}".to_string(),
        });
        assert_eq!(contract.validate_request(&request), Ok(()));
        let mut mismatched = request.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::InvalidStorageBoundary)
        );
    }

    #[test]
    fn server_storage_contract_requires_explicit_approval() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: Vec::new(),
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.storage-deny",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "storage-deny-1").unwrap();

        assert_eq!(
            ServerStorageContract::from_server_plan(&plan),
            Err(ApprovalError::StorageCapabilityUnavailable)
        );
    }

    #[test]
    fn server_host_api_request_is_bound_to_addon_and_activation() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.host-request",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "host-request-1").unwrap();
        let contract = ServerHostApiContract::from_server_plan(&plan).unwrap();

        let request = contract
            .request(ServerHostApiOperation::ListClients)
            .unwrap();
        assert_eq!(contract.validate_request(&request), Ok(()));
        assert_eq!(request.addon_id, "example.host-request");
        assert_eq!(request.activation_instance, "host-request-1");

        let mut mismatched = request.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            contract.validate_request(&mismatched),
            Err(ApprovalError::HostApiIdentityMismatch)
        );
    }

    #[test]
    fn client_network_boundary_is_deny_by_default_and_exactly_scoped() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Client],
            server: None,
            client: Some(container("registry.example/client", 'b')),
            capabilities: vec![AddonCapabilityRequest::network(
                ExecutionTarget::Client,
                NetworkAllowRule {
                    name: "weather-api".to_string(),
                    ip: "203.0.113.10".to_string(),
                    mac: Some("02:00:00:00:00:01".to_string()),
                    protocol: NetworkProtocol::Tcp,
                    ports: vec![443],
                },
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.client-network",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ClientExecutionPlan::from_approved(&approval, "client-network-1").unwrap();
        let boundary = ClientNetworkBoundary::from_client_plan(&plan).unwrap();

        assert!(boundary.deny_by_default);
        assert!(boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.11".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            80,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            None
        ));
        let access = boundary
            .request(
                "weather-api",
                "203.0.113.10",
                NetworkProtocol::Tcp,
                443,
                Some("02:00:00:00:00:01"),
            )
            .unwrap();
        assert_eq!(boundary.validate_request(&access), Ok(()));
        assert_eq!(
            boundary.request(
                "other-device",
                "203.0.113.10",
                NetworkProtocol::Tcp,
                443,
                Some("02:00:00:00:00:01"),
            ),
            Err(ApprovalError::InvalidNetworkBoundary)
        );
        let mut mismatched = access.clone();
        mismatched.activation_instance = "other-activation".to_string();
        assert_eq!(
            boundary.validate_request(&mismatched),
            Err(ApprovalError::InvalidNetworkBoundary)
        );
        let mut non_deny_boundary = boundary.clone();
        non_deny_boundary.deny_by_default = false;
        assert_eq!(
            non_deny_boundary.request(
                "weather-api",
                "203.0.113.10",
                NetworkProtocol::Tcp,
                443,
                Some("02:00:00:00:00:01"),
            ),
            Err(ApprovalError::InvalidNetworkBoundary)
        );
    }

    #[test]
    fn server_network_boundary_is_deny_by_default_and_exactly_scoped() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::network(
                ExecutionTarget::Server,
                NetworkAllowRule {
                    name: "weather-api".to_string(),
                    ip: "203.0.113.10".to_string(),
                    mac: Some("02:00:00:00:00:01".to_string()),
                    protocol: NetworkProtocol::Tcp,
                    ports: vec![443],
                },
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.network-boundary",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "network-boundary-1").unwrap();
        let boundary = ServerNetworkBoundary::from_server_plan(&plan).unwrap();

        assert!(boundary.deny_by_default);
        assert!(boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.11".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            80,
            Some("02:00:00:00:00:01")
        ));
        assert!(!boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            None
        ));
    }

    #[test]
    fn server_network_boundary_without_approval_rules_denies_all() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: Vec::new(),
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.network-deny",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "network-deny-1").unwrap();
        let boundary = ServerNetworkBoundary::from_server_plan(&plan).unwrap();

        assert!(!boundary.allows(
            "203.0.113.10".parse().unwrap(),
            NetworkProtocol::Tcp,
            443,
            None
        ));
    }

    #[test]
    fn server_host_api_contract_bounds_typed_client_responses_without_credentials() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.host-response",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "host-response-1").unwrap();
        let contract = ServerHostApiContract::from_server_plan(&plan).unwrap();

        let response = ServerHostApiResponse::clients(vec![ServerClientSummary {
            id: "client-1".to_string(),
            name: "Mirror".to_string(),
            status: "online".to_string(),
            version: Some("1.2.3".to_string()),
            last_seen: "2026-10-20T14:30:00Z".to_string(),
        }])
        .unwrap();
        assert_eq!(
            contract.validate_response(ServerHostApiOperation::ListClients, &response),
            Ok(())
        );
        assert!(!serde_json::to_string(&response)
            .unwrap()
            .contains("pairing"));
        assert!(!serde_json::to_string(&response)
            .unwrap()
            .contains("credential"));
    }

    #[test]
    fn server_host_api_contract_rejects_oversized_typed_client_responses() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.host-response-limit",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "host-response-limit-1").unwrap();
        let contract = ServerHostApiContract::from_server_plan(&plan).unwrap();
        let response = ServerHostApiResponse::clients(
            (0..101)
                .map(|index| ServerClientSummary {
                    id: format!("client-{index}"),
                    name: "Mirror".to_string(),
                    status: "online".to_string(),
                    version: None,
                    last_seen: "2026-10-20T14:30:00Z".to_string(),
                })
                .collect(),
        )
        .unwrap();

        assert_eq!(
            contract.validate_response(ServerHostApiOperation::ListClients, &response),
            Err(ApprovalError::HostApiResponseLimitExceeded)
        );
    }

    #[test]
    fn server_host_api_contract_is_read_only_and_typed() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.host-contract",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "host-contract-1").unwrap();

        let contract = ServerHostApiContract::from_server_plan(&plan).unwrap();
        assert_eq!(contract.addon_id, "example.host-contract");
        assert_eq!(contract.activation_instance, "host-contract-1");
        assert_eq!(
            contract.operations,
            vec![ServerHostApiOperation::ListClients]
        );
        assert!(contract.read_only);
        assert!(!contract.allow_database);
        assert!(!contract.allow_credentials);
        assert!(!contract.allow_os_control);
        assert_eq!(contract.max_items, 100);
        assert_eq!(contract.max_response_bytes, 64 * 1024);
    }

    #[test]
    fn server_host_api_contract_rejects_unknown_scope() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "database",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.unknown-scope",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "unknown-scope-1").unwrap();

        assert_eq!(
            ServerHostApiContract::from_server_plan(&plan),
            Err(ApprovalError::UnsupportedHostApiScope)
        );
    }

    #[test]
    fn approved_server_plan_produces_hardened_inert_oci_spec() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: Vec::new(),
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.inert-server",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();

        let plan = ServerExecutionPlan::from_approved(&approval, "inert-server-1").unwrap();
        let spec = ServerOciLaunchSpec::from_server_plan(&plan).unwrap();
        assert_eq!(spec.addon_id, "example.inert-server");
        assert_eq!(spec.activation_instance, "inert-server-1");
        assert_eq!(
            spec.image_ref,
            "registry.example/server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(spec.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(spec.cpu_millis, 1_000);
        assert_eq!(spec.pids, 128);
        assert_eq!(spec.writable_bytes, 128 * 1024 * 1024);
        assert!(spec.rootless);
        assert!(spec.non_root);
        assert!(spec.network_disabled);
        assert!(spec.read_only_rootfs);
        assert!(spec.no_new_privileges);
        assert!(spec.drop_all_capabilities);
        assert!(spec.seccomp_default);
    }

    #[test]
    fn inert_oci_spec_rejects_unimplemented_host_boundaries() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.host-api",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "host-api-1").unwrap();

        assert_eq!(
            ServerOciLaunchSpec::from_server_plan(&plan),
            Err(ApprovalError::OciCapabilityBoundaryUnavailable(
                CapabilityKind::HostApiRead
            ))
        );
    }

    #[test]
    fn inert_oci_spec_rejects_unimplemented_network_boundary() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::network(
                ExecutionTarget::Server,
                NetworkAllowRule {
                    name: "api".to_string(),
                    ip: "192.0.2.10".to_string(),
                    mac: None,
                    protocol: NetworkProtocol::Tcp,
                    ports: vec![443],
                },
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.network",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();
        let plan = ServerExecutionPlan::from_approved(&approval, "network-1").unwrap();

        assert_eq!(
            ServerOciLaunchSpec::from_server_plan(&plan),
            Err(ApprovalError::OciCapabilityBoundaryUnavailable(
                CapabilityKind::NetworkEgress
            ))
        );
    }

    #[test]
    fn approval_is_bound_to_addon_version_and_digest() {
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let mut approval = AddonApprovalRecord::pending(
            "example.server",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        approval.set_capability_approved(0, true).unwrap();
        approval.approve("admin", "2026-10-20T14:30:00Z").unwrap();

        assert_eq!(
            approval.validate_binding(
                "example.server",
                "1.0.1",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ),
            Err(ApprovalError::ApprovalBindingMismatch)
        );
        assert_eq!(
            approval.validate_binding(
                "example.server",
                "1.0.0",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            ),
            Err(ApprovalError::ApprovalBindingMismatch)
        );
    }

    #[test]
    fn approval_store_round_trips_private_records() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("screentra-addon-approval-{}", uuid::Uuid::new_v4()));
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let approval = AddonApprovalRecord::pending(
            "example.store",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        AddonApprovalStore::save(&approval).unwrap();
        let loaded = AddonApprovalStore::load("example.store").unwrap();
        assert_eq!(loaded, approval);
        if let Some(value) = old_data_dir {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn approval_store_lists_persisted_records_deterministically() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-addon-approval-list-{}",
            uuid::Uuid::new_v4()
        ));
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        for addon_id in ["example.zulu", "example.alpha"] {
            let approval = AddonApprovalRecord::pending(
                addon_id,
                "1.0.0",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                AddonExecutionRequest {
                    targets: vec![ExecutionTarget::Server],
                    server: Some(container("registry.example/server", 'a')),
                    client: None,
                    capabilities: vec![AddonCapabilityRequest::host_api_read(
                        ExecutionTarget::Server,
                        "clients",
                    )],
                },
            )
            .unwrap();
            AddonApprovalStore::save(&approval).unwrap();
        }
        let listed = AddonApprovalStore::list().unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|record| record.addon_id.as_str())
                .collect::<Vec<_>>(),
            vec!["example.alpha", "example.zulu"]
        );
        if let Some(value) = old_data_dir {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn approval_store_rejects_path_identity_mismatch() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "screentra-addon-approval-identity-{}",
            uuid::Uuid::new_v4()
        ));
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &root);
        }
        let request = AddonExecutionRequest {
            targets: vec![ExecutionTarget::Server],
            server: Some(container("registry.example/server", 'a')),
            client: None,
            capabilities: vec![AddonCapabilityRequest::host_api_read(
                ExecutionTarget::Server,
                "clients",
            )],
        };
        let approval = AddonApprovalRecord::pending(
            "example.path",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            request,
        )
        .unwrap();
        AddonApprovalStore::save(&approval).unwrap();
        let mut tampered = approval.clone();
        tampered.addon_id = "example.other".to_string();
        let path = root
            .join(APPROVAL_ROOT)
            .join(format!("{}{}", approval.addon_id, APPROVAL_FILE_SUFFIX));
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        assert_eq!(
            AddonApprovalStore::load(&approval.addon_id),
            Err("Approval record identity mismatch".to_string())
        );
        if let Some(value) = old_data_dir {
            unsafe {
                std::env::set_var("SIGNAGE_DATA_DIR", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_DATA_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    fn container(image: &str, digest_byte: char) -> AddonContainerSpec {
        AddonContainerSpec {
            image: image.to_string(),
            image_digest: format!("sha256:{}", digest_byte.to_string().repeat(64)),
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 1_000,
            pids: 128,
            writable_bytes: 128 * 1024 * 1024,
        }
    }
}
