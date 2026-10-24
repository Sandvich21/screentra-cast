// HMAC-signed token + session cookie auth for the signage admin UI.
use crate::db::{Database, UserRecord};
use crate::postgres::PostgresStore;
use crate::rate_limit::{Decision, RateLimiter, POLICY_AUTH_LOGIN};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand_core::OsRng;
use axum::{
    extract::{Extension, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

const MAX_SIGNED_TOKEN_BYTES: usize = 16 * 1024;
const MAX_SIGNED_PAYLOAD_BYTES: usize = 8 * 1024;
const MAX_GOOGLE_RESPONSE_BYTES: usize = 64 * 1024;

async fn read_google_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GOOGLE_RESPONSE_BYTES as u64)
    {
        return Err("Google response exceeds size limit".to_string());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_GOOGLE_RESPONSE_BYTES {
            return Err("Google response exceeds size limit".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[derive(Clone)]
pub struct AuthState {
    key: Vec<u8>,
    ttl: Duration,
    revoked: Arc<RwLock<HashSet<String>>>,
    login_limiter: Arc<RateLimiter>,
    repository: Arc<Mutex<Option<AuthRepository>>>,
}

#[derive(Clone)]
pub struct AuthAppState {
    pub auth: AuthState,
    pub repository: AuthRepository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Active,
    Revoked,
    MissingOrExpired,
}

/// Backend-neutral authentication repository. SQLite remains the default
/// adapter while PostgreSQL is introduced behind the same async boundary.
#[derive(Clone)]
pub enum AuthRepository {
    Sqlite(Arc<Database>),
    Postgres(Arc<PostgresStore>),
}

impl AuthRepository {
    async fn find_active_user_credentials(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String)>, String> {
        match self {
            Self::Sqlite(db) => db.find_active_user_credentials(username),
            Self::Postgres(db) => db.find_active_user_credentials(username).await,
        }
    }

    async fn record_user_login(&self, username: &str, now: &str) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.record_user_login(username, now),
            Self::Postgres(db) => db.record_user_login(username, now).await,
        }
    }

    async fn get_user_account(
        &self,
        username: &str,
    ) -> Result<Option<(String, String, String, bool, Option<String>)>, String> {
        match self {
            Self::Sqlite(db) => db.get_user_account(username),
            Self::Postgres(db) => db.get_user_account(username).await,
        }
    }

    async fn get_active_password_hash(&self, username: &str) -> Result<Option<String>, String> {
        match self {
            Self::Sqlite(db) => db.get_active_password_hash(username),
            Self::Postgres(db) => db.get_active_password_hash(username).await,
        }
    }

    async fn update_user_password(
        &self,
        username: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.update_user_password(username, password_hash, now),
            Self::Postgres(db) => db.update_user_password(username, password_hash, now).await,
        }
    }

    async fn clear_google_binding(&self, username: &str, now: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.clear_google_binding(username, now),
            Self::Postgres(db) => db.clear_google_binding(username, now).await,
        }
    }

    async fn bind_google_account(
        &self,
        username: &str,
        google_sub: &str,
        google_email: &str,
        now: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.bind_google_account(username, google_sub, google_email, now),
            Self::Postgres(db) => {
                db.bind_google_account(username, google_sub, google_email, now)
                    .await
            }
        }
    }

    async fn find_active_google_user(
        &self,
        google_sub: &str,
    ) -> Result<Option<(String, String)>, String> {
        match self {
            Self::Sqlite(db) => db.find_active_google_user(google_sub),
            Self::Postgres(db) => db.find_active_google_user(google_sub).await,
        }
    }

    async fn session_state(&self, token: &str) -> Result<SessionState, String> {
        match self {
            Self::Sqlite(db) => db
                .session_state(token)
                .map(|state| match state {
                    Some(true) => SessionState::Revoked,
                    Some(false) => SessionState::Active,
                    None => SessionState::MissingOrExpired,
                }),
            Self::Postgres(db) => db.session_state(token).await.map(|state| match state {
                Some(true) => SessionState::Revoked,
                Some(false) => SessionState::Active,
                None => SessionState::MissingOrExpired,
            }),
        }
    }

    async fn revoke_session(&self, token: &str) -> Result<u64, String> {
        match self {
            Self::Sqlite(db) => {
                db.revoke_session(token);
                Ok(1)
            }
            Self::Postgres(db) => db.revoke_session(token).await,
        }
    }

    pub(crate) async fn session_principal(
        &self,
        token: &str,
    ) -> Result<Option<(String, Option<String>, Option<bool>)>, String> {
        match self {
            Self::Sqlite(db) => db.session_principal(token),
            Self::Postgres(db) => db.session_principal(token).await,
        }
    }

    pub(crate) async fn create_session(
        &self,
        token: &str,
        username: &str,
        expires_at: i64,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_session(token, username, expires_at),
            Self::Postgres(db) => db.create_session(token, username, expires_at).await,
        }
    }

    pub(crate) async fn purge_expired_sessions(&self) -> Result<u64, String> {
        match self {
            Self::Sqlite(db) => db.purge_expired_sessions().map(|count| count as u64),
            Self::Postgres(db) => db.purge_expired_sessions().await,
        }
    }

    pub(crate) async fn list_users(&self) -> Result<Vec<UserRecord>, String> {
        match self {
            Self::Sqlite(db) => db.list_users(),
            Self::Postgres(db) => db.list_users().await,
        }
    }

    pub(crate) async fn get_user(&self, id: &str) -> Result<Option<UserRecord>, String> {
        match self {
            Self::Sqlite(db) => db.get_user(id),
            Self::Postgres(db) => db.get_user(id).await,
        }
    }

    pub(crate) async fn count_active_admins(&self) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.count_active_admins(),
            Self::Postgres(db) => db.count_active_admins().await,
        }
    }

    pub(crate) async fn record_audit_event(
        &self,
        actor: &str,
        role: &str,
        event_type: &str,
        target: &str,
        outcome: &str,
        detail: &str,
        occurred_at: &str,
    ) -> Result<i64, String> {
        match self {
            Self::Sqlite(db) => db.record_audit_event(
                Some(actor),
                Some(role),
                event_type,
                Some(target),
                outcome,
                Some(detail),
                occurred_at,
            ),
            Self::Postgres(db) => db
                .record_audit_event(
                    Some(actor),
                    Some(role),
                    event_type,
                    Some(target),
                    outcome,
                    Some(detail),
                    occurred_at,
                )
                .await,
        }
    }

    pub(crate) async fn create_user(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        role: &str,
        password_hash: &str,
        active: bool,
        now: &str,
    ) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.create_user(id, username, display_name, role, password_hash, active, now),
            Self::Postgres(db) => db.create_user(id, username, display_name, role, password_hash, active, now).await,
        }
    }

    pub(crate) async fn create_initial_admin(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        password_hash: &str,
        now: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.create_initial_admin(id, username, display_name, password_hash, now),
            Self::Postgres(db) => db
                .create_initial_admin(id, username, display_name, password_hash, now)
                .await,
        }
    }

    pub(crate) async fn update_user(
        &self,
        id: &str,
        username: &str,
        display_name: &str,
        role: &str,
        password_hash: Option<&str>,
        active: bool,
        now: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.update_user(id, username, display_name, role, password_hash, active, now),
            Self::Postgres(db) => db.update_user(id, username, display_name, role, password_hash, active, now).await,
        }
    }

    pub(crate) async fn delete_user_record(&self, id: &str) -> Result<bool, String> {
        match self {
            Self::Sqlite(db) => db.delete_user(id),
            Self::Postgres(db) => db.delete_user_record(id).await,
        }
    }
}

