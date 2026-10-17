use crate::trusted_service_database::{
    DatabaseOperation, DatabaseRequest, DatabaseResponse, TrustedServiceDatabaseContract,
    MAX_REQUEST_BYTES,
};
use crate::trusted_service_runtime::EXECUTION_ENABLED;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
};
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(unix)]
use tokio::sync::Semaphore;
#[cfg(unix)]
use tokio::task::JoinSet;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum GatewayStorageOperation {
    Get { key: String },
    Put { key: String, value_json: String },
    Delete { key: String },
    List { prefix: String, limit: u32 },
}

pub const GATEWAY_PROTOCOL_VERSION: u16 = 1;
pub const GATEWAY_SOCKET_PATH: &str = "/run/screentra/gateway.sock";

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

pub fn encode_connection_hello_frame(hello: &GatewayConnectionHello) -> Result<Vec<u8>, String> {
    let payload = encode_connection_hello(hello)?;
    encode_gateway_frame(&payload, MAX_REQUEST_BYTES)
}

pub fn decode_connection_hello_frame(
    bytes: &[u8],
    binding: &GatewayActivationBinding,
) -> Result<Option<(GatewayConnectionHello, usize)>, String> {
    let Some((payload, consumed)) = decode_gateway_frame(bytes, MAX_REQUEST_BYTES)? else {
        return Ok(None);
    };
    Ok(Some((
        decode_connection_hello(&payload, binding)?,
        consumed,
    )))
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

fn encode_gateway_frame(payload: &[u8], maximum_bytes: usize) -> Result<Vec<u8>, String> {
    if payload.len() > maximum_bytes || payload.len() > u32::MAX as usize {
        return Err("Gateway frame is too large".to_string());
    }
    let mut frame = Vec::with_capacity(GATEWAY_FRAME_LENGTH_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

fn decode_gateway_frame(
    bytes: &[u8],
    maximum_bytes: usize,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    if bytes.len() < GATEWAY_FRAME_LENGTH_BYTES {
        return Ok(None);
    }
    let header: [u8; GATEWAY_FRAME_LENGTH_BYTES] = bytes[..GATEWAY_FRAME_LENGTH_BYTES]
        .try_into()
        .map_err(|_| "Gateway frame header is invalid".to_string())?;
    let frame_len = u32::from_be_bytes(header) as usize;
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

#[derive(Debug, Clone)]
pub struct GatewayActivationBinding {
    pub contract: TrustedServiceDatabaseContract,
    pub state: GatewayActivationState,
    credential_lease: Option<(
        Arc<Mutex<crate::trusted_service_credentials::CredentialLeaseLedger>>,
        String,
    )>,
}

impl GatewayActivationBinding {
    pub fn new(contract: TrustedServiceDatabaseContract) -> Self {
        Self {
            contract,
            state: GatewayActivationState::Active,
            credential_lease: None,
        }
    }

    pub fn with_credential_lease(
        mut self,
        ledger: Arc<Mutex<crate::trusted_service_credentials::CredentialLeaseLedger>>,
        lease_id: String,
    ) -> Result<Self, String> {
        if lease_id.is_empty() {
            return Err("Gateway credential lease ID is empty".to_string());
        }
        let scope = crate::trusted_service_credentials::CredentialScope {
            addon_id: self.contract.addon_id.clone(),
            activation_instance: self.contract.activation_instance.clone(),
            capabilities: vec!["storage".to_string()],
        };
        ledger
            .lock()
            .map_err(|_| "Gateway credential lease state is unavailable".to_string())?
            .validate_for_use(&lease_id, &scope, chrono::Utc::now().fixed_offset())
            .map_err(|error| error.to_string())?;
        self.credential_lease = Some((ledger, lease_id));
        Ok(self)
    }

    fn validate(&self) -> Result<(), String> {
        if self.state != GatewayActivationState::Active {
            return Err("Gateway activation is not active".to_string());
        }
        self.contract
            .validate()
            .map_err(|_| "Gateway activation contract is not permitted".to_string())?;
        if let Some((ledger, lease_id)) = &self.credential_lease {
            let scope = crate::trusted_service_credentials::CredentialScope {
                addon_id: self.contract.addon_id.clone(),
                activation_instance: self.contract.activation_instance.clone(),
                capabilities: vec!["storage".to_string()],
            };
            ledger
                .lock()
                .map_err(|_| "Gateway credential lease state is unavailable".to_string())?
                .validate_for_use(lease_id, &scope, chrono::Utc::now().fixed_offset())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
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

    pub fn admit_frame(
        &mut self,
        bytes: &[u8],
        binding: &GatewayActivationBinding,
        now: Instant,
    ) -> Result<Option<(DecodedGatewayRequest, usize)>, String> {
        let Some((payload, consumed)) = decode_gateway_frame(bytes, MAX_REQUEST_BYTES)? else {
            return Ok(None);
        };
        Ok(Some((self.admit(&payload, binding, now)?, consumed)))
    }

    pub fn retire_activation(&mut self, binding: &GatewayActivationBinding) -> Result<(), String> {
        self.rate_limiter.retire(&binding.contract)?;
        self.replay_guard.retire(&binding.contract)?;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct GatewayAdmissionRegistry {
    admission: GatewayRequestAdmission,
    retired: BTreeSet<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct GatewayAdmissionState {
    inner: Arc<Mutex<GatewayAdmissionRegistry>>,
}

impl GatewayAdmissionState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_active(&self, binding: &GatewayActivationBinding) -> Result<(), String> {
        let registry = self
            .inner
            .lock()
            .map_err(|_| "Gateway admission state is unavailable".to_string())?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        if registry.retired.contains(&key) {
            return Err("Gateway activation admission is retired".to_string());
        }
        Ok(())
    }

    fn admit(
        &self,
        bytes: &[u8],
        binding: &GatewayActivationBinding,
        now: Instant,
    ) -> Result<DecodedGatewayRequest, String> {
        let mut registry = self
            .inner
            .lock()
            .map_err(|_| "Gateway admission state is unavailable".to_string())?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        if registry.retired.contains(&key) {
            return Err("Gateway activation admission is retired".to_string());
        }
        registry.admission.admit(bytes, binding, now)
    }

    fn retire(&self, binding: &GatewayActivationBinding) -> Result<(), String> {
        let mut registry = self
            .inner
            .lock()
            .map_err(|_| "Gateway admission state is unavailable".to_string())?;
        let key = (
            binding.contract.addon_id.clone(),
            binding.contract.activation_instance.clone(),
        );
        if !registry.retired.contains(&key) && registry.retired.len() >= MAX_TRACKED_ACTIVATIONS {
            return Err("Gateway retired-activation state is full".to_string());
        }
        registry.admission.retire_activation(binding)?;
        registry.retired.insert(key);
        Ok(())
    }
}

#[derive(Debug)]
pub struct GatewaySession {
    binding: GatewayActivationBinding,
    peer_policy: GatewayPeerPolicy,
    admission: GatewayAdmissionState,
    budget: GatewayOperationBudget,
    established: bool,
    retired: bool,
}

impl GatewaySession {
    pub fn new(
        binding: GatewayActivationBinding,
        peer_policy: GatewayPeerPolicy,
        admission: GatewayAdmissionState,
    ) -> Self {
        Self {
            binding,
            peer_policy,
            admission,
            budget: GatewayOperationBudget::new(),
            established: false,
            retired: false,
        }
    }

    pub fn accept_hello_frame(
        &mut self,
        bytes: &[u8],
        peer: GatewayPeerCredentials,
    ) -> Result<Option<usize>, String> {
        if self.retired {
            return Err("Gateway session is retired".to_string());
        }
        if self.established {
            return Err("Gateway session is already established".to_string());
        }
        self.peer_policy.validate(peer)?;
        self.admission.ensure_active(&self.binding)?;
        let Some((_, consumed)) = decode_connection_hello_frame(bytes, &self.binding)? else {
            return Ok(None);
        };
        self.established = true;
        Ok(Some(consumed))
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
        self.admission.ensure_active(&self.binding)?;
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
        self.budget.check_at(now)?;
        let decoded = self.admission.admit(bytes, &self.binding, now)?;
        self.budget.take_at(now)?;
        Ok(decoded)
    }

    pub fn admit_frame(
        &mut self,
        bytes: &[u8],
        now: Instant,
    ) -> Result<Option<(DecodedGatewayRequest, usize)>, String> {
        if !self.established {
            return Err("Gateway connection hello is required".to_string());
        }
        let Some((payload, consumed)) = decode_gateway_frame(bytes, MAX_REQUEST_BYTES)? else {
            return Ok(None);
        };
        Ok(Some((self.admit(&payload, now)?, consumed)))
    }

    pub fn retire(&mut self) -> Result<(), String> {
        self.established = false;
        self.retired = true;
        self.admission.retire(&self.binding)
    }
}

/// Typed operation handler used by the Unix transport. The transport never
/// exposes a database connection or accepts an untyped command channel.
pub trait GatewayOperationHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        request: DatabaseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DatabaseResponse, String>> + Send + 'a>>;
}

#[derive(Debug, Clone, Default)]
pub struct GatewayReadiness {
    active_sessions: Arc<AtomicUsize>,
}

impl GatewayReadiness {
    pub fn is_ready(&self) -> bool {
        self.active_sessions.load(Ordering::Acquire) > 0
    }

    fn mark_ready(&self) -> GatewayReadinessLease {
        self.active_sessions.fetch_add(1, Ordering::AcqRel);
        GatewayReadinessLease {
            readiness: self.clone(),
        }
    }

    fn mark_not_ready(&self) {
        self.active_sessions.store(0, Ordering::Release);
    }
}

struct GatewayReadinessLease {
    readiness: GatewayReadiness,
}

impl Drop for GatewayReadinessLease {
    fn drop(&mut self) {
        let _ = self
            .readiness
            .active_sessions
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
    }
}

#[cfg(unix)]
pub const MAX_ACTIVE_GATEWAY_CONNECTIONS: usize = 64;

#[cfg(unix)]
pub struct TrustedServiceUnixGateway {
    binding: GatewayActivationBinding,
    peer_policy: GatewayPeerPolicy,
    admission: GatewayAdmissionState,
    socket_path: PathBuf,
    readiness: GatewayReadiness,
}

#[cfg(unix)]
struct GatewaySocketCleanup {
    path: PathBuf,
    device: u64,
    inode: u64,
    readiness: GatewayReadiness,
}

#[cfg(unix)]
impl Drop for GatewaySocketCleanup {
    fn drop(&mut self) {
        self.readiness.mark_not_ready();
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn remove_owned_gateway_socket(path: &Path, device: u64, inode: u64) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Gateway socket could not be inspected for removal".to_string()),
    };
    if !metadata.file_type().is_socket()
        || metadata.dev() != device
        || metadata.ino() != inode
    {
        return Err("Gateway socket identity changed before removal".to_string());
    }
    fs::remove_file(path).map_err(|_| "Gateway socket could not be removed".to_string())
}

#[cfg(unix)]
impl TrustedServiceUnixGateway {
    pub fn new(
        binding: GatewayActivationBinding,
        peer_policy: GatewayPeerPolicy,
    ) -> Result<Self, String> {
        binding.validate()?;
        let socket_path = PathBuf::from(GATEWAY_SOCKET_PATH);
        if !socket_path.is_absolute() || socket_path != Path::new(GATEWAY_SOCKET_PATH) {
            return Err("Gateway socket path is not fixed".to_string());
        }
        Ok(Self {
            binding,
            peer_policy,
            admission: GatewayAdmissionState::new(),
            socket_path,
            readiness: GatewayReadiness::default(),
        })
    }

    pub fn readiness(&self) -> GatewayReadiness {
        self.readiness.clone()
    }

    /// Start the listener only after the global execution gate is enabled.
    /// The current build intentionally returns before binding any socket.
    pub async fn run(
        self,
        handler: Arc<dyn GatewayOperationHandler>,
    ) -> Result<(), String> {
        self.run_until_shutdown(handler, std::future::pending()).await
    }

    pub async fn run_until_shutdown<S>(
        self,
        handler: Arc<dyn GatewayOperationHandler>,
        shutdown: S,
    ) -> Result<(), String>
    where
        S: Future<Output = ()> + Send + 'static,
    {
        if !EXECUTION_ENABLED {
            return Err("Trusted service gateway is disabled".to_string());
        }
        let parent = self
            .socket_path
            .parent()
            .ok_or_else(|| "Gateway socket parent is invalid".to_string())?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|_| "Gateway socket parent is unavailable".to_string())?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err("Gateway socket parent is not a real directory".to_string());
        }
        if fs::symlink_metadata(&self.socket_path).is_ok() {
            return Err("Gateway socket path already exists".to_string());
        }
        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|_| "Gateway socket could not be bound".to_string())?;
        let socket_metadata = fs::symlink_metadata(&self.socket_path)
            .map_err(|_| "Gateway socket could not be inspected".to_string())?;
        if socket_metadata.file_type().is_symlink() || !socket_metadata.file_type().is_socket() {
            return Err("Gateway socket is not a Unix socket".to_string());
        }
        let _socket_cleanup = GatewaySocketCleanup {
            path: self.socket_path.clone(),
            device: socket_metadata.dev(),
            inode: socket_metadata.ino(),
            readiness: self.readiness.clone(),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = socket_metadata.permissions();
            permissions.set_mode(0o660);
            fs::set_permissions(&self.socket_path, permissions)
                .map_err(|_| "Gateway socket permissions could not be set".to_string())?;
        }
        let permits = Arc::new(Semaphore::new(MAX_ACTIVE_GATEWAY_CONNECTIONS));
        let readiness = self.readiness.clone();
        let mut tasks = JoinSet::new();
        let mut shutdown = Box::pin(shutdown);
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(_)) = joined {
                        // Individual connection failures are isolated from the listener.
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted
                        .map_err(|_| "Gateway socket accept failed".to_string())?;
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        continue;
                    };
                    let binding = self.binding.clone();
                    let peer_policy = self.peer_policy;
                    let admission = self.admission.clone();
                    let handler = handler.clone();
                    let readiness = readiness.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let _ = handle_unix_connection(
                            stream,
                            binding,
                            peer_policy,
                            admission,
                            handler,
                            readiness,
                        )
                        .await;
                    });
                }
            }
        };
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        drop(listener);
        if result.is_ok() {
            remove_owned_gateway_socket(
                &self.socket_path,
                socket_metadata.dev(),
                socket_metadata.ino(),
            )?;
        }
        result
    }
}

