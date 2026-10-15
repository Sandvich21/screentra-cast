use crate::db::Database;
use crate::models::Backup;
use crate::safe_archive;
use crate::version::configured_service;
use axum::{
    extract::{FromRef, Multipart, Path, Query, State},
    http::{Response, StatusCode},
    response::Json as AxumJson,
};
use chrono::Utc;
use std::fs;
use std::path::Path as StdPath;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

const MAX_BACKUP_UPLOAD_BYTES: u64 = 1 << 30;

#[derive(Clone)]
pub enum BackupMetadataRepository {
    Sqlite(std::sync::Arc<Database>),
    Postgres(std::sync::Arc<crate::postgres::PostgresStore>),
}

pub type AppState = BackupMetadataRepository;

impl FromRef<crate::api::AppState> for BackupMetadataRepository {
    fn from_ref(state: &crate::api::AppState) -> Self {
        Self::Sqlite(std::sync::Arc::clone(state))
    }
}

impl BackupMetadataRepository {
    async fn list_backups(&self) -> Result<Vec<Backup>, String> {
        match self {
            Self::Sqlite(db) => db.list_backups(),
            Self::Postgres(store) => store.list_backups().await,
        }
    }

    async fn insert_backup(&self, backup: &Backup) -> Result<(), String> {
        match self {
            Self::Sqlite(db) => db.insert_backup(backup),
            Self::Postgres(store) => store.insert_backup(backup).await,
        }
    }

    async fn get_backup_file_path(&self, id: &str) -> Result<String, String> {
        match self {
            Self::Sqlite(db) => db.get_backup_file_path(id),
            Self::Postgres(store) => store.get_backup_file_path(id).await,
        }
    }

    async fn delete_backup(&self, id: &str) -> Result<u64, String> {
        match self {
            Self::Sqlite(db) => db.delete_backup(id),
            Self::Postgres(store) => store.delete_backup(id).await,
        }
    }
}

pub async fn list_backups(
    State(db): State<AppState>,
) -> Result<AxumJson<Vec<Backup>>, (StatusCode, String)> {
    db.list_backups().await
        .map(AxumJson)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))
}

pub async fn create_backup(
    State(db): State<AppState>,
) -> Result<AxumJson<Backup>, (StatusCode, String)> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let safe_ts = now.replace(':', "-");
    let name = format!("Backup {}", &now[..16]);

    let data_dir = signage_data_dir();
    let app_root =
        std::env::current_dir().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    let backup_dir = format!("{}/backups", data_dir);
    fs::create_dir_all(&backup_dir)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    let filename = format!("ripley-signage-{}-{}.tar.gz", safe_ts, id);
    let full_path = format!("{}/{}", backup_dir, filename);

    // Archive the installed app plus the runtime data directory. We cd to the
    // PARENT of each root and pass `./<basename>` so the basename appears in
    // archive paths. This lets us use `--exclude` and `--transform` reliably.
    let install_basename = StdPath::new(&app_root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "install".to_string());
    let install_parent = StdPath::new(&app_root)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let data_basename = StdPath::new(&data_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "data".to_string());
    let data_parent = StdPath::new(&data_dir)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Exclude pre-transform paths: `./<install_basename>/target` etc.
    // tar's --exclude matches before --transform runs.
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(&full_path)
        .arg(format!("--exclude=./{}/target", install_basename))
        .arg(format!("--exclude=./{}/.git", install_basename))
        .arg(format!("--exclude=./{}/server-run.log", install_basename))
        .arg(format!("--exclude=./{}/server-run.pid", install_basename))
        .arg(format!("--exclude=./{}/backups", install_basename))
        // screentra-cast-signage-ca.crt inside the install dir is a published
        // copy of the local CA; only the protected certs copy is authoritative.
        .arg(format!(
            "--exclude=./{}/screentra-cast-signage-ca.crt",
            install_basename
        ))
        // Exclude old backups under data/ so each backup doesn't pack the
        // previous tarballs (~750 MB each, would explode size).
        .arg(format!("--exclude=./{}/backups", data_basename))
        // Transform: `./<install_basename>` -> `./app`, `./<data_basename>` -> `./data`.
        // Without the parent-cd trick above, the basenames don't appear in
        // archive paths and the transforms silently no-op.
        .arg("--transform")
        .arg(format!("s|./{}/|./app/|", install_basename))
        .arg("--transform")
        .arg(format!("s|./{}/|./data/|", data_basename))
        .arg("-C")
        .arg(&install_parent)
        .arg(format!("./{}", install_basename))
        .arg("-C")
        .arg(&data_parent)
        .arg(format!("./{}", data_basename));

    let output = cmd
        .output()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Failed to create tarball{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr)
                }
            ),
        ));
    }

    let metadata =
        fs::metadata(&full_path).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    let size = metadata.len();

    let backup = Backup {
        id: id.clone(),
        name: name.clone(),
        created_at: now.clone(),
        file_path: full_path.clone(),
        size_bytes: size,
    };

    db.insert_backup(&backup)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    Ok(AxumJson(backup))
}