impl AuthState {
    /// Bootstraps AuthState from env — no DB access needed yet.
    pub fn from_env() -> Self {
        let allow_default = matches!(
            std::env::var("SIGNAGE_ALLOW_DEFAULT_PASSWORD")
                .ok()
                .as_deref(),
            Some("1") | Some("true") | Some("yes")
        );
        let configured_hash = std::env::var("SIGNAGE_ADMIN_PASSWORD_HASH")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let configured_password = std::env::var("SIGNAGE_ADMIN_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty());
        let hash_set = configured_hash.is_some();
        let pw_set = configured_password.is_some();
        if !hash_set && !pw_set && !allow_default {
            eprintln!(
                "FATAL: no admin password configured. Set SIGNAGE_ADMIN_PASSWORD or \
                 SIGNAGE_ADMIN_PASSWORD_HASH, or pass SIGNAGE_ALLOW_DEFAULT_PASSWORD=1 to \
                 boot with the legacy default (NOT RECOMMENDED for any non-dev deployment)."
            );
            std::process::exit(2);
        }
        let raw = configured_password.unwrap_or_else(|| "admin".to_string());
        let key_input = configured_hash.unwrap_or_else(|| {
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            format!("{:x}", h.finalize())
        });
        // Loud warning when the literal "admin" password is in use.
        if !hash_set && raw == "admin" {
            eprintln!(
                "WARNING: SIGNAGE_ADMIN_PASSWORD is the literal 'admin'. This is the \
                 default for development only. Set a strong password before any non-dev \
                 deployment."
            );
        }
        let mut mac =
            HmacSha256::new_from_slice(key_input.as_bytes()).expect("HMAC accepts any key length");
        mac.update(b"ripley-signage/v1");
        let key = mac.finalize().into_bytes().to_vec();
        let ttl = std::env::var("SIGNAGE_SESSION_TTL_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15 * 60); // default 15 minutes
        AuthState {
            key,
            ttl: Duration::from_secs(ttl),
            revoked: Arc::new(RwLock::new(HashSet::new())),
            login_limiter: Arc::new(RateLimiter::new()),
            repository: Arc::new(Mutex::new(None)),
        }
    }

    pub fn load_from_repository(&self, repository: AuthRepository) {
        let mut guard = self.repository.lock().unwrap();
        *guard = Some(repository);
    }

    fn repository(&self) -> Option<AuthRepository> {
        self.repository.lock().unwrap().clone()
    }

    pub fn login_limiter(&self) -> Arc<RateLimiter> {
        self.login_limiter.clone()
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl.as_secs()
    }

    pub fn issue_for(&self, username: &str, role: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let exp = now + self.ttl.as_secs();
        let payload = serde_json::json!({ "sub": username, "role": role, "exp": exp, "iat": now })
            .to_string();
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let body = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{}.{}", body, sig)
    }

    fn decode_signed_payload(&self, token: &str) -> Option<serde_json::Value> {
        if token.len() > MAX_SIGNED_TOKEN_BYTES {
            return None;
        }
        let (body, sig) = token.rsplit_once('.')?;
        if body.len() > MAX_SIGNED_PAYLOAD_BYTES * 2 || sig.len() > 256 {
            return None;
        }
        let payload_bytes: Vec<u8> = URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
        if payload_bytes.len() > MAX_SIGNED_PAYLOAD_BYTES {
            return None;
        }
        let sig_bytes: Vec<u8> = URL_SAFE_NO_PAD.decode(sig.as_bytes()).ok()?;
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(&payload_bytes);
        let expected = mac.finalize().into_bytes();
        if expected.ct_eq(&sig_bytes).unwrap_u8() != 1 {
            return None;
        }
        serde_json::from_slice(&payload_bytes).ok()
    }

    pub fn issue_step_up_for(&self, username: &str, scope: &str) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let exp = now + Self::step_up_ttl(scope);
        let payload = serde_json::json!({ "sub": username, "scope": scope, "exp": exp }).to_string();
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let body = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{}.{}", body, sig)
    }

    pub fn verify_step_up(&self, token: &str, username: &str, scope: &str) -> bool {
        let Some(payload) = self.decode_signed_payload(token) else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        payload.get("exp").and_then(|v| v.as_u64()).is_some_and(|exp| exp > now)
            && payload.get("sub").and_then(|v| v.as_str()) == Some(username)
            && payload.get("scope").and_then(|v| v.as_str()) == Some(scope)
    }

    pub fn step_up_cookie_value(&self, username: &str, scope: &str) -> String {
        let ttl = Self::step_up_ttl(scope);
        format!(
            "screentra_cast_step_up={}; Path=/; Max-Age={}; HttpOnly; Secure; SameSite=Strict",
            self.issue_step_up_for(username, scope),
            ttl
        )
    }

    pub fn clear_step_up_cookie_value() -> &'static str {
        "screentra_cast_step_up=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict"
    }