#[cfg(unix)]
async fn read_unix_frame(stream: &mut UnixStream, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    let mut header = [0u8; GATEWAY_FRAME_LENGTH_BYTES];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| "Gateway frame header could not be read".to_string())?;
    let frame_len = u32::from_be_bytes(header) as usize;
    if frame_len > maximum_bytes {
        return Err("Gateway frame is too large".to_string());
    }
    let mut payload = vec![0u8; frame_len];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|_| "Gateway frame payload could not be read".to_string())?;
    Ok(payload)
}

#[cfg(target_os = "linux")]
fn unix_peer_credentials(stream: &UnixStream) -> Result<GatewayPeerCredentials, String> {
    use std::mem::size_of;
    use std::os::unix::io::AsRawFd;
    let mut credentials = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 || length as usize != size_of::<libc::ucred>() {
        return Err("Gateway peer credentials are unavailable".to_string());
    }
    Ok(GatewayPeerCredentials::from_os_credentials(
        credentials.uid,
        credentials.gid,
        credentials.pid as u32,
    ))
}

#[cfg(not(target_os = "linux"))]
fn unix_peer_credentials(_stream: &UnixStream) -> Result<GatewayPeerCredentials, String> {
    Err("Gateway peer credentials are unsupported on this platform".to_string())
}