fn signage_data_dir() -> String {
    if let Ok(dir) = std::env::var("SIGNAGE_DATA_DIR") {
        return dir;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/ripley".to_string());
    format!("{}/.local/share/signage", home)
}

#[derive(serde::Serialize)]
struct RestoreDryRunReport {
    install_dir: String,
    data_dir: String,
    service_name: String,
    service_kind: &'static str,
    backup_sha256: String,
    tarball_top_level_entries: Vec<String>,
    contains_core_database: bool,
    contains_trusted_service_staging: bool,
    would_extract_to_install: bool,
    would_extract_to_data: bool,
    would_restart_service: bool,
    warnings: Vec<String>,
}

#[derive(serde::Serialize)]
struct RestoreResult {
    status: &'static str,
    install_dir: String,
    data_dir: String,
    service_name: String,
    service_kind: &'static str,
    sha256_pre: String,
    sha256_post: String,
    duration_ms: u64,
    pre_restore_dir: Option<String>,
    restored_at: String,
}

fn tar_top_level_entries(tarball: &str) -> Result<Vec<String>, String> {
    safe_archive::inspect_tar_gz(StdPath::new(tarball))
}

async fn sha256_file(path: &str) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .await
        .map_err(|e| format!("sha256sum: {}", e))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let s = String::from_utf8_lossy(&output.stdout);
    Ok(s.split_whitespace().next().unwrap_or("").to_string())
}

async fn systemctl_action(action: &str, service: &str, kind: &str) -> Result<(), String> {
    let mut cmd = if kind == "user" {
        let mut c = Command::new("systemctl");
        c.arg("--user");
        c.arg(action);
        c.arg(service);
        c
    } else {
        let mut c = Command::new("sudo");
        c.arg("-n"); // non-interactive — fail loud if password is needed
        c.arg("systemctl");
        c.arg(action);
        c.arg(service);
        c
    };
    cmd.stdin(std::process::Stdio::null());
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to spawn systemctl: {}", e))?;
    if !output.status.success() {
        eprintln!("systemctl action failed: action={action} service={service}");
        return Err("service action failed".to_string());
    }
    Ok(())
}