    pub fn step_up_ttl(_scope: &str) -> u64 {
        300
    }

    pub fn supported_step_up_scope(scope: &str) -> bool {
        matches!(scope, "server-update" | "addon-execution-approval")
    }

    pub async fn token_payload(&self, token: &str) -> Option<serde_json::Value> {
        if !self.verify_persistent(token).await {
            return None;
        }
        self.decode_signed_payload(token)
    }

    pub fn verify(&self, token: &str) -> bool {
        let Some(v) = self.decode_signed_payload(token) else {
            return false;
        };
        let Some(exp) = v.get("exp").and_then(|x| x.as_u64()) else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        if exp <= now {
            return false;
        }
        if self
            .revoked
            .read()
            .map(|s| s.contains(token))
            .unwrap_or(false)
        {
            return false;
        }
        true
    }

    pub async fn verify_persistent(&self, token: &str) -> bool {
        if !self.verify(token) {
            return false;
        }
        let Some(repository) = self.repository() else {
            return true;
        };
        let Some(payload) = self.decode_signed_payload(token) else {
            return false;
        };
        let token_user = payload.get("sub").and_then(|v| v.as_str()).unwrap_or("");
        let token_role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let Ok(Some((session_user, user_role, active))) = repository.session_principal(token).await else {
            return false;
        };
        if !session_user.eq_ignore_ascii_case(token_user) {
            return false;
        }
        match (user_role, active) {
            (Some(role), Some(true)) => role == token_role,
            // Preserve the legacy environment-admin session only while there
            // is no active database administrator. Once a real admin exists,
            // the environment fallback cannot remain a backdoor.
            (None, None) => {
                token_user.eq_ignore_ascii_case("admin")
                    && token_role == "admin"
                    && repository.count_active_admins().await.ok() == Some(0)
            }
            _ => false,
        }
    }

    pub fn revoke(&self, token: &str) {
        if token.len() > MAX_SIGNED_TOKEN_BYTES {
            return;
        }
        // In-memory
        if let Ok(mut set) = self.revoked.write() {
            set.insert(token.to_string());
        }
    }

    pub async fn revoke_persistent(&self, token: &str) {
        self.revoke(token);
        if let Some(repository) = self.repository() {
            if let Err(error) = repository.revoke_session(token).await {
                log::warn!("auth: failed to persist token revocation: {error}");
            }
        }
    }