#[cfg(unix)]
async fn handle_unix_connection(
    mut stream: UnixStream,
    binding: GatewayActivationBinding,
    peer_policy: GatewayPeerPolicy,
    admission: GatewayAdmissionState,
    handler: Arc<dyn GatewayOperationHandler>,
    readiness: GatewayReadiness,
) -> Result<(), String> {
    let peer = unix_peer_credentials(&stream)?;
    let hello = tokio::time::timeout(
        MAX_REQUEST_DURATION,
        read_unix_frame(&mut stream, MAX_REQUEST_BYTES),
    )
    .await
    .map_err(|_| "Gateway hello timed out".to_string())??;
    let mut session = GatewaySession::new(binding, peer_policy, admission);
    session.accept_hello(&hello, peer)?;
    let _readiness_lease = readiness.mark_ready();
    loop {
        let request = tokio::time::timeout(
            MAX_REQUEST_DURATION,
            read_unix_frame(&mut stream, MAX_REQUEST_BYTES),
        )
        .await
        .map_err(|_| "Gateway request timed out".to_string())??;
        let decoded = session.admit(&request, Instant::now())?;
        let response = tokio::time::timeout(
            MAX_REQUEST_DURATION,
            handler.handle(decoded.database_request),
        )
        .await
        .map_err(|_| "Gateway operation timed out".to_string())?
        .map_err(|_| "Gateway operation failed".to_string())?;
        let frame = encode_response_frame(&response)?;
        stream
            .write_all(&frame)
            .await
            .map_err(|_| "Gateway response could not be written".to_string())?;
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

pub fn decode_response(bytes: &[u8]) -> Result<DatabaseResponse, String> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("Gateway response is too large".to_string());
    }
    let envelope: GatewayResponseEnvelope =
        serde_json::from_slice(bytes).map_err(|_| "Invalid gateway response".to_string())?;
    Ok(match envelope {
        GatewayResponseEnvelope::Value { value_json } => DatabaseResponse::Value(value_json),
        GatewayResponseEnvelope::Written => DatabaseResponse::Written,
        GatewayResponseEnvelope::Deleted { deleted } => DatabaseResponse::Deleted(deleted),
        GatewayResponseEnvelope::Entries { entries } => DatabaseResponse::Entries(entries),
    })
}