pub async fn restore_backup(
    State(db): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::http::Response<axum::body::Body>, (StatusCode, String)> {
    use axum::body::Body;
    use axum::http::{header, StatusCode as SC};

    let backup_file_path = db
        .get_backup_file_path(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    if !StdPath::new(&backup_file_path).exists() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("backup file not found: {}", backup_file_path),
        ));
    }

    // Detect service + dirs (same helpers create_backup uses).
    let install_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/opt/digital-signage".to_string());
    let data_dir = signage_data_dir();
    let (service_name, service_kind) = configured_service();

    let top = tar_top_level_entries(&backup_file_path)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("inspect tarball: {}", e),
            )
        })?;
    let sha = sha256_file(&backup_file_path)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    let contains_core_database = safe_archive::archive_contains_path(
        StdPath::new(&backup_file_path),
        StdPath::new("data/signage.db"),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("inspect tarball: {}", e)))?;
    let contains_trusted_service_staging = safe_archive::archive_contains_path_prefix(
        StdPath::new(&backup_file_path),
        StdPath::new("data/trusted-services"),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("inspect tarball: {}", e)))?;

    let mut warnings: Vec<String> = Vec::new();
    let would_app = top.iter().any(|e| e == "app");
    let would_data = top.iter().any(|e| e == "data");
    if !would_app {
        warnings
            .push("tarball has no `app/` top-level — install dir will not be restored".to_string());
    }
    if !would_data {
        warnings
            .push("tarball has no `data/` top-level — data dir will not be restored".to_string());
    }
    if !contains_core_database {
        warnings.push(
            "tarball has no `data/signage.db` — addon storage cannot be restored".to_string(),
        );
    }
    if !contains_trusted_service_staging {
        warnings.push(
            "tarball has no `data/trusted-services/` — staged trusted-service metadata will not be restored".to_string(),
        );
    }

    let report = RestoreDryRunReport {
        install_dir: install_dir.clone(),
        data_dir: data_dir.clone(),
        service_name: service_name.clone(),
        service_kind,
        backup_sha256: sha.clone(),
        tarball_top_level_entries: top,
        contains_core_database,
        contains_trusted_service_staging,
        would_extract_to_install: would_app,
        would_extract_to_data: would_data,
        would_restart_service: true,
        warnings: warnings.clone(),
    };

    // Dry-run gate. Without `?confirm=true` OR with `?dry_run=true`,
    // return the report (200) without touching disk.
    let want_dry_run = params.get("dry_run").map(|v| v == "true").unwrap_or(false);
    let confirmed = params.get("confirm").map(|v| v == "true").unwrap_or(false);
    if !confirmed || want_dry_run {
        let body = serde_json::json!({
            "status": "dry_run",
            "report": report,
        });
        let resp = Response::builder()
            .status(SC::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
        return Ok(resp);
    }

    // === actual restore ===
    let started = std::time::Instant::now();
    let restored_at = Utc::now().to_rfc3339();

    // Stage the tarball OUTSIDE the install/data dirs so that renaming
    // install_dir -> pre_restore_app doesn't leave us with a dangling
    // staging_root path. /var/tmp survives reboots; on dev boxes without
    // /var/tmp we fall back to $TMPDIR or `/tmp`.
    let ts = chrono::Utc::now().timestamp_millis();
    let staging_parent = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            if StdPath::new("/var/tmp").exists() {
                Some("/var/tmp".to_string())
            } else {
                Some("/tmp".to_string())
            }
        })
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "Failed to determine staging parent".to_string()))?;
    let staging_root = format!(
        "{}/ripley-restore-{}-{}",
        staging_parent,
        std::process::id(),
        ts
    );
    if let Err(e) = fs::create_dir_all(&staging_root) {
        log::error!("create backup restore staging failed: {e}");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ));
    }

    let extract_result = safe_archive::extract_tar_gz(
        StdPath::new(&backup_file_path),
        StdPath::new(&staging_root),
        1,
    );
    if let Err(error) = extract_result {
        log::warn!("backup restore archive rejected: {error}");
        let _ = fs::remove_dir_all(&staging_root);
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid backup archive".to_string(),
        ));
    }

    // Move existing install/data aside so the swap is atomic.
    let pre_restore_app = format!("{}.pre-restore-{}", install_dir, ts);
    let pre_restore_data = format!("{}.pre-restore-{}", data_dir, ts);
    if let Err(e) = fs::rename(&install_dir, &pre_restore_app) {
        log::error!("backup restore install rename failed: {e}");
        let _ = fs::remove_dir_all(&staging_root);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ));
    }
    if let Err(e) = fs::rename(&data_dir, &pre_restore_data) {
        log::error!("backup restore data rename failed: {e}");
        // Roll back the install rename.
        let _ = fs::rename(&pre_restore_app, &install_dir);
        let _ = fs::remove_dir_all(&staging_root);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ));
    }

    if let Err(e) = fs::rename(format!("{}/app", staging_root), &install_dir) {
        log::error!("backup restore app placement failed: {e}");
        // Rollback both renames + remove staging.
        let _ = fs::rename(&pre_restore_app, &install_dir);
        let _ = fs::rename(&pre_restore_data, &data_dir);
        let _ = fs::remove_dir_all(&staging_root);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ));
    }
    if let Err(e) = fs::rename(format!("{}/data", staging_root), &data_dir) {
        log::error!("backup restore data placement failed: {e}");
        // Rollback app placement, install rename, data rename.
        let _ = fs::remove_dir_all(&install_dir);
        let _ = fs::rename(&pre_restore_app, &install_dir);
        let _ = fs::rename(&pre_restore_data, &data_dir);
        let _ = fs::remove_dir_all(&staging_root);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        ));
    }

    // Staging dir should now be empty (we moved app/ and data/ out).
    let _ = fs::remove_dir_all(&staging_root);

    // Restart the service. Use --user or sudo -n; never hang on a TTY.
    // This is best-effort: a running binary under the dev box doesn't have
    // a systemd service, so systemctl will fail. We log the warning and
    // continue — the data on disk is already swapped.
    if let Err(e) = systemctl_action("restart", &service_name, service_kind).await {
        warnings.push(format!("service restart failed: {}", e));
    }

    // SHA256 of the new live signage.db (best-effort).
    let post_sha = sha256_file(&format!("{}/signage.db", data_dir))
        .await
        .unwrap_or_default();

    let result = RestoreResult {
        status: "restored",
        install_dir,
        data_dir,
        service_name,
        service_kind,
        sha256_pre: sha,
        sha256_post: post_sha,
        duration_ms: started.elapsed().as_millis() as u64,
        pre_restore_dir: Some(pre_restore_app),
        restored_at,
    };

    let body = serde_json::json!({
        "status": "restored",
        "result": result,
        "warnings": warnings,
    });
    let resp = Response::builder()
        .status(SC::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    Ok(resp)
}

