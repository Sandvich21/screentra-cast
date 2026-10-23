//! GitHub Releases updater for the server application.
//!
//! Releases are expected to contain a prebuilt, signed update bundle. This
//! module never builds source code on the target server and delegates bundle
//! verification/application to `version::import_update_bundle`.

use crate::api::ClientRepository;
use crate::version::{self, UpdateResult};
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const DEFAULT_REPOSITORY: &str = "Sandvich21/screentra-cast";
const DEFAULT_ASSET: &str = "screentra-cast-server-update.tar.gz";
const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PERSISTED_JSON_BYTES: usize = 64 * 1024;
const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

static UPDATE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateSettings {
    /// When enabled, the background loop checks and installs stable releases.
    pub auto_update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    pub auto_update: bool,
    pub state: String,
    pub current_version: String,
    pub current_commit: String,
    pub checked_at: Option<String>,
    pub latest_tag: Option<String>,
    pub latest_version: Option<String>,
    pub latest_name: Option<String>,
    pub latest_url: Option<String>,
    pub latest_notes: Option<String>,
    pub latest_published_at: Option<String>,
    pub asset_name: Option<String>,
    pub asset_size_bytes: Option<u64>,
    pub asset_digest: Option<String>,
    pub blocked_release_tag: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    body: Option<String>,
    published_at: Option<String>,
    prerelease: bool,
    draft: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize, Clone)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn state_path() -> PathBuf {
    PathBuf::from(version::install_dir_for_static()).join("server-update-status.json")
}

fn settings_path() -> PathBuf {
    PathBuf::from(version::install_dir_for_static()).join("server-update-settings.json")
}

fn repository() -> Result<String, String> {
    let value = std::env::var("SCREENTRA_CAST_GITHUB_REPO")
        .unwrap_or_else(|_| DEFAULT_REPOSITORY.to_string());
    let valid = value.split('/').count() == 2
        && value.split('/').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if valid {
        Ok(value)
    } else {
        Err("SCREENTRA_CAST_GITHUB_REPO must be in owner/repository form".to_string())
    }
}

fn asset_name() -> String {
    std::env::var("SCREENTRA_CAST_UPDATE_ASSET").unwrap_or_else(|_| DEFAULT_ASSET.to_string())
}

fn atomic_write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|e| format!("serialize update state: {e}"))?;
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("replace {}: {e}", path.display())
    })
}

fn read_bounded_json(path: PathBuf) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PERSISTED_JSON_BYTES as u64 {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take((MAX_PERSISTED_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_PERSISTED_JSON_BYTES).then_some(bytes)
}

pub fn read_settings() -> UpdateSettings {
    read_bounded_json(settings_path())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &UpdateSettings) -> Result<UpdateSettings, String> {
    let path = settings_path();
    atomic_write_json(&path, settings)?;
    Ok(settings.clone())
}

pub fn read_status() -> UpdateStatus {
    let settings = read_settings();
    let mut status = read_bounded_json(state_path())
        .and_then(|bytes| serde_json::from_slice::<UpdateStatus>(&bytes).ok())
        .unwrap_or_default();
    status.auto_update = settings.auto_update;
    if status.current_version.is_empty() {
        status.current_version = version::VERSION.to_string();
    }
    if status.current_commit.is_empty() {
        status.current_commit = version::GIT_COMMIT.to_string();
    }
    if status.state.is_empty() {
        status.state = "unknown".to_string();
    }
    status
}

fn write_status(status: &UpdateStatus) -> Result<(), String> {
    atomic_write_json(&state_path(), status)
}

fn error_status(message: impl Into<String>) -> UpdateStatus {
    let mut status = read_status();
    status.current_version = version::VERSION.to_string();
    status.current_commit = version::GIT_COMMIT.to_string();
    status.checked_at = Some(Utc::now().to_rfc3339());
    status.state = "error".to_string();
    status.error = Some(message.into());
    status
}

fn normalized_version(value: &str) -> &str {
    value.trim().trim_start_matches(['v', 'V'])
}

fn semver_tuple(value: &str) -> Option<(u64, u64, u64)> {
    let value = normalized_version(value).split(['-', '+']).next()?;
    let mut parts = value.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn is_newer_release(tag: &str) -> bool {
    let current = version::VERSION;
    match (semver_tuple(tag), semver_tuple(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => normalized_version(tag) != normalized_version(current),
    }
}

fn truncate_notes(body: Option<String>) -> Option<String> {
    body.map(|mut text| {
        if text.len() > 6000 {
            text.truncate(6000);
            text.push_str("\n…");
        }
        text
    })
}

fn github_client() -> Result<Client, String> {
    Client::builder()
        .user_agent("Screentra-Cast-Server-Updater")
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("create GitHub client: {e}"))
}

async fn fetch_latest_release() -> Result<(GithubRelease, GithubAsset), String> {
    let repo = repository()?;
    let configured_asset = asset_name();
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let response = github_client()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GitHub release request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub release request returned HTTP {status}"));
    }
    let release: GithubRelease = response
        .json()
        .await
        .map_err(|e| format!("parse GitHub release response: {e}"))?;
    if release.draft || release.prerelease {
        return Err("GitHub latest release is not a stable published release".to_string());
    }
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == configured_asset)
        .cloned()
        .ok_or_else(|| {
            format!(
                "GitHub release {} does not contain required asset {}",
                release.tag_name, configured_asset
            )
        })?;
    Ok((release, asset))
}

