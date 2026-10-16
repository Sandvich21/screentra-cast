use crate::trusted_service_database::{
    DatabaseOperation, DatabaseRequest, DatabaseResponse, TrustedServiceDatabaseContract,
    MAX_REQUEST_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum GatewayStorageOperation {
    Get { key: String },
    Put { key: String, value_json: String },
    Delete { key: String },
    List { prefix: String, limit: u32 },
}

pub const GATEWAY_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayConnectionHello {
    pub protocol_version: u16,
    pub addon_id: String,
    pub activation_instance: String,
}

pub fn validate_connection_hello(
    hello: &GatewayConnectionHello,
    binding: &GatewayActivationBinding,
) -> Result<(), String> {
    binding.validate()?;
    if hello.protocol_version != GATEWAY_PROTOCOL_VERSION
        || hello.addon_id != binding.contract.addon_id
        || hello.activation_instance != binding.contract.activation_instance
    {
        return Err("Gateway connection identity is not permitted".to_string());
    }
    Ok(())
}

pub fn encode_connection_hello(hello: &GatewayConnectionHello) -> Result<Vec<u8>, String> {
    let bytes =
        serde_json::to_vec(hello).map_err(|_| "Invalid gateway connection hello".to_string())?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway connection hello is too large".to_string());
    }
    Ok(bytes)
}

pub fn decode_connection_hello(
    bytes: &[u8],
    binding: &GatewayActivationBinding,
) -> Result<GatewayConnectionHello, String> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway connection hello is too large".to_string());
    }
    let hello: GatewayConnectionHello = serde_json::from_slice(bytes)
        .map_err(|_| "Invalid gateway connection hello".to_string())?;
    validate_connection_hello(&hello, binding)?;
    Ok(hello)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayPeerCredentials {
    uid: u32,
    gid: u32,
    pid: u32,
}