    pub fn gc_revocations(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        let mut drop = Vec::new();
        if let Ok(set) = self.revoked.read() {
            for tok in set.iter() {
                if let Some((body, _)) = tok.rsplit_once('.') {
                    if body.len() > MAX_SIGNED_PAYLOAD_BYTES * 2 {
                        continue;
                    }
                    let payload_bytes: Vec<u8> = match URL_SAFE_NO_PAD.decode(body.as_bytes()) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };
                    if payload_bytes.len() > MAX_SIGNED_PAYLOAD_BYTES {
                        continue;
                    }
                    let payload_str = match std::str::from_utf8(&payload_bytes) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let v: serde_json::Value = match serde_json::from_str(payload_str) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(exp) = v.get("exp").and_then(|x| x.as_u64()) {
                        if exp <= now {
                            drop.push(tok.clone());
                        }
                    }
                }
            }
        }
        if let Ok(mut set) = self.revoked.write() {
            for d in drop {
                set.remove(&d);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct DbLoginUser {
    username: String,
    role: String,
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2 password hashing failed")
        .to_string()
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    if let Ok(parsed) = PasswordHash::new(stored) {
        return Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
    }

    // Legacy records used sha256:<salt>:<hex digest>. Keep accepting this
    // format so existing accounts can migrate without a forced reset.
    let Some(rest) = stored.strip_prefix("sha256:") else {
        return false;
    };
    let Some((salt, expected)) = rest.split_once(':') else {
        return false;
    };
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    let got = format!("{:x}", h.finalize());
    got.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1
}

fn password_hash_needs_upgrade(stored: &str) -> bool {
    !stored.starts_with("$argon2id$")
}

async fn authenticate_db_user(
    repository: &AuthRepository,
    username: &str,
    password: &str,
) -> Option<DbLoginUser> {
    let user = username.trim();
    if user.is_empty() {
        return None;
    }
    let row = repository
        .find_active_user_credentials(user)
        .await
        .ok()
        .flatten()?;
    if !verify_password(password, &row.2) {
        return None;
    }
    let now = chrono::Utc::now().to_rfc3339();
    if password_hash_needs_upgrade(&row.2) {
        let upgraded = hash_password(password);
        if let Err(error) = repository
            .update_user_password(&row.0, &upgraded, &now)
            .await
        {
            log::warn!("auth: legacy password hash upgrade failed for {}: {}", row.0, error);
        }
    }
    let _ = repository.record_user_login(&row.0, &now).await;
    Some(DbLoginUser {
        username: row.0,
        role: row.1,
    })
}

fn verify_env_admin_password(password: &str) -> bool {
    let expected = std::env::var("SIGNAGE_ADMIN_PASSWORD_HASH")
        .ok()
        .unwrap_or_else(|| {
            let raw =
                std::env::var("SIGNAGE_ADMIN_PASSWORD").unwrap_or_else(|_| "admin".to_string());
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            format!("{:x}", h.finalize())
        });
    let mut provided = Sha256::new();
    provided.update(password.as_bytes());
    let provided_hex = format!("{:x}", provided.finalize());
    provided_hex
        .as_bytes()
        .ct_eq(expected.as_bytes())
        .unwrap_u8()
        == 1
}

async fn current_token_payload(
    auth: &AuthState,
    headers: &HeaderMap,
) -> Option<serde_json::Value> {
    let token = extract_token(headers)?;
    auth.token_payload(&token).await
}

async fn current_username(auth: &AuthState, headers: &HeaderMap) -> Option<String> {
    current_token_payload(auth, headers)
        .await
        .as_ref()
        .and_then(|p| p.get("sub"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}

fn google_client_id() -> Option<String> {
    std::env::var("SIGNAGE_GOOGLE_CLIENT_ID")
        .ok()
        .or_else(|| std::env::var("GOOGLE_CLIENT_ID").ok())
        .filter(|v| !v.trim().is_empty())
}
fn google_client_secret() -> Option<String> {
    std::env::var("SIGNAGE_GOOGLE_CLIENT_SECRET")
        .ok()
        .or_else(|| std::env::var("GOOGLE_CLIENT_SECRET").ok())
        .filter(|v| !v.trim().is_empty())
}
fn google_configured() -> bool {
    google_client_id().is_some() && google_client_secret().is_some()
}

fn public_base(headers: &HeaderMap) -> String {
    if let Ok(v) = std::env::var("SIGNAGE_PUBLIC_URL") {
        if !v.trim().is_empty() {
            return v.trim_end_matches('/').to_string();
        }
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:3000");
    format!("https://{}", host)
}

fn google_redirect_uri(headers: &HeaderMap) -> String {
    format!("{}/api/auth/google/callback", public_base(headers))
}

fn request_is_https(headers: &HeaderMap) -> bool {
    if let Some(proto) = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or(v).trim().to_ascii_lowercase())
    {
        if proto == "https" {
            return true;
        }
    }
    for name in [header::ORIGIN, header::REFERER] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            if v.starts_with("https://") {
                return true;
            }
            if v.starts_with("http://") {
                return false;
            }
        }
    }
    std::env::var("SIGNAGE_PUBLIC_URL")
        .map(|v| v.trim_start().starts_with("https://"))
        .unwrap_or(false)
}

fn session_cookie_value(headers: &HeaderMap, token: &str, ttl: u64) -> Option<String> {
    if !request_is_https(headers) {
        // On plain HTTP installs the admin UI uses the JSON token stored in
        // localStorage. Do not emit an insecure signage_session cookie: Firefox
        // rejects it when an older Secure cookie with the same name exists,
        // causing the visible "existing secure cookie" login warning.
        return None;
    }
    Some(format!(
        "signage_session={}; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age={}",
        token, ttl
    ))
}

fn clear_session_cookie_value(headers: &HeaderMap) -> Option<&'static str> {
    if request_is_https(headers) {
        Some("signage_session=; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age=0")
    } else {
        None
    }
}

fn state_key() -> Option<Vec<u8>> {
    let key = std::env::var("SIGNAGE_AUTH_SECRET").ok()?.into_bytes();
    (key.len() >= 32).then_some(key)
}
fn oauth_state_store() -> &'static Mutex<HashMap<String, u64>> {
    static STORE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn sign_google_state(mode: &str, username: Option<&str>) -> Option<String> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let issued_at = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let body = serde_json::json!({"mode":mode,"username":username.unwrap_or(""),"nonce":nonce,"iat":issued_at})
        .to_string();
    let body64 = URL_SAFE_NO_PAD.encode(body.as_bytes());
    let mut mac = HmacSha256::new_from_slice(&state_key()?).ok()?;
    mac.update(body64.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let mut store = oauth_state_store().lock().ok()?;
    store.retain(|_, timestamp| issued_at.saturating_sub(*timestamp) <= 900);
    if store.len() >= 4096 {
        store.clear();
    }
    store.insert(nonce, issued_at);
    Some(format!("{}.{}", body64, sig))
}
fn verify_google_state(state: &str) -> Option<serde_json::Value> {
    if state.len() > MAX_SIGNED_TOKEN_BYTES {
        return None;
    }
    let (body64, sig) = state.rsplit_once('.')?;
    if body64.len() > MAX_SIGNED_PAYLOAD_BYTES * 2 || sig.len() > 256 {
        return None;
    }
    let mut mac = HmacSha256::new_from_slice(&state_key()?).ok()?;
    mac.update(body64.as_bytes());
    let expected = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    if expected.as_bytes().ct_eq(sig.as_bytes()).unwrap_u8() != 1 {
        return None;
    }
    let body = URL_SAFE_NO_PAD.decode(body64.as_bytes()).ok()?;
    if body.len() > MAX_SIGNED_PAYLOAD_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let issued_at = value.get("iat")?.as_u64()?;
    let nonce = value.get("nonce")?.as_str()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if now.saturating_sub(issued_at) > 600 {
        return None;
    }
    let mut store = oauth_state_store().lock().ok()?;
    if store.remove(nonce).is_none() {
        return None;
    }
    Some(value)
}

fn google_auth_url(
    headers: &HeaderMap,
    mode: &str,
    username: Option<&str>,
) -> Result<String, (StatusCode, String)> {
    let client_id = google_client_id().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Google OAuth is not configured".to_string(),
    ))?;
    if google_client_secret().is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Google OAuth is not configured".to_string(),
        ));
    }
    let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &google_redirect_uri(headers))
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("prompt", "select_account")
        .append_pair(
            "state",
            &sign_google_state(mode, username).ok_or((
                StatusCode::SERVICE_UNAVAILABLE,
                "OAuth state signing is not configured".to_string(),
            ))?,
        );
    Ok(url.to_string())
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct GoogleCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: u64,
    pub username: String,
    pub role: String,
}

/// CSRF guard for POST /api/auth/login.
///
/// Compares the `Origin` header (preferred) or `Referer` header against the
/// `Host` header. Tolerates absent Origin (curl, server-side scripts) — but
/// rejects mismatched origins, which is what a browser cross-origin POST
/// would send.
///
/// Returns Ok(()) on allow, Err on reject.
pub fn csrf_check_login(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let host = match headers.get(header::HOST) {
        Some(h) => h.to_str().unwrap_or("").to_string(),
        None => return Ok(()), // No Host header is unusual; allow.
    };
    let host_trim = host.split(':').next().unwrap_or("");

    if let Some(o) = headers.get(header::ORIGIN) {
        if let Ok(o) = o.to_str() {
            // Origin format: scheme://host[:port]
            let o = o.trim_end_matches('/');
            let origin_host = o
                .split_once("://")
                .map(|(_, h)| h)
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            if origin_host != host_trim && !origin_host.is_empty() {
                log::warn!("csrf: login attempt from origin={} (host={})", o, host);
                return Err((
                    StatusCode::FORBIDDEN,
                    "Cross-origin login rejected".to_string(),
                ));
            }
        }
    } else if let Some(r) = headers.get(header::REFERER) {
        if let Ok(r) = r.to_str() {
            let r = r.trim_end_matches('/');
            let referer_host = r
                .split_once("://")
                .map(|(_, h)| h)
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            if referer_host != host_trim && !referer_host.is_empty() {
                log::warn!("csrf: login attempt from referer={} (host={})", r, host);
                return Err((
                    StatusCode::FORBIDDEN,
                    "Cross-origin login rejected".to_string(),
                ));
            }
        }
    }
    // No Origin or Referer: will accept (e.g. curl from operator).
    Ok(())
}

