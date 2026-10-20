use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::IpAddr;

use crate::addons;

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
    InvalidVersion,
    InvalidArtifactDigest,
    InvalidRequest(CapabilityContractError),
    CapabilityApprovalCountMismatch,
    InvalidCapabilityIndex,
    CapabilityNotApproved,
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
            schema_version: 1,
            addon_id: addon_id.to_string(),
            addon_version: addon_version.to_string(),
            artifact_digest: artifact_digest.to_string(),
            capability_approved: vec![false; request.capabilities.len()],
            request,
            state: ApprovalState::Pending,
            approved_by: None,
            approved_at: None,
            revoked_by: None,
            revoked_at: None,
        })
    }

    pub fn validate(&self) -> Result<(), ApprovalError> {
        if self.schema_version != 1 {
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
        if matches!(self.state, ApprovalState::Approved) {
            if self.approved_by.is_none() || self.approved_at.is_none() {
                return Err(ApprovalError::NotApproved);
            }
            if self.capability_approved.iter().any(|approved| !approved) {
                return Err(ApprovalError::CapabilityNotApproved);
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

    pub fn approve(&mut self, approver: &str, timestamp: &str) -> Result<(), ApprovalError> {
        if self.state != ApprovalState::Pending {
            return Err(ApprovalError::ApprovalAlreadyFinalized);
        }
        if self.capability_approved.iter().any(|approved| !approved) {
            return Err(ApprovalError::CapabilityNotApproved);
        }
        validate_approver(approver)?;
        validate_timestamp(timestamp)?;
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
        Ok(())
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