impl GatewayPeerCredentials {
    /// Constructed by a future OS-backed transport adapter, never from a frame.
    pub(crate) fn from_os_credentials(uid: u32, gid: u32, pid: u32) -> Self {
        Self { uid, gid, pid }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayPeerPolicy {
    expected_uid: u32,
    expected_gid: u32,
    expected_pid: u32,
}

impl GatewayPeerPolicy {
    pub fn new(expected_uid: u32, expected_gid: u32, expected_pid: u32) -> Result<Self, String> {
        if expected_uid == 0 || expected_gid == 0 || expected_pid == 0 {
            return Err(
                "Gateway peer policy requires non-root credentials and a process ID".to_string(),
            );
        }
        Ok(Self {
            expected_uid,
            expected_gid,
            expected_pid,
        })
    }

    pub fn validate(&self, peer: GatewayPeerCredentials) -> Result<(), String> {
        if peer.uid == 0
            || peer.gid == 0
            || peer.pid == 0
            || peer.uid != self.expected_uid
            || peer.gid != self.expected_gid
            || peer.pid != self.expected_pid
        {
            return Err("Gateway peer credentials are not permitted".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRequestEnvelope {
    pub addon_id: String,
    pub activation_instance: String,
    pub request_id: String,
    pub capability: String,
    pub operation: GatewayStorageOperation,
}

pub fn encode_request(request: &GatewayRequestEnvelope) -> Result<Vec<u8>, String> {
    validate_request_id(&request.request_id)?;
    let bytes = serde_json::to_vec(request).map_err(|_| "Invalid gateway request".to_string())?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway request is too large".to_string());
    }
    Ok(bytes)
}

const GATEWAY_FRAME_LENGTH_BYTES: usize = 4;

pub fn encode_gateway_frame(payload: &[u8], maximum_bytes: usize) -> Result<Vec<u8>, String> {
    if payload.len() > maximum_bytes || payload.len() > u32::MAX as usize {
        return Err("Gateway frame is too large".to_string());
    }
    let mut frame = Vec::with_capacity(GATEWAY_FRAME_LENGTH_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn decode_gateway_frame(
    bytes: &[u8],
    maximum_bytes: usize,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if bytes.len() < GATEWAY_FRAME_LENGTH_BYTES {
        return Ok(None);
    }
    let frame_len = u32::from_be_bytes(bytes[..GATEWAY_FRAME_LENGTH_BYTES].try_into().unwrap()) as usize;
    if frame_len > maximum_bytes {
        return Err("Gateway frame is too large".to_string());
    }
    let total_len = GATEWAY_FRAME_LENGTH_BYTES
        .checked_add(frame_len)
        .ok_or_else(|| "Gateway frame is too large".to_string())?;
    if bytes.len() < total_len {
        return Ok(None);
    }
    Ok(Some((
        bytes[GATEWAY_FRAME_LENGTH_BYTES..total_len].to_vec(),
        total_len,
    )))
}

pub fn encode_request_frame(request: &GatewayRequestEnvelope) -> Result<Vec<u8>, String> {
    let payload = encode_request(request)?;
    encode_gateway_frame(&payload, MAX_REQUEST_BYTES)
}

pub fn decode_request_frame(
    bytes: &[u8],
    binding: &GatewayActivationBinding,
) -> Result<Option<(DecodedGatewayRequest, usize)>, String> {
    let Some((payload, consumed)) = decode_gateway_frame(bytes, MAX_REQUEST_BYTES)? else {
        return Ok(None);
    };
    Ok(Some((decode_request_with_id(&payload, binding)?, consumed)))
}

pub fn encode_response_frame(response: &DatabaseResponse) -> Result<Vec<u8>, String> {
    let payload = encode_response(response)?;
    encode_gateway_frame(&payload, MAX_RESPONSE_BYTES)
}

fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err("Gateway request ID is invalid".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayActivationState {
    Active,
    Stopped,
    Disabled,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayActivationBinding {
    pub contract: TrustedServiceDatabaseContract,
    pub state: GatewayActivationState,
}

impl GatewayActivationBinding {
    pub fn new(contract: TrustedServiceDatabaseContract) -> Self {
        Self {
            contract,
            state: GatewayActivationState::Active,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.state != GatewayActivationState::Active {
            return Err("Gateway activation is not active".to_string());
        }
        self.contract
            .validate()
            .map_err(|_| "Gateway activation contract is not permitted".to_string())
    }
}

pub const MAX_REQUESTS_PER_ACTIVATION: u32 = 60;
const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct RateState {
    window_started: Instant,
    requests: u32,
}

#[derive(Debug, Default)]
pub struct GatewayRateLimiter {
    entries: BTreeMap<(String, String), RateState>,
}

impl GatewayRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(
        &mut self,
        binding: &GatewayActivationBinding,
        now: Instant,
    ) -> Result<(), String> {
        binding.validate()?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_TRACKED_ACTIVATIONS {
            return Err("Gateway activation rate-limit state is full".to_string());
        }
        let state = self.entries.entry(key).or_insert(RateState {
            window_started: now,
            requests: 0,
        });
        if now
            .checked_duration_since(state.window_started)
            .unwrap_or_default()
            >= RATE_WINDOW
        {
            state.window_started = now;
            state.requests = 0;
        }
        if state.requests >= MAX_REQUESTS_PER_ACTIVATION {
            return Err("Gateway activation rate limit exceeded".to_string());
        }
        state.requests += 1;
        Ok(())
    }

    fn retire(&mut self, contract: &TrustedServiceDatabaseContract) -> Result<(), String> {
        contract
            .validate()
            .map_err(|_| "Gateway activation contract is not permitted".to_string())?;
        self.entries.remove(&(
            contract.addon_id.clone(),
            contract.activation_instance.clone(),
        ));
        Ok(())
    }
}
pub const MAX_REQUEST_IDS_PER_ACTIVATION: usize = 1_024;
pub const MAX_TRACKED_ACTIVATIONS: usize = 1_024;

#[derive(Debug, Default)]
pub struct GatewayReplayGuard {
    accepted: BTreeMap<(String, String), BTreeSet<String>>,
}

impl GatewayReplayGuard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(
        &self,
        binding: &GatewayActivationBinding,
        request_id: &str,
    ) -> Result<(), String> {
        binding.validate()?;
        validate_request_id(request_id)?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        let Some(ids) = self.accepted.get(&key) else {
            if self.accepted.len() >= MAX_TRACKED_ACTIVATIONS {
                return Err("Gateway replay state is full".to_string());
            }
            return Ok(());
        };
        if ids.contains(request_id) {
            return Err("Gateway request was already accepted".to_string());
        }
        if ids.len() >= MAX_REQUEST_IDS_PER_ACTIVATION {
            return Err("Gateway replay state is full".to_string());
        }
        Ok(())
    }

    pub fn accept(
        &mut self,
        binding: &GatewayActivationBinding,
        request_id: &str,
    ) -> Result<(), String> {
        self.check(binding, request_id)?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        self.accepted
            .entry(key)
            .or_default()
            .insert(request_id.to_string());
        Ok(())
    }

    fn retire(&mut self, contract: &TrustedServiceDatabaseContract) -> Result<(), String> {
        contract
            .validate()
            .map_err(|_| "Gateway activation contract is not permitted".to_string())?;
        self.accepted.remove(&(
            contract.addon_id.clone(),
            contract.activation_instance.clone(),
        ));
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedGatewayRequest {
    pub request_id: String,
    pub database_request: DatabaseRequest,
}

fn decode_request_with_id(
    bytes: &[u8],
    binding: &GatewayActivationBinding,
) -> Result<DecodedGatewayRequest, String> {
    binding.validate()?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Gateway request is too large".to_string());
    }
    let envelope: GatewayRequestEnvelope =
        serde_json::from_slice(bytes).map_err(|_| "Invalid gateway request".to_string())?;
    validate_request_id(&envelope.request_id)?;
    if envelope.capability != "storage" {
        return Err("Gateway capability is not permitted".to_string());
    }
    let request_id = envelope.request_id.clone();
    let database_request = envelope.into_database_request();
    database_request
        .validate_against(&binding.contract)
        .map_err(|_| "Gateway request is not permitted".to_string())?;
    Ok(DecodedGatewayRequest {
        request_id,
        database_request,
    })
}

pub fn decode_request(
    bytes: &[u8],
    binding: &GatewayActivationBinding,
) -> Result<DatabaseRequest, String> {
    Ok(decode_request_with_id(bytes, binding)?.database_request)
}

#[derive(Debug, Default)]
pub struct GatewayRequestAdmission {
    rate_limiter: GatewayRateLimiter,
    replay_guard: GatewayReplayGuard,
}

impl GatewayRequestAdmission {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn admit(
        &mut self,
        bytes: &[u8],
        binding: &GatewayActivationBinding,
        now: Instant,
    ) -> Result<DecodedGatewayRequest, String> {
        let decoded = decode_request_with_id(bytes, binding)?;
        self.replay_guard.check(binding, &decoded.request_id)?;
        self.rate_limiter.check(binding, now)?;
        self.replay_guard.accept(binding, &decoded.request_id)?;
        Ok(decoded)
    }

    pub fn retire_activation(&mut self, binding: &GatewayActivationBinding) -> Result<(), String> {
        self.rate_limiter.retire(&binding.contract)?;
        self.replay_guard.retire(&binding.contract)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct GatewaySession {
    binding: GatewayActivationBinding,
    peer_policy: GatewayPeerPolicy,
    admission: GatewayRequestAdmission,
    budget: GatewayOperationBudget,
    established: bool,
    retired: bool,
}

impl GatewaySession {
    pub fn new(binding: GatewayActivationBinding, peer_policy: GatewayPeerPolicy) -> Self {
        Self {
            binding,
            peer_policy,
            admission: GatewayRequestAdmission::new(),
            budget: GatewayOperationBudget::new(),
            established: false,
            retired: false,
        }
    }

    pub fn accept_hello(
        &mut self,
        bytes: &[u8],
        peer: GatewayPeerCredentials,
    ) -> Result<(), String> {
        if self.retired {
            return Err("Gateway session is retired".to_string());
        }
        if self.established {
            return Err("Gateway session is already established".to_string());
        }
        self.peer_policy.validate(peer)?;
        decode_connection_hello(bytes, &self.binding)?;
        self.established = true;
        Ok(())
    }

    pub fn is_established(&self) -> bool {
        self.established
    }

    pub fn admit(&mut self, bytes: &[u8], now: Instant) -> Result<DecodedGatewayRequest, String> {
        if !self.established {
            return Err("Gateway connection hello is required".to_string());
        }
        self.budget.take_at(now)?;
        self.admission.admit(bytes, &self.binding, now)
    }

    pub fn retire(&mut self) -> Result<(), String> {
        self.established = false;
        self.retired = true;
        self.admission.retire_activation(&self.binding)
    }
}

pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub const MAX_OPERATIONS_PER_CONNECTION: u32 = 100;
pub const MAX_REQUEST_DURATION: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum GatewayResponseEnvelope {
    Value { value_json: Option<String> },
    Written,
    Deleted { deleted: bool },
    Entries { entries: Vec<(String, String)> },
}

fn encoded_json_string_len(value: &str) -> Option<usize> {
    value.chars().try_fold(2usize, |length, character| {
        let extra = match character {
            '"' | '\\' => 2,
            character if character <= '\u{1f}' => 6,
            character => character.len_utf8(),
        };
        length.checked_add(extra)
    })
}

pub fn encode_response(response: &DatabaseResponse) -> Result<Vec<u8>, String> {
    let estimated_size = match response {
        DatabaseResponse::Value(value_json) => 32usize
            .checked_add(match value_json.as_deref() {
                Some(value) => encoded_json_string_len(value)
                    .ok_or_else(|| "Gateway response is too large".to_string())?,
                None => 0,
            })
            .ok_or_else(|| "Gateway response is too large".to_string())?,
        DatabaseResponse::Written => 32,
        DatabaseResponse::Deleted(_) => 48,
        DatabaseResponse::Entries(entries) => {
            let entries_size = entries.iter().try_fold(0usize, |total, (key, value)| {
                let item_size = encoded_json_string_len(key)?
                    .checked_add(encoded_json_string_len(value)?)?
                    .checked_add(3)?;
                total.checked_add(item_size)
            });
            64usize
                .checked_add(
                    entries_size.ok_or_else(|| "Gateway response is too large".to_string())?,
                )
                .ok_or_else(|| "Gateway response is too large".to_string())?
        }
    };
    if estimated_size > MAX_RESPONSE_BYTES {
        return Err("Gateway response is too large".to_string());
    }
    let envelope = match response {
        DatabaseResponse::Value(value_json) => GatewayResponseEnvelope::Value {
            value_json: value_json.clone(),
        },
        DatabaseResponse::Written => GatewayResponseEnvelope::Written,
        DatabaseResponse::Deleted(deleted) => {
            GatewayResponseEnvelope::Deleted { deleted: *deleted }
        }
        DatabaseResponse::Entries(entries) => GatewayResponseEnvelope::Entries {
            entries: entries.clone(),
        },
    };
    let bytes =
        serde_json::to_vec(&envelope).map_err(|_| "Invalid gateway response".to_string())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("Gateway response is too large".to_string());
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayOperationBudget {
    used: u32,
    deadline: Instant,
}

impl GatewayOperationBudget {
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    pub fn new_at(started_at: Instant) -> Self {
        Self {
            used: 0,
            deadline: started_at + MAX_REQUEST_DURATION,
        }
    }

    pub fn take(&mut self) -> Result<(), String> {
        self.take_at(Instant::now())
    }

    pub fn take_at(&mut self, now: Instant) -> Result<(), String> {
        if now >= self.deadline {
            return Err("Gateway request deadline exceeded".to_string());
        }
        if self.used >= MAX_OPERATIONS_PER_CONNECTION {
            return Err("Gateway operation budget exceeded".to_string());
        }
        self.used += 1;
        Ok(())
    }
}

impl GatewayRequestEnvelope {
    fn into_database_request(self) -> DatabaseRequest {
        let operation = match self.operation {
            GatewayStorageOperation::Get { key } => DatabaseOperation::Get { key },
            GatewayStorageOperation::Put { key, value_json } => {
                DatabaseOperation::Put { key, value_json }
            }
            GatewayStorageOperation::Delete { key } => DatabaseOperation::Delete { key },
            GatewayStorageOperation::List { prefix, limit } => {
                DatabaseOperation::List { prefix, limit }
            }
        };
        DatabaseRequest {
            addon_id: self.addon_id,
            activation_instance: self.activation_instance,
            operation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_connection_hello, decode_gateway_frame, decode_request, decode_request_frame,
        encode_connection_hello, encode_gateway_frame, encode_request, encode_request_frame,
        encode_response, encode_response_frame, validate_connection_hello,
        GatewayActivationBinding,
        GatewayActivationState, GatewayConnectionHello, GatewayOperationBudget,
        GatewayPeerCredentials, GatewayPeerPolicy, GatewayRateLimiter, GatewayReplayGuard,
        GatewayRequestAdmission, GatewayRequestEnvelope, GatewaySession, GatewayStorageOperation,
        GATEWAY_PROTOCOL_VERSION, MAX_OPERATIONS_PER_CONNECTION, MAX_REQUESTS_PER_ACTIVATION,
        MAX_RESPONSE_BYTES, MAX_TRACKED_ACTIVATIONS,
    };
    use crate::trusted_service_database::{
        DatabaseResponse, TrustedServiceDatabaseContract, MAX_REQUEST_BYTES,
    };
    use std::time::{Duration, Instant};

    fn contract() -> TrustedServiceDatabaseContract {
        TrustedServiceDatabaseContract::core_storage_api("example.weather", "activation-1")
    }

    fn binding() -> GatewayActivationBinding {
        GatewayActivationBinding::new(contract())
    }
    fn request() -> GatewayRequestEnvelope {
        GatewayRequestEnvelope {
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
            request_id: "request-1".to_string(),
            capability: "storage".to_string(),
            operation: GatewayStorageOperation::Put {
                key: "state".to_string(),
                value_json: "{\"enabled\":true}".to_string(),
            },
        }
    }

    #[test]
    fn peer_policy_requires_exact_non_root_credentials() {
        let policy =
            GatewayPeerPolicy::new(1000, 1001, 42).expect("non-root policy should be valid");
        policy
            .validate(GatewayPeerCredentials::from_os_credentials(1000, 1001, 42))
            .expect("matching peer credentials should be accepted");
        assert!(policy
            .validate(GatewayPeerCredentials::from_os_credentials(1002, 1001, 42))
            .is_err());
        assert!(policy
            .validate(GatewayPeerCredentials::from_os_credentials(1000, 1002, 42))
            .is_err());
        assert!(policy
            .validate(GatewayPeerCredentials::from_os_credentials(1000, 1001, 43))
            .is_err());
        assert!(policy
            .validate(GatewayPeerCredentials::from_os_credentials(0, 1001, 42))
            .is_err());
        assert!(GatewayPeerPolicy::new(0, 1001, 42).is_err());
        assert!(GatewayPeerPolicy::new(1000, 0, 42).is_err());
        assert!(GatewayPeerPolicy::new(1000, 1001, 0).is_err());
    }

    #[test]
    fn validates_connection_hello_before_request_admission() {
        let valid = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let hello_bytes = encode_connection_hello(&valid).expect("hello should encode");
        assert_eq!(
            decode_connection_hello(&hello_bytes, &binding()).expect("hello should decode"),
            valid
        );
        validate_connection_hello(&valid, &binding()).expect("hello should match binding");

        let mut wrong_identity = valid.clone();
        wrong_identity.addon_id = "example.other".to_string();
        assert!(validate_connection_hello(&wrong_identity, &binding()).is_err());
        let mut wrong_version = valid;
        wrong_version.protocol_version += 1;
        assert!(validate_connection_hello(&wrong_version, &binding()).is_err());
        let oversized = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "x".repeat(MAX_REQUEST_BYTES),
            activation_instance: "activation-1".to_string(),
        };
        assert!(encode_connection_hello(&oversized).is_err());
        let mut inactive = binding();
        inactive.state = GatewayActivationState::Stopped;
        assert!(validate_connection_hello(
            &GatewayConnectionHello {
                protocol_version: GATEWAY_PROTOCOL_VERSION,
                addon_id: "example.weather".to_string(),
                activation_instance: "activation-1".to_string(),
            },
            &inactive,
        )
        .is_err());
    }

    #[test]
    fn gateway_frame_codec_bounds_and_handles_partial_frames() {
        let frame = encode_gateway_frame(b"hello", 16).expect("frame should encode");
        assert_eq!(decode_gateway_frame(&frame[..2], 16).unwrap(), None);
        let mut partial = frame[..frame.len() - 1].to_vec();
        assert_eq!(decode_gateway_frame(&partial, 16).unwrap(), None);
        partial.push(*frame.last().unwrap());
        let (payload, consumed) = decode_gateway_frame(&partial, 16).unwrap().unwrap();
        assert_eq!(payload, b"hello");
        assert_eq!(consumed, frame.len());
        assert!(encode_gateway_frame(&[0u8; 17], 16).is_err());
        let oversized_length = (17u32).to_be_bytes();
        assert!(decode_gateway_frame(&oversized_length, 16).is_err());
    }

    #[test]
    fn typed_request_frame_wrapper_preserves_binding_and_partial_reads() {
        let frame = encode_request_frame(&request()).expect("request frame should encode");
        assert_eq!(decode_request_frame(&frame[..3], &binding()).unwrap(), None);
        let (decoded, consumed) = decode_request_frame(&frame, &binding())
            .unwrap()
            .expect("complete frame should decode");
        assert_eq!(decoded.request_id, "request-1");
        assert_eq!(consumed, frame.len());
        let response_frame = encode_response_frame(&DatabaseResponse::Written)
            .expect("response frame should encode");
        assert!(decode_gateway_frame(&response_frame, MAX_RESPONSE_BYTES)
            .unwrap()
            .is_some());
    }

    #[test]
    fn session_requires_valid_hello_before_admitting_requests() {
        let request_bytes = encode_request(&request()).expect("request should encode");
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let hello_bytes = encode_connection_hello(&hello).expect("hello should encode");
        let peer = GatewayPeerCredentials::from_os_credentials(1000, 1001, 42);
        let peer_policy =
            GatewayPeerPolicy::new(1000, 1001, 42).expect("peer policy should be valid");
        let start = Instant::now();
        let mut session = GatewaySession::new(binding(), peer_policy);
        assert!(session.admit(&request_bytes, start).is_err());
        assert!(session
            .accept_hello(
                &hello_bytes,
                GatewayPeerCredentials::from_os_credentials(1002, 1001, 42),
            )
            .is_err());
        assert!(!session.is_established());
        assert!(session
            .accept_hello(br#"{"protocol_version":0}"#, peer)
            .is_err());
        assert!(!session.is_established());
        session
            .accept_hello(&hello_bytes, peer)
            .expect("valid hello should establish session");
        assert!(session.is_established());
        assert!(session.accept_hello(&hello_bytes, peer).is_err());
        assert!(session.admit(&request_bytes, start).is_ok());
        let second_request = encode_request(&GatewayRequestEnvelope {
            request_id: "request-2".to_string(),
            ..request()
        })
        .expect("second request should encode");
        assert!(session
            .admit(&second_request, start + Duration::from_secs(6))
            .is_err());
        session
            .retire()
            .expect("session retirement should clean admission state");
        assert!(!session.is_established());
        assert!(session.accept_hello(&hello_bytes, peer).is_err());
        assert!(session.admit(&request_bytes, start).is_err());
    }

    #[test]
    fn round_trips_bounded_typed_storage_request() {
        let bytes = encode_request(&request()).expect("request should encode");
        let decoded = decode_request(&bytes, &binding()).expect("request should decode");
        assert_eq!(decoded.addon_id, "example.weather");
        assert_eq!(decoded.activation_instance, "activation-1");
    }

    #[test]
    fn rejects_wrong_capability_and_identity_before_backend_access() {
        let mut wrong_capability = request();
        wrong_capability.capability = "read:clients".to_string();
        let bytes = encode_request(&wrong_capability).expect("envelope should encode");
        assert!(decode_request(&bytes, &binding()).is_err());

        let mut wrong_identity = request();
        wrong_identity.activation_instance = "activation-2".to_string();
        let bytes = encode_request(&wrong_identity).expect("envelope should encode");
        assert!(decode_request(&bytes, &binding()).is_err());
    }

    #[test]
    fn bounds_responses_and_connection_operations() {
        let oversized = DatabaseResponse::Value(Some("x".repeat(70 * 1024)));
        assert!(encode_response(&oversized).is_err());

        let oversized_entries = DatabaseResponse::Entries(vec![
            ("first".to_string(), "x".repeat(40 * 1024)),
            ("second".to_string(), "x".repeat(40 * 1024)),
        ]);
        assert!(encode_response(&oversized_entries).is_err());

        let mut budget = GatewayOperationBudget::new();
        for _ in 0..MAX_OPERATIONS_PER_CONNECTION {
            budget
                .take()
                .expect("budget should allow configured operations");
        }
        assert!(budget.take().is_err());
    }

    #[test]
    fn rejects_operations_after_bounded_request_deadline() {
        let start = Instant::now();
        let mut budget = GatewayOperationBudget::new_at(start);
        budget
            .take_at(start)
            .expect("operation should be accepted before deadline");
        assert!(budget.take_at(start + Duration::from_secs(6)).is_err());
    }

    #[test]
    fn rejects_stopped_disabled_and_replaced_activation_bindings() {
        let bytes = encode_request(&request()).expect("request should encode");
        for state in [
            GatewayActivationState::Stopped,
            GatewayActivationState::Disabled,
            GatewayActivationState::Replaced,
        ] {
            let mut inactive = binding();
            inactive.state = state;
            assert!(decode_request(&bytes, &inactive).is_err());
        }
    }
    #[test]
    fn rejects_invalid_and_duplicate_request_ids() {
        let mut invalid = request();
        invalid.request_id = "request id".to_string();
        assert!(encode_request(&invalid).is_err());

        let mut guard = GatewayReplayGuard::new();
        guard
            .accept(&binding(), "request-1")
            .expect("first request ID should be accepted");
        assert!(guard.accept(&binding(), "request-1").is_err());
        let mut inactive = binding();
        inactive.state = GatewayActivationState::Stopped;
        assert!(guard.accept(&inactive, "request-2").is_err());
    }
    #[test]
    fn bounds_replay_state_across_activation_instances() {
        let mut guard = GatewayReplayGuard::new();
        for index in 0..MAX_TRACKED_ACTIVATIONS {
            let contract = TrustedServiceDatabaseContract::core_storage_api(
                &format!("example.service{index}"),
                "activation-1",
            );
            let binding = GatewayActivationBinding::new(contract);
            guard
                .accept(&binding, "request-1")
                .expect("replay state should allow configured activations");
        }
        let final_binding = GatewayActivationBinding::new(
            TrustedServiceDatabaseContract::core_storage_api("example.final", "activation-1"),
        );
        assert!(guard.accept(&final_binding, "request-1").is_err());
    }

    #[test]
    fn rate_limits_each_active_activation_and_resets_after_window() {
        let mut limiter = GatewayRateLimiter::new();
        let start = Instant::now();
        for _ in 0..MAX_REQUESTS_PER_ACTIVATION {
            limiter
                .check(&binding(), start)
                .expect("request should be within rate limit");
        }
        assert!(limiter.check(&binding(), start).is_err());
        assert!(limiter
            .check(&binding(), start + Duration::from_secs(60))
            .is_ok());

        let mut inactive = binding();
        inactive.state = GatewayActivationState::Stopped;
        assert!(limiter.check(&inactive, start).is_err());
    }
    #[test]
    fn admission_binds_rate_limit_and_replay_checks_to_decoded_request() {
        let bytes = encode_request(&request()).expect("request should encode");
        let start = Instant::now();
        let mut admission = GatewayRequestAdmission::new();
        let decoded = admission
            .admit(&bytes, &binding(), start)
            .expect("first request should be admitted");
        assert_eq!(decoded.request_id, "request-1");
        assert_eq!(decoded.database_request.activation_instance, "activation-1");
        assert!(admission.admit(&bytes, &binding(), start).is_err());
        for index in 2..=60 {
            let mut next = request();
            next.request_id = format!("request-{index}");
            let next_bytes = encode_request(&next).expect("request should encode");
            admission
                .admit(&next_bytes, &binding(), start)
                .expect("duplicate rejection must not consume rate capacity");
        }
        let mut exhausted = request();
        exhausted.request_id = "request-61".to_string();
        let exhausted_bytes = encode_request(&exhausted).expect("request should encode");
        assert!(admission
            .admit(&exhausted_bytes, &binding(), start)
            .is_err());

        let mut inactive = binding();
        inactive.state = GatewayActivationState::Stopped;
        assert!(admission.admit(&bytes, &inactive, start).is_err());
    }

    #[test]
    fn retires_rate_and_replay_state_for_replaced_activations() {
        let bytes = encode_request(&request()).expect("request should encode");
        let start = Instant::now();
        let mut admission = GatewayRequestAdmission::new();
        admission
            .admit(&bytes, &binding(), start)
            .expect("request should be admitted");

        let mut stopped = binding();
        stopped.state = GatewayActivationState::Replaced;
        admission
            .retire_activation(&stopped)
            .expect("replaced activation state should be retired");
        assert!(admission.admit(&bytes, &binding(), start).is_ok());
    }

    #[test]
    fn rejects_oversized_frames_without_parsing_them() {
        let bytes = vec![b' '; MAX_REQUEST_BYTES + 1];
        assert!(decode_request(&bytes, &binding()).is_err());
    }
}