/// CSRF guard for state-changing requests authenticated by the session cookie.
/// Bearer-token API clients do not use this path; browsers using the HttpOnly
/// cookie must prove that Origin or Referer belongs to the request Host.
pub fn csrf_check_state_change(headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or((StatusCode::FORBIDDEN, "CSRF origin required".to_string()))?;
    let source = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|value| value.to_str().ok())
        .ok_or((StatusCode::FORBIDDEN, "CSRF origin required".to_string()))?;
    let source_url = Url::parse(source).map_err(|_| {
        (StatusCode::FORBIDDEN, "Invalid CSRF origin".to_string())
    })?;
    let source_host = source_url.host_str().unwrap_or("").to_ascii_lowercase();
    let expected_url = Url::parse(&format!("{}://{}", source_url.scheme(), host)).map_err(|_| {
        (StatusCode::FORBIDDEN, "Invalid request host".to_string())
    })?;
    let expected_host = expected_url.host_str().unwrap_or("").to_ascii_lowercase();
    let ports_match = expected_url.port_or_known_default() == source_url.port_or_known_default();
    if source_host.is_empty() || source_host != expected_host || !ports_match {
        log::warn!("csrf: state-changing request from origin={} (host={})", source, host);
        return Err((
            StatusCode::FORBIDDEN,
            "Cross-origin state change rejected".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod csrf_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    fn map_with(items: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in items {
            m.insert(
                HeaderName::from_bytes(k.as_bytes()).expect("test header name"),
                HeaderValue::from_str(v).expect("test header value"),
            );
        }
        m
    }

    #[test]
    fn state_change_requires_same_origin() {
        let same = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("origin", "http://screentra-cast.home.arpa:3000"),
        ]);
        assert!(csrf_check_state_change(&same).is_ok());
        let cross = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("origin", "http://evil.example"),
        ]);
        assert!(csrf_check_state_change(&cross).is_err());
        assert!(csrf_check_state_change(&map_with(&[("host", "screentra-cast.home.arpa:3000")])).is_err());
    }

    #[test]
    fn csrf_allows_same_origin() {
        let m = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("origin", "http://screentra-cast.home.arpa:3000"),
        ]);
        assert!(csrf_check_login(&m).is_ok());
    }

    #[test]
    fn csrf_rejects_cross_origin() {
        let m = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("origin", "http://evil.example.com"),
        ]);
        assert!(matches!(
            csrf_check_login(&m),
            Err((StatusCode::FORBIDDEN, _))
        ));
    }

    #[test]
    fn csrf_rejects_cross_referer() {
        let m = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("referer", "http://evil.example.com/login"),
        ]);
        assert!(matches!(
            csrf_check_login(&m),
            Err((StatusCode::FORBIDDEN, _))
        ));
    }

    #[test]
    fn csrf_allows_no_origin_or_referer() {
        let m = map_with(&[("host", "screentra-cast.home.arpa:3000")]);
        assert!(csrf_check_login(&m).is_ok());
    }

    #[test]
    fn csrf_allows_https_matching_host() {
        let m = map_with(&[
            ("host", "screentra-cast.home.arpa:3000"),
            ("origin", "https://screentra-cast.home.arpa:3000"),
        ]);
        assert!(csrf_check_login(&m).is_ok());
    }
}

pub async fn login(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Response, (StatusCode, String)> {
    // Rate-limit by client IP. We deliberately use the socket peer identity
    // (X-Forwarded-For honoured only when behind a trusted proxy). 5 attempts
    // per 5 minutes by default prevents brute-force password guessing.
    // CSRF check: reject cross-origin POSTs to /api/auth/login.
    csrf_check_login(&headers)?;

    let ip = client_ip(&headers);
    let auth = &state.auth;
    match auth
        .login_limiter()
        .check(&format!("login:{}", ip), POLICY_AUTH_LOGIN)
    {
        Decision::Allow => {}
        Decision::Deny { retry_after_secs } => {
            log::warn!(
                "rate-limit: login attempt from ip={} blocked; retry_in={}s",
                ip,
                retry_after_secs
            );
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                format!("Too many login attempts; retry in {}s", retry_after_secs),
            ));
        }
    }

    let username_input = body.username.as_deref().unwrap_or("").trim();
    let db_has_active_admin = state
        .repository
        .count_active_admins()
        .await
        .ok()
        .map(|count| count > 0);
    let login_user = if !username_input.is_empty() {
        authenticate_db_user(&state.repository, username_input, &body.password).await
    } else {
        None
    };
    let (username, role) = if let Some(user) = login_user {
        (user.username, user.role)
    } else if db_has_active_admin == Some(false)
        && (username_input.is_empty() || username_input.eq_ignore_ascii_case("admin"))
        && verify_env_admin_password(&body.password)
    {
        ("admin".to_string(), "admin".to_string())
    } else {
        log::warn!(
            "auth: failed login from ip={} user={} (invalid credentials)",
            ip,
            username_input
        );
        return Err((StatusCode::UNAUTHORIZED, "Invalid credentials".to_string()));
    };
    // Evict the successful attempt from the rate limiter so it does not
    // count against the 5-attempt failed-login budget.
    auth.login_limiter().evict(&format!("login:{}", ip));

    log::info!(
        "auth: successful login from ip={} user={} role={}",
        ip,
        username,
        role
    );
    let token = auth.issue_for(&username, &role);
    let ttl = auth.ttl_seconds();
    // Persist session to DB so it survives server restarts
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let expires_at_secs = now + ttl as i64;
    if let Err(e) = state
        .repository
        .create_session(&token, &username, expires_at_secs)
        .await
    {
        log::warn!("auth: failed to persist session for {}: {}", username, e);
    }
    let mut resp = Json(LoginResponse {
        token: token.clone(),
        expires_in: ttl,
        username: username.clone(),
        role,
    })
    .into_response();
    if let Some(cookie) = session_cookie_value(&headers, &token, ttl) {
        if let Ok(val) = HeaderValue::from_str(&cookie) {
            resp.headers_mut().insert(header::SET_COOKIE, val);
        }
    }
    if let Some(scope) = body.step_up_scope.as_deref() {
        if !AuthState::supported_step_up_scope(scope) {
            return Err((StatusCode::BAD_REQUEST, "unsupported step-up scope".to_string()));
        }
        if let Ok(val) = HeaderValue::from_str(&auth.step_up_cookie_value(&username, scope)) {
            resp.headers_mut().append(header::SET_COOKIE, val);
        }
    }
    Ok(resp)
}