fn release_status(release: &GithubRelease, asset: &GithubAsset) -> UpdateStatus {
    let mut status = read_status();
    status.auto_update = read_settings().auto_update;
    status.current_version = version::VERSION.to_string();
    status.current_commit = version::GIT_COMMIT.to_string();
    status.checked_at = Some(Utc::now().to_rfc3339());
    status.latest_tag = Some(release.tag_name.clone());
    status.latest_version = Some(normalized_version(&release.tag_name).to_string());
    status.latest_name = release.name.clone();
    status.latest_url = Some(release.html_url.clone());
    status.latest_notes = truncate_notes(release.body.clone());
    status.latest_published_at = release.published_at.clone();
    status.asset_name = Some(asset.name.clone());
    status.asset_size_bytes = Some(asset.size);
    status.asset_digest = asset.digest.clone();
    status.error = None;
    status.state = if is_newer_release(&release.tag_name) {
        "available".to_string()
    } else {
        "current".to_string()
    };
    status
}

pub async fn check_latest() -> Result<UpdateStatus, String> {
    match fetch_latest_release().await {
        Ok((release, asset)) => {
            let status = release_status(&release, &asset);
            write_status(&status)?;
            Ok(status)
        }
        Err(error) => {
            let status = error_status(error.clone());
            let _ = write_status(&status);
            Err(error)
        }
    }
}

async fn download_asset(asset: &GithubAsset) -> Result<PathBuf, String> {
    if asset.size > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "GitHub update asset is too large: {} bytes",
            asset.size
        ));
    }
    let path = PathBuf::from(version::install_dir_for_static()).join(format!(
        ".screentra-cast-github-update-{}.tar.gz",
        Uuid::new_v4()
    ));
    let response = github_client()?
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|e| format!("GitHub update download failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub update download returned HTTP {}",
            response.status()
        ));
    }
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| format!("create update staging file: {e}"))?;
    let mut received = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read GitHub update asset: {e}"))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > MAX_DOWNLOAD_BYTES {
            let _ = tokio::fs::remove_file(&path).await;
            return Err("GitHub update asset exceeded the 1 GiB safety limit".to_string());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("stage GitHub update asset: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("flush GitHub update asset: {e}"))?;
    drop(file);

    if let Some(digest) = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
    {
        let verify_path = path.clone();
        let actual = tokio::task::spawn_blocking(move || version::sha256_of_file(&verify_path))
            .await
            .map_err(|e| format!("join GitHub asset hash task: {e}"))??;
        if actual != digest {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(
                "GitHub update asset digest did not match the release metadata".to_string(),
            );
        }
    }
    Ok(path)
}