pub async fn delete_backup(
    State(db): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, (StatusCode, String)> {
    let file_path = db
        .get_backup_file_path(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    // Delete the file if it exists (ignore error if already gone - idempotent delete)
    let _ = fs::remove_file(&file_path);

    // Always delete the database record
    db.delete_backup(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn upload_backup(
    State(db): State<AppState>,
    mut multipart: Multipart,
) -> Result<axum::http::StatusCode, (StatusCode, String)> {
    let data_dir = signage_data_dir();
    let backup_dir = format!("{}/backups", data_dir);
    fs::create_dir_all(&backup_dir)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    if let Ok(Some(field)) = multipart.next_field().await {
        let filename = field.file_name().unwrap_or("uploaded.tar.gz").to_string();
        // Strip any path separators from the filename.
        let safe_name = filename
            .rsplit('/')
            .next()
            .unwrap_or(&filename)
            .rsplit('\\')
            .next()
            .unwrap_or(&filename)
            .to_string();
        if safe_name.is_empty() || safe_name.contains("..") {
            return Err((StatusCode::BAD_REQUEST, "Invalid backup filename".to_string()));
        }
        let full_path = format!("{}/{}", backup_dir, safe_name);

        // Stream to a private temporary file rather than buffering an entire
        // multipart backup in memory. The limit is enforced while reading.
        let probe_path = format!("{}/.upload-{}", backup_dir, Uuid::new_v4());
        let mut probe_file = tokio::fs::File::create(&probe_path)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
        let mut size = 0_u64;
        let mut field = field;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid multipart upload".to_string()))?
        {
            size = size.saturating_add(chunk.len() as u64);
            if size > MAX_BACKUP_UPLOAD_BYTES {
                drop(probe_file);
                let _ = tokio::fs::remove_file(&probe_path).await;
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "Backup upload is too large".to_string()));
            }
            probe_file
                .write_all(&chunk)
                .await
                .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
        }
        probe_file
            .flush()
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
        drop(probe_file);

        // Verify it parses as a tarball with `app/` and/or `data/` at top
        // before saving. Reject anything else.
        let top = match tar_top_level_entries(&probe_path) {
            Ok(t) => t,
            Err(e) => {
                let _ = fs::remove_file(&probe_path);
                eprintln!("uploaded backup is not a valid tarball: {e}");
                return Err((StatusCode::BAD_REQUEST, "Uploaded file is not a valid backup".to_string()));
            }
        };
        if !top.iter().any(|e| e == "app") && !top.iter().any(|e| e == "data") {
            let _ = fs::remove_file(&probe_path);
            return Err((
                StatusCode::BAD_REQUEST,
                "Uploaded backup has no app or data payload".to_string(),
            ));
        }

        fs::rename(&probe_path, &full_path)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

        // Register in DB so the operator can pick this backup for restore
        // via PUT /api/backups/{id}?confirm=true. NO auto-restore.
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let size = size as i64;
        let name = format!("Uploaded {}", &now[..16]);
        let _backup = Backup {
            id: id.clone(),
            name: name.clone(),
            created_at: now.clone(),
            file_path: full_path.clone(),
            size_bytes: size as u64,
        };
        db.insert_backup(&_backup)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

        return Ok(axum::http::StatusCode::CREATED);
    }

    Err((StatusCode::BAD_REQUEST, "No file uploaded".to_string()))
}

pub async fn download_backup(
    Path(id): Path<String>,
    State(db): State<AppState>,
) -> Result<Response<axum::body::Body>, (StatusCode, String)> {
    let file_path = db
        .get_backup_file_path(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;

    let path = StdPath::new(&file_path);
    let filename = path
        .file_name()
        .ok_or((StatusCode::NOT_FOUND, "Invalid filename".to_string()))?
        .to_str()
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid encoding".to_string(),
        ))?;
    let metadata = fs::metadata(path)
        .map_err(|_| (StatusCode::NOT_FOUND, "Backup not found".to_string()))?;

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "Backup not found".to_string()))?;
    let stream = tokio_util::io::ReaderStream::new(file);

    let body = axum::body::Body::from_stream(stream);

    let response = Response::builder()
        .header("Content-Type", "application/gzip")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", filename),
        )
        .header("Content-Length", metadata.len().to_string())
        .body(body)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string()))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::to_bytes,
        http::{Request, StatusCode},
        routing::put,
        Router,
    };
    use flate2::{write::GzEncoder, Compression};
    use std::sync::{Mutex, OnceLock};
    use tar::Builder;
    use tower::ServiceExt;

    static BACKUP_HTTP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "screentra-backup-http-{label}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    fn append_test_file(builder: &mut Builder<GzEncoder<std::fs::File>>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, data).unwrap();
    }

    #[tokio::test]
    async fn restore_http_dry_run_reports_addon_and_trusted_service_state() {
        let _guard = BACKUP_HTTP_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        let root = test_root("dry-run");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let archive_path = root.join("backup.tar.gz");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let encoder = GzEncoder::new(archive_file, Compression::default());
        let mut builder = Builder::new(encoder);
        append_test_file(&mut builder, "app/server", b"binary");
        append_test_file(&mut builder, "data/signage.db", b"sqlite");
        append_test_file(
            &mut builder,
            "data/trusted-services/example.weather/manifest.json",
            b"manifest",
        );
        builder.into_inner().unwrap().finish().unwrap();

        let db_path = root.join("metadata.db");
        let db = crate::db::Database::open(db_path.to_str().unwrap()).unwrap();
        db.insert_backup(&crate::models::Backup {
            id: "dry-run".to_string(),
            name: "Dry run".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            file_path: archive_path.to_string_lossy().to_string(),
            size_bytes: std::fs::metadata(&archive_path).unwrap().len(),
        })
        .unwrap();

        let old_cwd = std::env::current_dir().unwrap();
        let old_data_dir = std::env::var_os("SIGNAGE_DATA_DIR");
        std::env::set_current_dir(&root).unwrap();
        unsafe {
            std::env::set_var("SIGNAGE_DATA_DIR", &data_dir);
        }
        let app = Router::new()
            .route("/api/backups/{id}", put(restore_backup))
            .with_state(BackupMetadataRepository::Sqlite(std::sync::Arc::new(db)));
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/backups/dry-run?dry_run=true")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "dry_run");
        assert_eq!(payload["report"]["contains_core_database"], true);
        assert_eq!(payload["report"]["contains_trusted_service_staging"], true);
        assert_eq!(payload["report"]["warnings"].as_array().unwrap().len(), 0);

        std::env::set_current_dir(old_cwd).unwrap();
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
}