pub fn decode_response_frame(bytes: &[u8]) -> Result<Option<(DatabaseResponse, usize)>, String> {
    let Some((payload, consumed)) = decode_gateway_frame(bytes, MAX_RESPONSE_BYTES)? else {
        return Ok(None);
    };
    Ok(Some((decode_response(&payload)?, consumed)))
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
        self.check_at(now)?;
        self.used += 1;
        Ok(())
    }

    fn check_at(&self, now: Instant) -> Result<(), String> {
        if now >= self.deadline {
            return Err("Gateway request deadline exceeded".to_string());
        }
        if self.used >= MAX_OPERATIONS_PER_CONNECTION {
            return Err("Gateway operation budget exceeded".to_string());
        }
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
        decode_connection_hello, decode_connection_hello_frame, decode_gateway_frame,
        decode_request, decode_request_frame, decode_response_frame, encode_connection_hello,
        encode_connection_hello_frame, encode_gateway_frame, encode_request, encode_request_frame,
        encode_response, encode_response_frame, validate_connection_hello,
        GatewayActivationBinding, GatewayActivationState, GatewayAdmissionState,
        GatewayConnectionHello, GatewayOperationBudget, GatewayOperationHandler, GatewayReadiness,
        GatewayPeerCredentials, GatewayPeerPolicy,
        GatewayRateLimiter, GatewayReplayGuard, GatewayRequestAdmission, GatewayRequestEnvelope,
        GatewaySession, GatewayStorageOperation, TrustedServiceUnixGateway,
        GATEWAY_PROTOCOL_VERSION,
        MAX_OPERATIONS_PER_CONNECTION, MAX_REQUESTS_PER_ACTIVATION, MAX_RESPONSE_BYTES,
        MAX_TRACKED_ACTIVATIONS,
    };
    use crate::trusted_service_database::{
        DatabaseResponse, TrustedServiceDatabaseContract, MAX_REQUEST_BYTES,
    };
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn contract() -> TrustedServiceDatabaseContract {
        TrustedServiceDatabaseContract::core_storage_api("example.weather", "activation-1")
    }

    fn binding() -> GatewayActivationBinding {
        GatewayActivationBinding::new(contract())
    }

    #[cfg(unix)]
    struct NoopGatewayHandler;

    #[cfg(unix)]
    impl GatewayOperationHandler for NoopGatewayHandler {
        fn handle<'a>(
            &'a self,
            _request: crate::trusted_service_database::DatabaseRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DatabaseResponse, String>> + Send + 'a>> {
            Box::pin(async { Ok(DatabaseResponse::Written) })
        }
    }

    #[test]
    fn gateway_credential_lease_binding_rejects_revoked_requests() {
        let lease = crate::trusted_service_credentials::CredentialLease {
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
            capabilities: vec!["storage".to_string()],
            lease_id: "gateway-lease-1".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
        };
        let mut ledger = crate::trusted_service_credentials::CredentialLeaseLedger::default();
        ledger
            .register(lease)
            .expect("future metadata lease should register");
        let ledger = Arc::new(std::sync::Mutex::new(ledger));
        let binding = GatewayActivationBinding::new(contract())
            .with_credential_lease(ledger.clone(), "gateway-lease-1".to_string())
            .expect("matching credential lease should bind");
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let peer = GatewayPeerCredentials::from_os_credentials(1000, 1001, 42);
        let policy = GatewayPeerPolicy::new(1000, 1001, 42).expect("peer policy");
        let mut session = GatewaySession::new(binding, policy, GatewayAdmissionState::new());
        session
            .accept_hello(&encode_connection_hello(&hello).expect("hello"), peer)
            .expect("active lease should establish the session");
        ledger
            .lock()
            .expect("ledger lock")
            .revoke("gateway-lease-1")
            .expect("revoke lease");
        assert!(session.admit(&encode_request(&request()).expect("request"), Instant::now()).is_err());
    }

    #[test]
    fn gateway_readiness_requires_a_valid_hello() {
        let readiness = GatewayReadiness::default();
        assert!(!readiness.is_ready());
        let first = readiness.mark_ready();
        assert!(readiness.is_ready());
        let second = readiness.mark_ready();
        assert!(readiness.is_ready());
        drop(first);
        assert!(readiness.is_ready());
        drop(second);
        assert!(!readiness.is_ready());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_gateway_never_binds_while_execution_is_disabled() {
        let peer_policy = GatewayPeerPolicy::new(1000, 1000, 1).expect("peer policy");
        let gateway = TrustedServiceUnixGateway::new(binding(), peer_policy)
            .expect("disabled gateway configuration should validate");
        let error = gateway
            .run(Arc::new(NoopGatewayHandler))
            .await
            .expect_err("disabled gateway must not bind");
        assert!(error.contains("disabled"));
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
    fn typed_response_frame_wrapper_round_trips_one_frame_at_a_time() {
        let response = DatabaseResponse::Entries(vec![
            ("alpha".to_string(), "{\"ok\":true}".to_string()),
            ("beta".to_string(), String::new()),
        ]);
        let first = encode_response_frame(&response).expect("response frame should encode");
        let second = encode_response_frame(&DatabaseResponse::Written)
            .expect("second response frame should encode");
        let mut combined = first.clone();
        combined.extend_from_slice(&second);

        let (decoded, consumed) = decode_response_frame(&combined)
            .unwrap()
            .expect("first response frame should decode");
        assert_eq!(decoded, response);
        assert_eq!(consumed, first.len());
        let (decoded_second, consumed_second) = decode_response_frame(&combined[consumed..])
            .unwrap()
            .expect("second response frame should decode");
        assert_eq!(decoded_second, DatabaseResponse::Written);
        assert_eq!(consumed_second, second.len());
    }

    #[test]
    fn response_encoding_rejects_oversized_entries_before_serialization() {
        let response = DatabaseResponse::Entries(vec![(
            "oversized".to_string(),
            "x".repeat(MAX_RESPONSE_BYTES),
        )]);
        assert!(encode_response(&response).is_err());
        assert!(encode_response_frame(&response).is_err());
    }

    #[test]
    fn response_frame_decoder_rejects_oversized_length_before_payload_read() {
        let oversized_length = ((MAX_RESPONSE_BYTES + 1) as u32).to_be_bytes();
        assert!(decode_response_frame(&oversized_length).is_err());
    }

    #[test]
    fn session_accepts_framed_hello_only_after_peer_validation() {
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let frame = encode_connection_hello_frame(&hello).expect("hello frame should encode");
        let policy = GatewayPeerPolicy::new(1000, 1001, 42).expect("policy should be valid");
        let peer = GatewayPeerCredentials::from_os_credentials(1000, 1001, 42);
        let mut session = GatewaySession::new(binding(), policy, GatewayAdmissionState::new());

        assert_eq!(session.accept_hello_frame(&frame[..2], peer).unwrap(), None);
        assert!(!session.is_established());
        assert!(session
            .accept_hello_frame(
                &frame,
                GatewayPeerCredentials::from_os_credentials(1002, 1001, 42),
            )
            .is_err());
        assert!(!session.is_established());
        assert_eq!(
            session.accept_hello_frame(&frame, peer).unwrap(),
            Some(frame.len())
        );
        assert!(session.is_established());
        let request_frame = encode_request_frame(&request()).expect("request frame should encode");
        let (decoded_request, consumed) = session
            .admit_frame(&request_frame, Instant::now())
            .unwrap()
            .expect("request frame should be admitted after hello");
        assert_eq!(decoded_request.request_id, "request-1");
        assert_eq!(consumed, request_frame.len());
        session.retire().expect("session should retire");
        assert!(session.admit_frame(&request_frame, Instant::now()).is_err());
        assert!(session.accept_hello_frame(&frame, peer).is_err());
    }

    #[test]
    fn typed_hello_frame_wrapper_preserves_binding_and_partial_reads() {
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let frame = encode_connection_hello_frame(&hello).expect("hello frame should encode");
        assert_eq!(
            decode_connection_hello_frame(&frame[..2], &binding()).unwrap(),
            None
        );
        let (decoded, consumed) = decode_connection_hello_frame(&frame, &binding())
            .unwrap()
            .expect("complete hello frame should decode");
        assert_eq!(decoded, hello);
        assert_eq!(consumed, frame.len());
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
        let (response, consumed) = decode_response_frame(&response_frame)
            .unwrap()
            .expect("response frame should decode");
        assert_eq!(response, DatabaseResponse::Written);
        assert_eq!(consumed, response_frame.len());
    }

    #[test]
    fn framed_admission_waits_for_complete_payload_before_consuming_budget() {
        let frame = encode_request_frame(&request()).expect("request frame should encode");
        let mut admission = GatewayRequestAdmission::new();
        assert_eq!(
            admission
                .admit_frame(&frame[..3], &binding(), Instant::now())
                .unwrap(),
            None
        );
        let (decoded, consumed) = admission
            .admit_frame(&frame, &binding(), Instant::now())
            .unwrap()
            .expect("complete request frame should be admitted");
        assert_eq!(decoded.request_id, "request-1");
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn session_rejects_invalid_requests_without_consuming_operation_budget() {
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
        let mut session = GatewaySession::new(binding(), peer_policy, GatewayAdmissionState::new());
        session
            .accept_hello(&hello_bytes, peer)
            .expect("hello should establish session");

        for _ in 0..MAX_OPERATIONS_PER_CONNECTION {
            assert!(session.admit(b"not-json", start).is_err());
        }
        let request_bytes = encode_request(&request()).expect("request should encode");
        assert!(session.admit(&request_bytes, start).is_ok());
    }

    #[test]
    fn shared_admission_state_enforces_limits_across_sessions() {
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let hello_bytes = encode_connection_hello(&hello).expect("hello should encode");
        let peer = GatewayPeerCredentials::from_os_credentials(1000, 1001, 42);
        let peer_policy = GatewayPeerPolicy::new(1000, 1001, 42).expect("policy should be valid");
        let shared = GatewayAdmissionState::new();
        let mut first = GatewaySession::new(binding(), peer_policy, shared.clone());
        let mut second = GatewaySession::new(binding(), peer_policy, shared);
        first
            .accept_hello(&hello_bytes, peer)
            .expect("first hello should establish");
        second
            .accept_hello(&hello_bytes, peer)
            .expect("second hello should establish");

        for index in 0..MAX_REQUESTS_PER_ACTIVATION {
            let mut request = request();
            request.request_id = format!("shared-{index}");
            let bytes = encode_request(&request).expect("request should encode");
            let session = if index % 2 == 0 {
                &mut first
            } else {
                &mut second
            };
            session
                .admit(&bytes, Instant::now())
                .expect("shared activation quota should admit the first 60 requests");
        }

        let mut rejected = request();
        rejected.request_id = "shared-rejected".to_string();
        let bytes = encode_request(&rejected).expect("request should encode");
        assert!(first.admit(&bytes, Instant::now()).is_err());
    }

    #[test]
    fn retiring_one_session_retires_shared_activation_admission() {
        let hello = GatewayConnectionHello {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
        };
        let hello_bytes = encode_connection_hello(&hello).expect("hello should encode");
        let peer = GatewayPeerCredentials::from_os_credentials(1000, 1001, 42);
        let peer_policy = GatewayPeerPolicy::new(1000, 1001, 42).expect("policy should be valid");
        let shared = GatewayAdmissionState::new();
        let mut first = GatewaySession::new(binding(), peer_policy, shared.clone());
        let mut second = GatewaySession::new(binding(), peer_policy, shared.clone());
        let mut third = GatewaySession::new(binding(), peer_policy, shared);
        first
            .accept_hello(&hello_bytes, peer)
            .expect("first hello should establish");
        second
            .accept_hello(&hello_bytes, peer)
            .expect("second hello should establish");
        first
            .retire()
            .expect("retirement should clear shared state");
        assert!(third.accept_hello(&hello_bytes, peer).is_err());

        let bytes = encode_request(&request()).expect("request should encode");
        assert!(second.admit(&bytes, Instant::now()).is_err());
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
        let mut session = GatewaySession::new(binding(), peer_policy, GatewayAdmissionState::new());
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
        assert_eq!(budget.used, 1);
        budget
            .take_at(start + Duration::from_secs(6))
            .expect_err("expired requests must remain rejected without changing the budget");
        assert_eq!(budget.used, 1);
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