/// Check the latest stable release and, when newer, download and apply its
/// signed bundle. The caller owns the service restart step.
pub async fn apply_latest() -> Result<Option<UpdateResult>, String> {
    let lock = UPDATE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;
    let (release, asset) = fetch_latest_release().await?;
    let status = release_status(&release, &asset);
    write_status(&status)?;
    if status.state != "available" {
        return Ok(None);
    }

    let path = match download_asset(&asset).await {
        Ok(path) => path,
        Err(error) => {
            let mut failed = status;
            failed.state = "error".to_string();
            failed.blocked_release_tag = failed.latest_tag.clone();
            failed.error = Some(error.clone());
            let _ = write_status(&failed);
            return Err(error);
        }
    };
    let import_path = path.clone();
    let result =
        tokio::task::spawn_blocking(move || version::import_update_bundle(&import_path, false))
            .await
            .map_err(|e| format!("join update import: {e}"))?;
    let _ = tokio::fs::remove_file(&path).await;
    match result {
        Ok(result) => {
            mark_applying(&result)?;
            Ok(Some(result))
        }
        Err(error) => {
            let mut failed = status;
            failed.state = "error".to_string();
            failed.blocked_release_tag = failed.latest_tag.clone();
            failed.error = Some(error.clone());
            let _ = write_status(&failed);
            Err(error)
        }
    }
}

pub fn mark_applying(result: &UpdateResult) -> Result<(), String> {
    let mut status = read_status();
    status.state = "applying".to_string();
    status.error = None;
    status.latest_tag = Some(format!("v{}", result.manifest.version));
    status.latest_version = Some(result.manifest.version.clone());
    status.blocked_release_tag = None;
    write_status(&status)
}

pub fn mark_error(error: impl Into<String>) -> Result<(), String> {
    let mut status = error_status(error);
    status.auto_update = read_settings().auto_update;
    write_status(&status)
}

/// Background opt-in updater. It waits after startup, then checks at a modest
/// cadence so enabling the checkbox cannot cause an immediate restart loop.
pub async fn run_auto_update_loop(repository: ClientRepository) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        if read_settings().auto_update {
            let check = check_latest().await;
            let blocked = matches!(
                &check,
                Ok(status)
                    if status.state == "available"
                        && status.blocked_release_tag.is_some()
                        && status.blocked_release_tag.as_deref() == status.latest_tag.as_deref()
            );
            if blocked {
                log::warn!("Automatic GitHub update is paused after a failed release; retry it manually from System");
            } else {
                match apply_latest().await {
                    Ok(Some(result)) => {
                        let _ = repository
                            .record_audit_event(
                                Some("system"),
                                Some("system"),
                                "server_update_import",
                                Some("github-latest"),
                                "success",
                                Some("signed GitHub release applied; restarting service"),
                                &Utc::now().to_rfc3339(),
                            )
                            .await;
                        match tokio::task::spawn_blocking(version::restart_configured_service).await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                let _ = mark_error(format!(
                                    "update applied but restart failed: {error}"
                                ));
                            }
                            Err(error) => {
                                let _ = mark_error(format!(
                                    "update applied but restart task failed: {error}"
                                ));
                            }
                        }
                        log::info!(
                            "Automatic GitHub update applied: {}",
                            result.manifest.version
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = repository
                            .record_audit_event(
                                Some("system"),
                                Some("system"),
                                "server_update_import",
                                Some("github-latest"),
                                "failure",
                                Some("automatic GitHub update rejected or failed"),
                                &Utc::now().to_rfc3339(),
                            )
                            .await;
                        log::warn!("Automatic GitHub update failed: {error}");
                    }
                }
            }
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer_release, normalized_version, read_bounded_json, MAX_PERSISTED_JSON_BYTES};
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn release_version_comparison_handles_v_prefix() {
        assert_eq!(normalized_version("v1.2.3"), "1.2.3");
        assert!(is_newer_release("999.0.0"));
        assert!(!is_newer_release("0.1.0"));
    }

    #[test]
    fn persisted_json_reader_rejects_oversized_files() {
        let path = std::env::temp_dir().join(format!("screentra-update-json-{}", Uuid::new_v4()));
        fs::write(&path, vec![b'x'; MAX_PERSISTED_JSON_BYTES + 1]).unwrap();
        assert!(read_bounded_json(path.clone()).is_none());
        let _ = fs::remove_file(path);
    }
}