async fn record_step_up_audit(
    repository: &AuthRepository,
    principal: &crate::middleware::AuthenticatedPrincipal,
    outcome: &str,
    detail: &str,
) {
    let now = Utc::now().to_rfc3339();
    let _ = repository
        .record_audit_event(
            &principal.username,
            &principal.role,
            "step_up_authentication",
            "step-up",
            outcome,
            detail,
            &now,
        )
        .await;
}

#[derive(Deserialize)]
pub struct StepUpRequest {
    pub password: String,
    pub scope: String,
}

pub async fn step_up(
    State(state): State<AuthAppState>,
    Extension(principal): Extension<crate::middleware::AuthenticatedPrincipal>,
    Json(body): Json<StepUpRequest>,
) -> Result<Response, (StatusCode, String)> {
    if !AuthState::supported_step_up_scope(&body.scope) {
        record_step_up_audit(
            &state.repository,
            &principal,
            "failure",
            "unsupported step-up scope",
        )
        .await;
        return Err((StatusCode::BAD_REQUEST, "unsupported step-up scope".to_string()));
    }
    let limiter_key = format!("step-up:{}:{}", principal.username, body.scope);
    match state
        .auth
        .login_limiter()
        .check(&limiter_key, POLICY_AUTH_LOGIN)
    {
        Decision::Allow => {}
        Decision::Deny { retry_after_secs } => {
            record_step_up_audit(
                &state.repository,
                &principal,
                "failure",
                "step-up rate limit exceeded",
            )
            .await;
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                format!("Too many step-up attempts; retry in {}s", retry_after_secs),
            ));
        }
    }
    let stored_hash = match state
        .repository
        .get_active_password_hash(&principal.username)
        .await
    {
        Ok(hash) => hash,
        Err(_) => {
            record_step_up_audit(
                &state.repository,
                &principal,
                "failure",
                "step-up password lookup failed",
            )
            .await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "step-up unavailable".to_string()));
        }
    };
    let valid = if let Some(hash) = stored_hash.as_deref() {
        verify_password(&body.password, hash)
    } else {
        let legacy_admin = if principal.username.eq_ignore_ascii_case("admin") {
            state
                .repository
                .count_active_admins()
                .await
                .ok()
                .map(|count| count == 0)
                .unwrap_or(false)
        } else {
            false
        };
        legacy_admin && verify_env_admin_password(&body.password)
    };
    if !valid {
        record_step_up_audit(
            &state.repository,
            &principal,
            "failure",
            "step-up password verification failed",
        )
        .await;
        return Err((StatusCode::UNAUTHORIZED, "step-up failed".to_string()));
    }
    state.auth.login_limiter().evict(&limiter_key);
    record_step_up_audit(
        &state.repository,
        &principal,
        "success",
        "step-up authentication granted",
    )
    .await;
    let mut response = Json(serde_json::json!({
        "success": true,
        "scope": body.scope,
        "expires_in": AuthState::step_up_ttl(&body.scope),
    }))
    .into_response();
    let cookie = state
        .auth
        .step_up_cookie_value(&principal.username, &body.scope);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

pub async fn clear_step_up(State(_state): State<AuthAppState>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(AuthState::clear_step_up_cookie_value()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

pub async fn logout(State(state): State<AuthAppState>, headers: HeaderMap) -> Response {
    if let Some(tok) = extract_token(&headers) {
        state.auth.revoke_persistent(&tok).await;
        log::info!("auth: logout (token revoked)");
    } else {
        log::info!("auth: logout (no token)");
    }
    let mut resp = (StatusCode::NO_CONTENT, "").into_response();
    if let Some(cookie) = clear_session_cookie_value(&headers) {
        if let Ok(val) = HeaderValue::from_str(cookie) {
            resp.headers_mut().insert(header::SET_COOKIE, val);
        }
    }
    resp
}

pub async fn status(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let token = extract_token(&headers);
    let payload = match token.as_deref() {
        Some(token) => state.auth.token_payload(token).await,
        None => None,
    };
    let authenticated = payload.is_some();
    Json(serde_json::json!({
        "authenticated": authenticated,
        "ttl_seconds": state.auth.ttl_seconds(),
        "username": payload.as_ref().and_then(|p| p.get("sub")).and_then(|v| v.as_str()).unwrap_or(""),
        "role": payload.as_ref().and_then(|p| p.get("role")).and_then(|v| v.as_str()).unwrap_or(""),
    }))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
    #[serde(default)]
    pub step_up_scope: Option<String>,
}

pub async fn account_status(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = current_username(&state.auth, &headers)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    if username == "admin" {
        return Ok(Json(
            serde_json::json!({"username":"admin","role":"admin","env_admin":true,"google_configured":google_configured(),"google_bound":false,"google_email":null}),
        ));
    }
    let user = state
        .repository
        .get_user_account(&username)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    Ok(Json(serde_json::json!({
        "username": user.0,
        "display_name": user.1,
        "role": user.2,
        "active": user.3,
        "google_email": user.4.clone(),
        "google_bound": user.4.is_some(),
        "env_admin": false,
        "google_configured": google_configured(),
    })))
}

pub async fn change_password(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = current_username(&state.auth, &headers)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    if username == "admin" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Built-in admin password is managed by SIGNAGE_ADMIN_PASSWORD".to_string(),
        ));
    }
    if body.new_password.len() < 8 {
        return Err((
            StatusCode::BAD_REQUEST,
            "New password must be at least 8 characters".to_string(),
        ));
    }
    let stored = state
        .repository
        .get_active_password_hash(&username)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "User not found".to_string()))?;
    if !verify_password(&body.current_password, &stored) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Current password is incorrect".to_string(),
        ));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let hash = hash_password(&body.new_password);
    state
        .repository
        .update_user_password(&username, &hash, &now)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    Ok(Json(serde_json::json!({"ok":true})))
}

pub async fn google_bind_url(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = current_username(&state.auth, &headers)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    if username == "admin" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Create a database admin user before binding Google SSO".to_string(),
        ));
    }
    Ok(Json(
        serde_json::json!({"configured": google_configured(), "url": google_auth_url(&headers, "bind", Some(&username))?}),
    ))
}

pub async fn google_login_url(
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Ok(Json(
        serde_json::json!({"configured": google_configured(), "url": google_auth_url(&headers, "login", None)?}),
    ))
}

pub async fn google_unbind(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let username = current_username(&state.auth, &headers)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    if username == "admin" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Built-in admin has no Google binding".to_string(),
        ));
    }
    state
        .repository
        .clear_google_binding(&username, &chrono::Utc::now().to_rfc3339())
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    Ok(Json(serde_json::json!({"ok":true})))
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
}
#[derive(Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: Option<String>,
}

pub async fn google_callback(
    State(state): State<AuthAppState>,
    headers: HeaderMap,
    Query(q): Query<GoogleCallbackQuery>,
) -> Response {
    let fail = |msg: &str| {
        Redirect::temporary(&format!(
            "/?google_error={}",
            url::form_urlencoded::byte_serialize(msg.as_bytes()).collect::<String>()
        ))
        .into_response()
    };
    if let Some(e) = q.error.as_deref() {
        return fail(e);
    }
    let Some(code) = q.code.as_deref() else {
        return fail("missing_code");
    };
    let Some(st) = q.state.as_deref().and_then(verify_google_state) else {
        return fail("bad_state");
    };
    let mode = st.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if mode == "bind" {
        let expected = st.get("username").and_then(|v| v.as_str()).unwrap_or("");
        if expected.is_empty() || current_username(&state.auth, &headers).await.as_deref() != Some(expected) {
            return fail("bind_session_mismatch");
        }
    }
    let client_id = match google_client_id() {
        Some(v) => v,
        None => return fail("google_not_configured"),
    };
    let client_secret = match google_client_secret() {
        Some(v) => v,
        None => return fail("google_not_configured"),
    };
    let client = reqwest::Client::new();
    let token = match client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code),
            ("client_id", &client_id),
            ("client_secret", &client_secret),
            ("redirect_uri", &google_redirect_uri(&headers)),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
    {
        Ok(r) => match read_google_json::<GoogleTokenResponse>(r).await {
            Ok(t) => t,
            Err(_) => return fail("token_parse_failed"),
        },
        Err(_) => return fail("token_exchange_failed"),
    };
    let info = match client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(r) => match read_google_json::<GoogleUserInfo>(r).await {
            Ok(i) => i,
            Err(_) => return fail("userinfo_parse_failed"),
        },
        Err(_) => return fail("userinfo_failed"),
    };
    if mode == "bind" {
        let username = st.get("username").and_then(|v| v.as_str()).unwrap_or("");
        if username.is_empty() || username == "admin" {
            return fail("bad_bind_user");
        }
        let email = info.email.unwrap_or_default();
        if let Err(e) = state
            .repository
            .bind_google_account(
                username,
                &info.sub,
                &email,
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
        {
            log::warn!("google bind failed: {}", e);
            return fail("bind_failed");
        }
        return Redirect::temporary("/?google_bind=ok").into_response();
    }
    if mode == "login" {
        let (username, role) = match state.repository.find_active_google_user(&info.sub).await {
            Ok(Some(v)) => v,
            Ok(None) | Err(_) => return fail("google_account_not_bound"),
        };
        let token = state.auth.issue_for(&username, &role);
        let ttl = state.auth.ttl_seconds();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;
        if state
            .repository
            .create_session(&token, &username, now + ttl as i64)
            .await
            .is_err()
        {
            return fail("session_persist_failed");
        }
        let mut resp = Redirect::temporary("/?google_login=ok").into_response();
        if let Some(cookie) = session_cookie_value(&headers, &token, ttl) {
            if let Ok(val) = HeaderValue::from_str(&cookie) {
                resp.headers_mut().insert(header::SET_COOKIE, val);
            }
        }
        return resp;
    }
    fail("bad_mode")
}

pub fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                return Some(rest.trim().to_string());
            }
        }
    }
    if let Some(cookie) = headers.get(header::COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                let part = part.trim();
                if let Some(rest) = part.strip_prefix("signage_session=") {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

pub fn extract_step_up_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("x-screentra-step-up") {
        if let Ok(token) = value.to_str() {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }
    headers.get(header::COOKIE).and_then(|cookie| {
        cookie.to_str().ok()?.split(';').find_map(|part| {
            part.trim()
                .strip_prefix("screentra_cast_step_up=")
                .map(str::to_string)
        })
    })
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
pub struct AuthUser;

/// Resolve the client IP from request headers. Behavior depends on the
/// `SIGNAGE_TRUSTED_PROXY` env var:
///
/// - `SIGNAGE_TRUSTED_PROXY=1` (default off): honors `X-Real-IP` /
///   `X-Forwarded-For`. Operators MUST set this when running behind a
///   reverse proxy that strips/forwards these headers; otherwise rate-limit
///   can be bypassed by setting them in a direct request.
/// - Otherwise (default): ignores both headers and falls through to
///   "unknown". Rate-limit key collisions on "unknown" are acceptable on a
///   LAN-only deployment; if you expose the server to the internet, set
///   `SIGNAGE_TRUSTED_PROXY=1` AND terminate TLS at a reverse proxy that
///   drops client-set `X-Real-IP` / `X-Forwarded-For` headers.
pub fn client_ip(headers: &HeaderMap) -> String {
    let trust_proxy = matches!(
        std::env::var("SIGNAGE_TRUSTED_PROXY").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    );
    if !trust_proxy {
        return "unknown".to_string();
    }
    if let Some(v) = headers.get("x-real-ip") {
        if let Ok(s) = v.to_str() {
            return s.to_string();
        }
    }
    if let Some(v) = headers.get("x-forwarded-for") {
        if let Ok(s) = v.to_str() {
            if let Some(first) = s.split(',').next() {
                return first.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::http::HeaderValue;

    fn test_state() -> AuthState {
        AuthState {
            key: vec![1; 32],
            ttl: Duration::from_secs(60),
            revoked: Arc::new(RwLock::new(HashSet::new())),
            login_limiter: Arc::new(RateLimiter::new()),
            repository: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn issue_and_verify_round_trip() {
        let s = test_state();
        let tok = s.issue_for("admin", "admin");
        assert!(s.verify(&tok), "freshly-issued token should verify");
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let s = test_state();
        let tok = s.issue_for("admin", "admin");
        // Flip the last char of the signature.
        let mut tampered: String = tok.clone();
        let last = tampered.pop().unwrap();
        let swapped = if last == 'A' { 'B' } else { 'A' };
        tampered.push(swapped);
        assert!(!s.verify(&tampered));
    }

    #[test]
    fn tampered_body_fails_verify() {
        let s = test_state();
        let tok = s.issue_for("admin", "admin");
        // Flip a character in the body portion.
        let parts: Vec<&str> = tok.split('.').collect();
        assert_eq!(parts.len(), 2);
        let body: String = parts[0].to_string();
        let mut chars: Vec<char> = body.chars().collect();
        let last = chars.pop().unwrap();
        chars.push(if last == 'A' { 'B' } else { 'A' });
        let new_body: String = chars.into_iter().collect();
        let tampered = format!("{}.{}", new_body, parts[1]);
        assert!(!s.verify(&tampered));
    }

    #[test]
    fn oversized_signed_tokens_are_rejected_before_decode() {
        let state = test_state();
        assert!(state
            .decode_signed_payload(&"x".repeat(MAX_SIGNED_TOKEN_BYTES + 1))
            .is_none());
    }

    #[test]
    fn oversized_revocations_are_not_retained() {
        let state = test_state();
        state.revoke(&"x".repeat(MAX_SIGNED_TOKEN_BYTES + 1));
        assert!(state.revoked.read().unwrap().is_empty());
    }

    #[test]
    fn malformed_token_fails_verify() {
        let s = test_state();
        for bad in ["", "no-dot", ".", "a.", ".b"] {
            assert!(!s.verify(bad), "should reject {:?}", bad);
        }
    }

    #[test]
    fn revoked_token_fails_verify() {
        let s = test_state();
        let tok = s.issue_for("admin", "admin");
        assert!(s.verify(&tok));
        s.revoke(&tok);
        assert!(!s.verify(&tok));
    }

    #[test]
    fn ttl_seconds_reflects_constructor() {
        let s = AuthState {
            key: vec![2; 32],
            ttl: Duration::from_secs(123),
            revoked: Arc::new(RwLock::new(HashSet::new())),
            login_limiter: Arc::new(RateLimiter::new()),
            repository: Arc::new(Mutex::new(None)),
        };
        assert_eq!(s.ttl_seconds(), 123);
    }

    #[test]
    fn extract_token_prefers_bearer_over_cookie() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer bearer-tok"),
        );
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("signage_session=cookie-tok"),
        );
        assert_eq!(extract_token(&h).as_deref(), Some("bearer-tok"));
    }

    #[test]
    fn extract_token_falls_back_to_cookie() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("signage_session=cookie-tok"),
        );
        assert_eq!(extract_token(&h).as_deref(), Some("cookie-tok"));
    }

    #[test]
    fn client_ip_ignores_headers_when_trusted_proxy_disabled() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        // Make sure the gate is closed by default in tests
        std::env::remove_var("SIGNAGE_TRUSTED_PROXY");
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("1.2.3.4"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("5.6.7.8"));
        let ip = client_ip(&headers);
        assert_eq!(
            ip, "unknown",
            "headers should be ignored when SIGNAGE_TRUSTED_PROXY is unset"
        );
    }

    #[test]
    fn client_ip_honors_headers_when_trusted_proxy_enabled() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        std::env::set_var("SIGNAGE_TRUSTED_PROXY", "1");
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("1.2.3.4"));
        assert_eq!(client_ip(&headers), "1.2.3.4");
        std::env::remove_var("SIGNAGE_TRUSTED_PROXY");
    }

    #[test]
    fn client_ip_falls_back_to_unknown_when_proxy_enabled_but_no_headers() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        std::env::set_var("SIGNAGE_TRUSTED_PROXY", "1");
        let headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), "unknown");
        std::env::remove_var("SIGNAGE_TRUSTED_PROXY");
    }

    #[test]
    fn addon_execution_approval_is_a_supported_step_up_scope() {
        assert!(AuthState::supported_step_up_scope("addon-execution-approval"));
    }

    #[test]
    fn password_hashes_use_argon2id_and_verify() {
        let hash = hash_password("correct horse battery staple");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong password", &hash));
        assert!(!password_hash_needs_upgrade(&hash));
    }

    #[test]
    fn legacy_sha256_password_hashes_still_verify_and_need_upgrade() {
        let salt = "legacy-test-salt";
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(b":");
        hasher.update(b"legacy-password");
        let legacy = format!("sha256:{salt}:{:x}", hasher.finalize());
        assert!(verify_password("legacy-password", &legacy));
        assert!(!verify_password("wrong password", &legacy));
        assert!(password_hash_needs_upgrade(&legacy));
    }

    #[test]
    fn malformed_password_hashes_fail_closed() {
        assert!(!verify_password("password", "sha256:not-a-valid-record"));
        assert!(!verify_password("password", "not-a-password-hash"));
    }

    #[test]
    fn extract_token_returns_none_when_missing() {
        let h = HeaderMap::new();
        assert_eq!(extract_token(&h), None);
    }
}
