// ── Version Manager ──────────────────────────────────────
// Provides information about the running version, and the ability to
// export/update the application itself (binary + static frontend + deps manifest).
//
// Two bundle types:
//   - state bundle (.state.tar.gz): database, uploads, backups index.
//     For migrating app state to a new device. Existing feature.
//   - update bundle (.update.tar.gz): binary + static/ + deps manifest.
//     For upgrading the running application to a new version. NEW.
//
// Update bundle import is safe: it stages the new files, validates the
// new binary, swaps them into place atomically, then restarts. If the
// new binary fails to start, it auto-rolls back to the previous one.

use crate::db::Database;
use crate::safe_archive;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;
fn signage_data_dir() -> String {
    std::env::var("SIGNAGE_DATA_DIR").unwrap_or_else(|_| "/home/board/data".to_string())
}

fn bounded_command_stdout(
    mut command: Command,
    maximum: usize,
) -> Result<(std::process::ExitStatus, Vec<u8>), String> {
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn command: {error}"))?;
    let mut output = Vec::new();
    let read_result = child
        .stdout
        .take()
        .ok_or_else(|| "command stdout was not piped".to_string())?
        .take((maximum as u64).saturating_add(1))
        .read_to_end(&mut output);
    if let Err(error) = read_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("read command output: {error}"));
    }
    if output.len() > maximum {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("command output exceeded {maximum} bytes"));
    }
    child
        .wait()
        .map(|status| (status, output))
        .map_err(|error| format!("wait command: {error}"))
}

/// Build metadata injected at compile time. Override with:
///   SIGNAGE_VERSION=v1.2.3 cargo build --release
///   SIGNAGE_BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ) cargo build --release
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PKG_NAME: &str = env!("CARGO_PKG_NAME");
pub const BUILD_TIME: &str = match option_env!("SIGNAGE_BUILD_TIME") {
    Some(t) => t,
    None => "unknown",
};
pub const GIT_COMMIT: &str = match option_env!("SIGNAGE_GIT_COMMIT") {
    Some(c) => c,
    None => "unknown",
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    pub pkg_name: String,
    pub build_time: String,
    pub git_commit: String,
    pub uptime_seconds: u64,
    pub binary_path: String,
    pub install_dir: String,
    pub database_path: String,
    pub static_dir: String,
    pub backups_dir: String,
    pub uploads_dir: String,
}

/// Collect version + system info. Called by `GET /api/version`.
pub fn collect_version_info() -> VersionInfo {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("unknown"));
    let install_dir = exe
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let local_share = signage_data_dir();

    VersionInfo {
        version: VERSION.to_string(),
        pkg_name: PKG_NAME.to_string(),
        build_time: BUILD_TIME.to_string(),
        git_commit: GIT_COMMIT.to_string(),
        uptime_seconds: uptime_seconds(),
        binary_path: exe.to_string_lossy().to_string(),
        install_dir,
        database_path: format!("{}/signage.db", local_share),
        static_dir: static_dir_path().to_string_lossy().to_string(),
        backups_dir: format!("{}/backups", install_dir_for_static()),
        uploads_dir: format!("{}/uploads", local_share),
    }
}

pub fn configured_service() -> (String, &'static str) {
    if let Ok(name) = std::env::var("SIGNAGE_SERVICE_NAME") {
        let name = name.trim();
        let kind = match std::env::var("SIGNAGE_SERVICE_KIND").ok().as_deref() {
            Some("user") => "user",
            Some("system") | None => "system",
            _ => "",
        };
        let allowed = [
            ("screentra-cast-server.service", "system"),
            ("signage.service", "system"),
            ("screentra-cast-server.service", "user"),
            ("signage-server.service", "user"),
        ];
        if allowed.iter().any(|(candidate, candidate_kind)| {
            *candidate == name && *candidate_kind == kind
        }) {
            return (name.to_string(), kind);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        for name in ["screentra-cast-server.service", "signage-server.service"] {
            if Path::new(&format!("{home}/.config/systemd/user/{name}")).exists() {
                return (name.to_string(), "user");
            }
        }
    }
    for name in ["screentra-cast-server.service", "signage.service"] {
        if ["/etc/systemd/system", "/lib/systemd/system"]
            .iter()
            .any(|dir| Path::new(&format!("{dir}/{name}")).exists())
        {
            return (name.to_string(), "system");
        }
    }
    ("screentra-cast-server.service".to_string(), "system")
}

pub fn restart_configured_service() -> Result<(), String> {
    let (service, kind) = configured_service();
    let mut command = if kind == "user" {
        let mut command = Command::new("systemctl");
        command.arg("--user");
        command
    } else {
        let mut command = Command::new("sudo");
        command.arg("-n").arg("systemctl");
        command
    };
    let status = command
        .arg("restart")
        .arg(&service)
        .status()
        .map_err(|error| format!("restart {service}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("restart {service} exited with {:?}", status.code()))
    }
}

/// Find the install/base directory that owns `static/`.
///
/// Supports both installed deployments (`/opt/digital-signage/static`) and
/// source-tree/dev deployments where the running binary is under
/// `target/{debug,release}/signage` but static files live at `../.. /static`.
pub fn install_dir_for_static() -> String {
    static_dir_path()
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

/// Find the static directory currently used by this instance.
pub fn static_dir_path() -> PathBuf {
    // Installed layout first.
    let installed = PathBuf::from("/opt/digital-signage/static");
    if installed.exists() {
        return installed;
    }

    // Optional explicit override for unusual deployments.
    if let Ok(p) = std::env::var("SIGNAGE_STATIC_DIR") {
        let path = PathBuf::from(p);
        if path.exists() {
            return path;
        }
    }

    // Source-tree layout. If current_exe is target/debug/signage or
    // target/release/signage, repo root is two parents above target profile.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(profile_dir) = exe.parent() {
            let sibling = profile_dir.join("static");
            if sibling.exists() {
                return sibling;
            }
            if let Some(target_dir) = profile_dir.parent() {
                if let Some(repo_dir) = target_dir.parent() {
                    let repo_static = repo_dir.join("static");
                    if repo_static.exists() {
                        return repo_static;
                    }
                }
            }
            return sibling;
        }
    }

    PathBuf::from("static")
}

pub fn uptime_seconds() -> u64 {
    // Read /proc/uptime — Linux-specific, but the kiosk is Linux.
    if let Ok(s) = fs::read_to_string("/proc/uptime") {
        if let Some(first) = s.split_whitespace().next() {
            if let Ok(f) = first.parse::<f64>() {
                return f as u64;
            }
        }
    }
    0
}

// ── State export/import (for migration to a new device) ──

/// Bundle the entire app state into a tarball at the given destination path.
/// Includes: database, uploaded files, backups, static/.
pub fn export_state_bundle(dest: &Path) -> Result<(), String> {
    let local_share = signage_data_dir();
    let install_dir = install_dir_for_static();

    // Create a temp staging directory (in the install dir to avoid /tmp being a tmpfs).
    let stage = std::path::PathBuf::from(install_dir_for_static())
        .join(format!(".signage-export-{}", Uuid::new_v4()));
    fs::create_dir_all(&stage).map_err(|e| format!("create staging dir: {}", e))?;

    // 1. Copy the database
    let db_src = format!("{}/signage.db", local_share);
    if Path::new(&db_src).exists() {
        fs::copy(&db_src, stage.join("signage.db")).map_err(|e| format!("copy db: {}", e))?;
    }

    // 2. Copy uploads (user-uploaded files: music, images, etc.)
    let uploads_src = format!("{}/uploads", local_share);
    if Path::new(&uploads_src).exists() {
        copy_dir_recursive(Path::new(&uploads_src), &stage.join("uploads"))
            .map_err(|e| format!("copy uploads: {}", e))?;
    }

    // 3. Copy backups INDEX only (skip the .tar.gz files which can be huge)
    let backups_src = format!("{}/backups", install_dir);
    let backups_dst = stage.join("backups");
    fs::create_dir_all(&backups_dst).ok();
    if Path::new(&backups_src).exists() {
        if let Ok(entries) = fs::read_dir(&backups_src) {
            for entry in entries.flatten() {
                let from = entry.path();
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                // Skip large tarball files; they would bloat the bundle and can be re-exported separately.
                if name_str.ends_with(".tar.gz") {
                    continue;
                }
                let to = backups_dst.join(&name);
                if from.is_dir() {
                    copy_dir_recursive(&from, &to).ok();
                } else {
                    fs::copy(&from, &to).ok();
                }
            }
        }
    }

    // 4. Add a manifest with version info
    let info = collect_version_info();
    let manifest = serde_json::to_string_pretty(&info).unwrap_or_default();
    fs::write(stage.join("manifest.json"), manifest)
        .map_err(|e| format!("write manifest: {}", e))?;

    // 5. Tar it all up
    let tar_status = Command::new("tar")
        .arg("-czf")
        .arg(dest)
        .arg("-C")
        .arg(stage.parent().unwrap_or(Path::new("/tmp")))
        .arg(stage.file_name().unwrap())
        .status();
    match tar_status {
        Ok(s) if s.success() => {
            // Best-effort cleanup
            let _ = fs::remove_dir_all(&stage);
            Ok(())
        }
        Ok(s) => Err(format!("tar exited with {:?}", s.code())),
        Err(e) => Err(format!("failed to run tar: {}", e)),
    }
}

/// Restore the state from a tarball produced by `export_state_bundle`.
/// WARNING: this OVERWRITES the current database, uploads, and backups.
pub fn import_state_bundle(src: &Path) -> Result<(), String> {
    if !src.exists() {
        return Err(format!("Source tarball not found: {}", src.display()));
    }

    let local_share = signage_data_dir();
    let install_dir = install_dir_for_static();

    // Create a temp extraction directory
    let stage = std::path::PathBuf::from(install_dir_for_static())
        .join(format!(".signage-import-{}", Uuid::new_v4()));
    fs::create_dir_all(&stage).map_err(|e| format!("create staging dir: {}", e))?;

    if let Err(error) = safe_archive::extract_tar_gz(src, &stage, 0) {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("safe archive extraction failed: {error}"));
    }

    // 2. Find the actual extracted subdirectory (tar -C creates a subdir matching the original name)
    let mut extracted = None;
    if let Ok(entries) = fs::read_dir(&stage) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                extracted = Some(entry.path());
                break;
            }
        }
    }
    let extracted = extracted.ok_or_else(|| "no extracted directory found".to_string())?;

    // 3. Restore the database
    let db_src = extracted.join("signage.db");
    if db_src.exists() {
        fs::create_dir_all(&local_share).ok();
        // Back up the current DB first (in case something goes wrong)
        let db_dest = format!("{}/signage.db", local_share);
        if Path::new(&db_dest).exists() {
            let backup_path = format!("{}/signage.db.before-import", local_share);
            let _ = fs::copy(&db_dest, &backup_path);
        }
        fs::copy(&db_src, &db_dest).map_err(|e| format!("restore db: {e}"))?;
        // Sanitize the freshly-restored DB: drop any leftover legacy Phase 3
        // tables that may be in the bundle. The migration's idempotent DROPs
        // would handle this on next startup, but sanitizing here means the
        // next restart doesn't have to touch the dead data, and the export
        // immediately after an import never re-bundles the legacy tables.
        // Best-effort: log on failure but don't abort the import.
        if let Err(e) = Database::sanitize_imported_db(Path::new(&db_dest)) {
            log::warn!(
                "import sanitizer could not clean {}: {} (will be cleaned at next startup)",
                db_dest,
                e
            );
        }
    }

    // 4. Restore uploads
    let uploads_src = extracted.join("uploads");
    if uploads_src.exists() {
        let dest = format!("{}/uploads", local_share);
        let _ = fs::create_dir_all(&dest);
        // Remove existing uploads and replace
        let _ = fs::remove_dir_all(&dest);
        copy_dir_recursive(&uploads_src, Path::new(&dest))
            .map_err(|e| format!("restore uploads: {}", e))?;
    }

    // 5. Restore backups
    let backups_src = extracted.join("backups");
    if backups_src.exists() {
        let dest = format!("{}/backups", install_dir);
        let _ = fs::create_dir_all(&dest);
        let _ = fs::remove_dir_all(&dest);
        copy_dir_recursive(&backups_src, Path::new(&dest))
            .map_err(|e| format!("restore backups: {}", e))?;
    }

    // 6. Cleanup
    let _ = fs::remove_dir_all(&stage);
    Ok(())
}

// ── Update Bundle export/import (for upgrading the application) ──

/// Manifest embedded in every update bundle. Records the version of the
/// binary and the SHA256 of the `signage` binary inside the bundle so the
/// importer can verify the download wasn't corrupted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// Migration metadata (matches VersionInfo for the source server)
    pub version: String,
    pub pkg_name: String,
    pub build_time: String,
    pub git_commit: String,
    pub source_binary_path: String,
    pub source_install_dir: String,

    /// Binary SHA256 (hex) — verify after upload
    pub binary_sha256: String,
    pub binary_size_bytes: u64,
    /// Canonical SHA256 of every application-static file in the bundle.
    pub static_sha256: String,

    /// Files included in the bundle
    pub has_binary: bool,
    pub has_static: bool,
    pub has_changelog: bool,

    /// Detected runtime dependencies. The importer checks these are present
    /// before swapping the binary.
    pub runtime_dependencies: RuntimeDeps,

    /// When the bundle was created
    pub exported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDeps {
    /// Names of executables that must be on PATH
    pub binaries: Vec<String>,
    /// Names of shared libraries that the binary depends on (best-effort)
    pub shared_libs: Vec<String>,
}

impl Default for RuntimeDeps {
    fn default() -> Self {
        RuntimeDeps {
            binaries: vec![
                "systemctl".to_string(),
                "tar".to_string(),
                "openssl".to_string(),
            ],
            shared_libs: vec![],
        }
    }
}

/// Detect the runtime dependencies the running binary has. Best-effort:
/// scans the binary for NEEDED entries via `ldd`, and probes PATH for
/// the well-known tooling.
pub fn detect_runtime_deps() -> RuntimeDeps {
    let binaries = vec![
        "systemctl".to_string(),
        "tar".to_string(),
        "openssl".to_string(),
    ];

    let mut shared_libs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut command = Command::new("ldd");
        command.arg(&exe);
        if let Ok((_, stdout)) = bounded_command_stdout(command, 64 * 1024) {
            if let Ok(s) = String::from_utf8(stdout) {
                for line in s.lines() {
                    // "libssl.so.3 => /usr/lib/..."
                    if let Some(name) = line.split_whitespace().next() {
                        // Strip the .so.N suffix to dedupe
                        let base = name.split(".so").next().unwrap_or(name);
                        if !shared_libs.contains(&base.to_string()) {
                            shared_libs.push(base.to_string());
                        }
                    }
                }
            }
        }
    }

    RuntimeDeps {
        binaries,
        shared_libs,
    }
}

/// SHA256 of a file as hex.
pub fn sha256_of_file(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("open {}: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {}", path.display(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Canonical SHA256 of a directory tree. File paths are sorted and included
/// alongside their contents so a signed manifest protects both filenames and
/// frontend bytes, not only the server binary.
pub fn sha256_of_directory(path: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_directory_files(path, path, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (relative, absolute) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let mut file = fs::File::open(&absolute)
            .map_err(|e| format!("open {}: {}", absolute.display(), e))?;
        let mut buf = [0u8; 8192];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("read {}: {}", absolute.display(), e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_directory_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    for entry in fs::read_dir(current)
        .map_err(|e| format!("read directory {}: {}", current.display(), e))?
    {
        let entry = entry.map_err(|e| format!("read directory entry: {e}"))?;
        let absolute = entry.path();
        if absolute.is_dir() {
            collect_directory_files(root, &absolute, files)?;
        } else {
            let relative = absolute
                .strip_prefix(root)
                .map_err(|e| format!("relative static path: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, absolute));
        }
    }
    Ok(())
}

/// Size of a file in bytes.
pub fn file_size(path: &Path) -> Result<u64, String> {
    let m = fs::metadata(path).map_err(|e| format!("stat {}: {}", path.display(), e))?;
    Ok(m.len())
}

fn update_signing_key_path() -> PathBuf {
    std::env::var_os("SCREENTRA_CAST_UPDATE_SIGNING_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/screentra-cast/server-update-signing.key"))
}

fn update_verify_key_path() -> PathBuf {
    std::env::var_os("SCREENTRA_CAST_UPDATE_VERIFY_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/screentra-cast/server-update-signing.pub"))
}

fn sign_update_manifest(manifest: &Path, signature: &Path) -> Result<(), String> {
    let key = update_signing_key_path();
    if !key.is_file() {
        return Err(format!(
            "update signing key is missing: {}",
            key.display()
        ));
    }
    let status = Command::new("openssl")
        .args(["pkeyutl", "-sign", "-rawin", "-inkey"])
        .arg(&key)
        .args(["-in"])
        .arg(manifest)
        .args(["-out"])
        .arg(signature)
        .status()
        .map_err(|e| format!("run update signing verifier: {e}"))?;
    if !status.success() {
        return Err("openssl could not sign update manifest".to_string());
    }
    Ok(())
}

fn verify_update_manifest_signature(
    manifest: &Path,
    signature: &Path,
    public_key: &Path,
) -> Result<(), String> {
    if !signature.is_file() {
        return Err("bundle is missing manifest.sig".to_string());
    }
    if !public_key.is_file() {
        return Err(format!(
            "approved update public key is missing: {}",
            public_key.display()
        ));
    }
    let status = Command::new("openssl")
        .args(["pkeyutl", "-verify", "-rawin", "-pubin", "-inkey"])
        .arg(public_key)
        .args(["-sigfile"])
        .arg(signature)
        .args(["-in"])
        .arg(manifest)
        .status()
        .map_err(|e| format!("run update signature verification: {e}"))?;
    if !status.success() {
        return Err("manifest signature verification failed".to_string());
    }
    Ok(())
}

/// Build an update bundle at the given destination. The bundle contains:
///   - signage          (the running binary)
///   - static/          (the frontend dir, excluding server-local CA trust)
///   - CHANGELOG.md     (optional, if present)
///   - manifest.json    (version info + binary SHA256 + runtime deps)
///   - manifest.sig     (detached Ed25519 signature over manifest.json)
pub fn export_update_bundle(dest: &Path) -> Result<UpdateManifest, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    let install_dir = install_dir_for_static();
    let static_dir = static_dir_path();

    if !exe.exists() {
        return Err(format!("binary not found at {}", exe.display()));
    }
    if !static_dir.exists() {
        return Err(format!("static dir not found at {}", static_dir.display()));
    }

    // Stage to a temp dir
    let stage = PathBuf::from(&install_dir)
        .join(format!(".signage-update-export-{}", Uuid::new_v4()));
    fs::create_dir_all(&stage).map_err(|e| format!("create staging dir: {}", e))?;

    // 1. Copy the binary
    let binary_name = "signage";
    fs::copy(&exe, stage.join(binary_name)).map_err(|e| format!("copy binary: {}", e))?;

    // 2. Copy static/
    let static_dst = stage.join("static");
    copy_dir_recursive_excluding(
        &static_dir,
        &static_dst,
        &["uploads", "backups", "screentra-cast-signage-ca.crt"],
    )
    .map_err(|e| format!("copy static: {}", e))?;

    // 3. Copy CHANGELOG.md if present (source repo only; not always there)
    let changelog_src = PathBuf::from(&install_dir).join("CHANGELOG.md");
    let has_changelog = changelog_src.exists();
    if has_changelog {
        let _ = fs::copy(&changelog_src, stage.join("CHANGELOG.md"));
    }

    // 4. Build the manifest
    let binary_sha256 = sha256_of_file(&exe)?;
    let binary_size = file_size(&exe)?;
    let runtime_dependencies = detect_runtime_deps();

    let manifest = UpdateManifest {
        version: VERSION.to_string(),
        pkg_name: PKG_NAME.to_string(),
        build_time: BUILD_TIME.to_string(),
        git_commit: GIT_COMMIT.to_string(),
        source_binary_path: exe.to_string_lossy().to_string(),
        source_install_dir: install_dir.clone(),
        binary_sha256,
        binary_size_bytes: binary_size,
        static_sha256: sha256_of_directory(&static_dst)?,
        has_binary: true,
        has_static: true,
        has_changelog,
        runtime_dependencies,
        exported_at: Utc::now().to_rfc3339(),
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("serialize manifest: {}", e))?;
    let manifest_path = stage.join("manifest.json");
    fs::write(&manifest_path, manifest_json)
        .map_err(|e| format!("write manifest: {}", e))?;
    if let Err(e) = sign_update_manifest(&manifest_path, &stage.join("manifest.sig")) {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }

    // 5. Tar it up
    let tar_status = Command::new("tar")
        .arg("-czf")
        .arg(dest)
        .arg("-C")
        .arg(stage.parent().unwrap_or(Path::new("/tmp")))
        .arg(stage.file_name().unwrap())
        .status();
    match tar_status {
        Ok(s) if s.success() => {
            let _ = fs::remove_dir_all(&stage);
            Ok(manifest)
        }
        Ok(s) => Err(format!("tar exited with {:?}", s.code())),
        Err(e) => Err(format!("failed to run tar: {}", e)),
    }
}

/// Result of an update import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResult {
    pub manifest: UpdateManifest,
    pub binary_swapped: bool,
    pub static_swapped: bool,
    pub changelog_saved: bool,
    pub backup_binary_path: String,
    pub backup_static_path: String,
    pub message: String,
}

fn validate_archive_member_name(name: &str) -> Result<(), String> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized.as_bytes().get(1) == Some(&b':')
        || normalized.chars().any(|c| c.is_control())
    {
        return Err(format!("unsafe archive member path: {name:?}"));
    }
    if normalized.split('/').any(|part| part == "..") {
        return Err(format!("archive member escapes staging directory: {name:?}"));
    }
    Ok(())
}

fn validate_archive_entry_type(kind: char) -> Result<(), String> {
    if matches!(kind, '-' | 'd') {
        Ok(())
    } else {
        Err(format!("archive contains unsupported special entry type: {kind}"))
    }
}

fn validate_update_archive(src: &Path) -> Result<(), String> {
    safe_archive::inspect_tar_gz(src)
        .map(|_| ())
        .map_err(|error| format!("unsafe archive: {error}"))
}

/// Apply an update bundle to this instance. The flow:
///   1. Extract to a staging dir
///   2. Verify manifest.json is present and valid
///   3. Verify manifest.sig against the pinned release public key
///   4. Verify the binary's SHA256 matches the manifest
///   5. Verify required runtime tooling is present
///   6. Validate the new binary runs: `./new_bin --version` should succeed
///   7. Back up the current binary and static/
///   8. Swap into place
///   9. Leave the .bak files in place for manual rollback
///  10. Schedule a restart
///
/// Returns the UpdateResult. The handler will restart the service after
/// returning success.
pub fn import_update_bundle(src: &Path, force_skip_rollback: bool) -> Result<UpdateResult, String> {
    if !src.exists() {
        return Err(format!("Source tarball not found: {}", src.display()));
    }

    let install_dir = install_dir_for_static();
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    let static_dir = static_dir_path();

    // 1. Extract to a staging dir inside the install dir (never /tmp).
    // install_dir is always a service-user-owned path, so files created
    // here are not world-readable. Fallback to a known-safe /var/tmp
    // location only if the install dir parent resolves to / (root filesystem).
    let stage = PathBuf::from(&install_dir)
                .join(format!(".signage-update-import-{}", Uuid::new_v4()));
    validate_update_archive(src)?;
    if let Err(error) = safe_archive::extract_tar_gz(src, &stage, 0) {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("safe archive extraction failed: {error}"));
    }

    // 2. Locate the extracted subdir
    let mut extracted = None;
    if let Ok(entries) = fs::read_dir(&stage) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                extracted = Some(entry.path());
                break;
            }
        }
    }
    let extracted = extracted.ok_or_else(|| "no extracted directory in tarball".to_string())?;

    // 3. Read & validate manifest
    let manifest_path = extracted.join("manifest.json");
    if !manifest_path.exists() {
        let _ = fs::remove_dir_all(&stage);
        return Err("bundle missing manifest.json — not a valid update bundle".to_string());
    }
    let manifest_bytes = fs::read(&manifest_path).map_err(|e| format!("read manifest: {}", e))?;
    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("parse manifest.json: {}", e))?;

    // The manifest signature is the release trust boundary. HTTPS and the
    // manifest's self-reported hashes only prove transport consistency; the
    // detached signature proves that an approved release key produced them.
    if let Err(e) = verify_update_manifest_signature(
        &manifest_path,
        &extracted.join("manifest.sig"),
        &update_verify_key_path(),
    ) {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("update bundle signature rejected: {e}"));
    }

    let actual_static_sha256 = sha256_of_directory(&extracted.join("static"))?;
    if actual_static_sha256 != manifest.static_sha256 {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!(
            "static SHA256 mismatch: expected {}, got {}",
            manifest.static_sha256, actual_static_sha256
        ));
    }

    // 4. Verify binary SHA256
    let new_binary = extracted.join("signage");
    if !new_binary.exists() {
        let _ = fs::remove_dir_all(&stage);
        return Err("bundle missing `signage` binary".to_string());
    }
    let new_sha = sha256_of_file(&new_binary)?;
    if new_sha != manifest.binary_sha256 {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!(
            "binary SHA256 mismatch: manifest says {}, file is {} — bundle may be corrupted",
            manifest.binary_sha256, new_sha
        ));
    }

    // 5. Verify runtime deps
    for bin in &manifest.runtime_dependencies.binaries {
        if !path_exists_in_path(bin) {
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("required runtime dependency missing: {}", bin));
        }
    }

    // 6. Validate the new binary runs usefully
    let test = Command::new(&new_binary)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match test {
        Ok(status) => {
            if !status.success() {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!(
                    "new binary refused to run (--version failed): {}",
                    status
                ));
            }
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("could not execute new binary: {}", e));
        }
    }

    // 7. Back up the current binary. (static backup is created on demand by
    // swap_static_atomic, which moves the existing static dir aside as
    // .rollback.<ts>; that path doubles as our rollback handle.)
    let ts = Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| Utc::now().timestamp_micros());
    let backup_binary_path = format!("{}.bak.{}", exe.to_string_lossy(), ts);
    let _ = fs::copy(&exe, &backup_binary_path);

    // 8. Swap the binary (atomic rename of the temp file)
    let binary_swapped = match swap_binary(&new_binary, &exe) {
        Ok(()) => true,
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("failed to swap binary: {}", e));
        }
    };

    // 9. Atomic static swap (staged via .next.<ts>, old moved to .rollback.<ts>)
    let new_static = extracted.join("static");
    let mut static_swapped = false;
    let mut static_rollback_path: Option<PathBuf> = None;
    if new_static.exists() {
        match swap_static_atomic(
            &new_static,
            &static_dir,
            &["uploads", "backups", "screentra-cast-signage-ca.crt"],
            ts,
        ) {
            Ok(rollback_path) => {
                static_swapped = true;
                static_rollback_path = Some(rollback_path);
            }
            Err(e) => {
                let backup_binary = PathBuf::from(&backup_binary_path);
                if !force_skip_rollback {
                    // Auto-rollback the binary and any partially-staged state.
                    let backup_static_path = static_dir
                        .parent()
                        .map(|p| p.join(format!("static.rollback.{}", ts)))
                        .unwrap_or_else(|| {
                            PathBuf::from(format!("{}/static.rollback.{}", install_dir, ts))
                        });
                    let _ = restore_from_backup(
                        &backup_binary,
                        &backup_static_path,
                        &exe,
                        &static_dir,
                    );
                    let _ = fs::remove_dir_all(&stage);
                    write_last_update_failed(
                        &install_dir,
                        &manifest,
                        &format!("auto-rollback: {}", e),
                        &backup_binary_path,
                        &backup_static_path.to_string_lossy(),
                    );
                    return Err(format!("static swap failed (auto-rolled back): {}", e));
                } else {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(format!(
                        "static swap failed (force=true, NOT rolled back): {}",
                        e
                    ));
                }
            }
        }

        // Republish local CA after static swap succeeds.
        if let Err(e) = republish_local_ca(&static_dir) {
            if !force_skip_rollback {
                // Restore both binary and static.
                let static_bak = static_rollback_path.clone().unwrap_or_else(|| {
                    PathBuf::from(format!("{}/static.bak.rollback.{}", install_dir, ts))
                });
                let _ = restore_from_backup(
                    &PathBuf::from(&backup_binary_path),
                    &static_bak,
                    &exe,
                    &static_dir,
                );
                let _ = fs::remove_dir_all(&stage);
                write_last_update_failed(
                    &install_dir,
                    &manifest,
                    &format!("auto-rollback: {}", e),
                    &backup_binary_path,
                    &static_bak.to_string_lossy(),
                );
                return Err(format!(
                    "local CA republish failed (auto-rolled back): {}",
                    e
                ));
            } else {
                let _ = fs::remove_dir_all(&stage);
                return Err(format!(
                    "local CA republish failed (force=true, NOT rolled back): {}",
                    e
                ));
            }
        }
    }

    let backup_static_path = static_rollback_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("{}/static.bak.{}", install_dir, ts));

    // 10. Save CHANGELOG.md if present and no install-dir one exists
    let changelog_saved = false;
    let new_changelog = extracted.join("CHANGELOG.md");
    if new_changelog.exists() {
        let dest = PathBuf::from(&install_dir).join("CHANGELOG.md");
        let _ = fs::copy(&new_changelog, &dest);
    }
    let _ = changelog_saved; // future: return whether we wrote a new one

    // 11. Cleanup staging
    let _ = fs::remove_dir_all(&stage);

    // Success — clear any prior failure marker. Keep .bak files for one
    // more success cycle so an admin can still rollback to "the one before
    // this one" by hand; if they want them gone, they can rm.
    clear_last_update_failed(&install_dir);

    Ok(UpdateResult {
        manifest,
        binary_swapped,
        static_swapped,
        changelog_saved,
        backup_binary_path,
        backup_static_path,
        message: "Update applied. Service is restarting.".to_string(),
    })
}

/// Atomic-ish binary swap: move the new binary into a temp path adjacent
/// to the current one, then rename it over the current binary. On Linux
/// this is atomic on the same filesystem.
fn swap_binary(new_binary: &Path, dst: &Path) -> Result<(), String> {
    let dir = dst
        .parent()
        .ok_or_else(|| "dst has no parent".to_string())?;
    fs::create_dir_all(dir).map_err(|e| format!("create binary dir: {}", e))?;
    let tmp_path = dir.join(format!("signage.new.{}", Utc::now().timestamp()));
    fs::copy(new_binary, &tmp_path).map_err(|e| format!("copy new binary to staging: {}", e))?;

    // Preserve permissions when the current binary exists. If this is the
    // first self-update after a manual install, or the running path was deleted
    // by a previous failed update, the destination may not exist; still install
    // the validated new binary with normal executable permissions.
    match fs::metadata(dst) {
        Ok(metadata) => {
            let _ = fs::set_permissions(&tmp_path, metadata.permissions());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755));
            }
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("stat original: {}", e));
        }
    }

    fs::rename(&tmp_path, dst).map_err(|e| format!("rename into place: {}", e))?;
    Ok(())
}

/// Look up an executable in PATH.
fn path_exists_in_path(name: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let candidate = path.join(name);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

/// Republish this server's local CA after static asset updates.
///
/// Application Update bundles are code/assets only; they must not move CA trust
/// from the source server to the target. If the target has a local CA under the
/// TLS cert directory (or install-dir certs/), copy that public CA back to
/// static/install/screentra-cast-signage-ca.crt so clients fetch the CA that actually
/// signs this server's live certificate.
fn republish_local_ca(static_dir: &Path) -> Result<bool, String> {
    let cert_dir = std::env::var("SIGNAGE_TLS_CERT")
        .ok()
        .and_then(|p| PathBuf::from(p).parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from(install_dir_for_static()).join("certs"));
    let local_ca = cert_dir.join("screentra-cast-local-ca.crt");
    if !local_ca.exists() {
        return Ok(false);
    }
    let install_dir = static_dir.join("install");
    fs::create_dir_all(&install_dir).map_err(|e| format!("create static/install: {}", e))?;
    let published = install_dir.join("screentra-cast-signage-ca.crt");
    fs::copy(&local_ca, &published).map_err(|e| {
        format!(
            "copy {} to {}: {}",
            local_ca.display(),
            published.display(),
            e
        )
    })?;
    Ok(true)
}

/// Copy a directory and all its contents recursively. Best-effort.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Copy a directory recursively while skipping state/media/cert artifacts that
/// do not belong in application update bundles.
fn copy_dir_recursive_excluding(
    src: &Path,
    dst: &Path,
    skip_names: &[&str],
) -> std::io::Result<()> {
    if !src.exists() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if skip_names.iter().any(|skip| *skip == name_str) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_dir_recursive_excluding(&from, &to, skip_names)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Atomic-ish static swap: stage the merged new static as `.next.<ts>`,
/// carry over excluded state entries (uploads/, backups/, local CA) from
/// the live static, move the existing `static` aside as `.rollback.<ts>`,
/// then rename the staged dir into place.
fn swap_static_atomic(
    new_static_src: &Path,
    static_dir: &Path,
    skip_names: &[&str],
    timestamp: i64,
) -> Result<PathBuf, String> {
    let parent = static_dir
        .parent()
        .ok_or_else(|| "static dir has no parent".to_string())?;
    let staged = parent.join(format!("static.next.{}", timestamp));
    let rollback = parent.join(format!("static.rollback.{}", timestamp));

    fs::create_dir_all(&staged)
        .map_err(|e| format!("create staged dir {}: {}", staged.display(), e))?;

    // Step 1: stage the new app files into .next.<ts>
    if new_static_src.exists() {
        copy_dir_recursive_excluding(new_static_src, &staged, skip_names)
            .map_err(|e| format!("stage new static into {}: {}", staged.display(), e))?;
    }

    // Step 2: carry over excluded state entries from the live static. We
    // do this *before* moving the live static aside so we don't need to
    // handle a half-merged state if copy fails. If skip_names is empty
    // or the source static doesn't exist, this loop is a no-op.
    if static_dir.exists() {
        if let Ok(entries) = fs::read_dir(static_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !skip_names.iter().any(|skip| *skip == name_str) {
                    continue;
                }
                let from = entry.path();
                let to = staged.join(&name);
                if from.is_dir() {
                    copy_dir_recursive(&from, &to).map_err(|e| {
                        format!("carry over {} to {}: {}", from.display(), to.display(), e)
                    })?;
                } else {
                    fs::copy(&from, &to).map_err(|e| {
                        format!("carry over {} to {}: {}", from.display(), to.display(), e)
                    })?;
                }
            }
        }
    }

    // Step 3: move existing static aside.
    if static_dir.exists() {
        fs::rename(static_dir, &rollback).map_err(|e| {
            let _ = fs::remove_dir_all(&staged);
            format!(
                "move existing static {} to rollback {}: {}",
                static_dir.display(),
                rollback.display(),
                e
            )
        })?;
    }

    // Step 4: promote staged.
    if let Err(e) = fs::rename(&staged, static_dir) {
        if rollback.exists() {
            let _ = fs::rename(&rollback, static_dir);
        }
        let _ = fs::remove_dir_all(&staged);
        return Err(format!(
            "promote staged {} to {}: {}",
            staged.display(),
            static_dir.display(),
            e
        ));
    }
    Ok(rollback)
}

/// Restore both binary and static from the per-update backups.
fn restore_from_backup(
    backup_binary: &Path,
    backup_static: &Path,
    live_binary: &Path,
    static_dir: &Path,
) -> Result<bool, String> {
    let mut restored = false;
    if backup_binary.exists() {
        fs::copy(backup_binary, live_binary)
            .map_err(|e| format!("restore binary from {}: {}", backup_binary.display(), e))?;
        let _ = fs::remove_file(backup_binary);
        restored = true;
    }
    if backup_static.exists() {
        if static_dir.exists() {
            let _ = fs::remove_dir_all(static_dir);
        }
        if let Err(e) = fs::rename(backup_static, static_dir) {
            return Err(format!(
                "restore static from {}: {}",
                backup_static.display(),
                e
            ));
        }
        restored = true;
    }
    Ok(restored)
}

/// Marker written when an update fails mid-flight and gets auto-rolled back.
fn write_last_update_failed(
    install_dir: &str,
    manifest: &UpdateManifest,
    error: &str,
    backup_binary: &str,
    backup_static: &str,
) {
    let payload = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "git_commit": manifest.git_commit,
        "version": manifest.version,
        "error": error,
        "backup_binary": backup_binary,
        "backup_static": backup_static,
    });
    let path = std::path::PathBuf::from(install_dir).join("LAST_UPDATE_FAILED.json");
    if let Ok(s) = serde_json::to_string_pretty(&payload) {
        let _ = fs::write(&path, s);
    }
}

pub fn clear_last_update_failed(install_dir: &str) {
    let path = std::path::PathBuf::from(install_dir).join("LAST_UPDATE_FAILED.json");
    let _ = fs::remove_file(&path);
}

pub fn read_last_update_failed(install_dir: &str) -> Option<serde_json::Value> {
    let path = std::path::PathBuf::from(install_dir).join("LAST_UPDATE_FAILED.json");
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()
}

#[cfg(test)]
mod update_swap_tests {
    use super::*;

    #[test]
    fn archive_member_validation_rejects_traversal_and_absolute_paths() {
        assert!(validate_archive_member_name("bundle/static/index.html").is_ok());
        assert!(validate_archive_member_name("bundle/../outside").is_err());
        assert!(validate_archive_member_name("/etc/screentra-cast.env").is_err());
        assert!(validate_archive_member_name("bundle\\..\\outside").is_err());
    }

    #[test]
    fn archive_member_validation_rejects_special_file_types() {
        assert!(validate_archive_entry_type('-').is_ok());
        assert!(validate_archive_entry_type('d').is_ok());
        assert!(validate_archive_entry_type('l').is_err());
        assert!(validate_archive_entry_type('h').is_err());
        assert!(validate_archive_entry_type('p').is_err());
    }

    #[test]
    fn update_manifest_signature_is_required() {
        let root = std::env::temp_dir().join(format!(
            "ripley-update-signature-required-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("manifest.json");
        let signature = root.join("manifest.sig");
        fs::write(&manifest, b"{}\n").unwrap();
        let missing_key = root.join("missing.pub");
        assert!(verify_update_manifest_signature(&manifest, &signature, &missing_key).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn update_manifest_signature_round_trip_rejects_tampering() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "ripley-update-signature-roundtrip-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let key = root.join("signing.key");
        let public_key = root.join("signing.pub");
        let manifest = root.join("manifest.json");
        let signature = root.join("manifest.sig");
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519", "-out"])
            .arg(&key)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("openssl")
            .args(["pkey", "-in"])
            .arg(&key)
            .args(["-pubout", "-out"])
            .arg(&public_key)
            .status()
            .unwrap()
            .success());
        fs::write(&manifest, b"{\"version\":\"test\"}\n").unwrap();
        let previous = std::env::var_os("SCREENTRA_CAST_UPDATE_SIGNING_KEY");
        unsafe {
            std::env::set_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY", &key);
        }
        sign_update_manifest(&manifest, &signature).unwrap();
        verify_update_manifest_signature(&manifest, &signature, &public_key).unwrap();
        fs::write(&manifest, b"{\"version\":\"tampered\"}\n").unwrap();
        assert!(verify_update_manifest_signature(&manifest, &signature, &public_key).is_err());
        if let Some(value) = previous {
            unsafe {
                std::env::set_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY", value);
            }
        } else {
            unsafe {
                std::env::remove_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn exported_update_bundle_contains_detached_manifest_signature() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "ripley-update-export-signature-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let key = root.join("signing.key");
        let dest = root.join("update.tar.gz");
        assert!(Command::new("openssl")
            .args(["genpkey", "-algorithm", "Ed25519", "-out"])
            .arg(&key)
            .status()
            .unwrap()
            .success());
        let old_key = std::env::var_os("SCREENTRA_CAST_UPDATE_SIGNING_KEY");
        let old_static = std::env::var_os("SIGNAGE_STATIC_DIR");
        unsafe {
            std::env::set_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY", &key);
            std::env::set_var(
                "SIGNAGE_STATIC_DIR",
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"),
            );
        }
        export_update_bundle(&dest).unwrap();
        let listing = Command::new("tar")
            .args(["-tzf"])
            .arg(&dest)
            .output()
            .unwrap();
        assert!(listing.status.success());
        assert!(String::from_utf8_lossy(&listing.stdout)
            .lines()
            .any(|name| name.ends_with("/manifest.sig")));
        if let Some(value) = old_key {
            unsafe { std::env::set_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY", value); }
        } else {
            unsafe { std::env::remove_var("SCREENTRA_CAST_UPDATE_SIGNING_KEY"); }
        }
        if let Some(value) = old_static {
            unsafe { std::env::set_var("SIGNAGE_STATIC_DIR", value); }
        } else {
            unsafe { std::env::remove_var("SIGNAGE_STATIC_DIR"); }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_binary_allows_missing_original() {
        let root = std::env::temp_dir().join(format!(
            "ripley-swap-missing-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let new_binary = root.join("new-signage");
        let dst = root.join("bin").join("signage");
        fs::write(&new_binary, b"new binary").unwrap();
        swap_binary(&new_binary, &dst).expect("swap should create missing dst parent and binary");
        assert_eq!(fs::read(&dst).unwrap(), b"new binary");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_binary_replaces_existing_original() {
        let root = std::env::temp_dir().join(format!(
            "ripley-swap-existing-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let new_binary = root.join("new-signage");
        let dst = root.join("signage");
        fs::write(&new_binary, b"new binary").unwrap();
        fs::write(&dst, b"old binary").unwrap();
        swap_binary(&new_binary, &dst).expect("swap should replace existing binary");
        assert_eq!(fs::read(&dst).unwrap(), b"new binary");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn static_copy_skips_published_ca_file() {
        let root = std::env::temp_dir().join(format!(
            "ripley-copy-skip-ca-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(src.join("install")).unwrap();
        fs::write(src.join("app.js"), b"app").unwrap();
        fs::write(
            src.join("install").join("screentra-cast-signage-ca.crt"),
            b"source-ca",
        )
        .unwrap();
        copy_dir_recursive_excluding(&src, &dst, &["screentra-cast-signage-ca.crt"]).unwrap();
        assert_eq!(fs::read(dst.join("app.js")).unwrap(), b"app");
        assert!(!dst.join("install").join("screentra-cast-signage-ca.crt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn republish_local_ca_restores_target_ca_after_static_copy() {
        let _guard = crate::PROCESS_ENV_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "ripley-republish-ca-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let certs = root.join("certs");
        let static_dir = root.join("static");
        fs::create_dir_all(&certs).unwrap();
        fs::create_dir_all(static_dir.join("install")).unwrap();
        fs::write(certs.join("screentra-cast-local-ca.crt"), b"target-local-ca").unwrap();
        fs::write(
            static_dir.join("install").join("screentra-cast-signage-ca.crt"),
            b"stale-source-ca",
        )
        .unwrap();
        let old = std::env::var("SIGNAGE_TLS_CERT").ok();
        unsafe {
            std::env::set_var("SIGNAGE_TLS_CERT", certs.join("signage-server.crt"));
        }
        republish_local_ca(&static_dir).unwrap();
        if let Some(v) = old {
            unsafe {
                std::env::set_var("SIGNAGE_TLS_CERT", v);
            }
        } else {
            unsafe {
                std::env::remove_var("SIGNAGE_TLS_CERT");
            }
        }
        assert_eq!(
            fs::read(static_dir.join("install").join("screentra-cast-signage-ca.crt")).unwrap(),
            b"target-local-ca"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_static_atomic_replaces_existing_and_keeps_rollback() {
        let root = std::env::temp_dir().join(format!(
            "ripley-static-atomic-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let new_static = root.join("new_static");
        let static_dir = root.join("static");
        fs::create_dir_all(new_static.join("css")).unwrap();
        fs::write(new_static.join("css/app.css"), b"new-css").unwrap();
        fs::write(new_static.join("index.html"), b"<html>new</html>").unwrap();
        fs::create_dir_all(static_dir.join("uploads")).unwrap();
        fs::write(static_dir.join("old.html"), b"<html>old</html>").unwrap();
        fs::write(static_dir.join("uploads/img.png"), b"keep").unwrap();
        // The caller supplies the update timestamp so the rollback handle is
        // deterministic and can be restored if a later update step fails.
        let rollback = swap_static_atomic(&new_static, &static_dir, &["uploads"], 4242)
            .expect("swap ok");
        assert_eq!(rollback, root.join("static.rollback.4242"));
        assert_eq!(
            fs::read(static_dir.join("css/app.css")).unwrap(),
            b"new-css"
        );
        assert_eq!(
            fs::read(static_dir.join("index.html")).unwrap(),
            b"<html>new</html>"
        );
        // uploads preserved (carried over to new static)
        assert_eq!(
            fs::read(static_dir.join("uploads/img.png")).unwrap(),
            b"keep"
        );
        // rollback exists and has old html
        let mut rollback_path: Option<std::path::PathBuf> = None;
        if let Ok(entries) = fs::read_dir(&root) {
            for e in entries.flatten() {
                let s = e.file_name().to_string_lossy().to_string();
                if s.starts_with("static.rollback.") {
                    rollback_path = Some(e.path());
                    break;
                }
            }
        }
        let rollback = rollback_path.expect("rollback dir exists");
        assert_eq!(
            fs::read(rollback.join("old.html")).unwrap(),
            b"<html>old</html>"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_from_backup_restores_binary_and_static() {
        let root = std::env::temp_dir().join(format!(
            "ripley-restore-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let install = root.join("install");
        let bin_dir = install.join("target").join("release");
        fs::create_dir_all(&bin_dir).unwrap();
        let live = bin_dir.join("signage");
        let bak = install.join("target/release/signage.bak.42");
        fs::write(&live, b"NEW-BROKEN").unwrap();
        fs::write(&bak, b"OLD-GOOD").unwrap();
        // set up static scenario
        let static_dir = install.join("static");
        let bak_static = install.join("static.rollback.42");
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("static-marker.txt"), b"new-static").unwrap();
        fs::create_dir_all(&bak_static).unwrap();
        fs::write(bak_static.join("static-marker.txt"), b"old-static").unwrap();
        let restored =
            restore_from_backup(&bak, &bak_static, &live, &static_dir).expect("restore ok");
        assert!(restored);
        assert_eq!(fs::read(&live).unwrap(), b"OLD-GOOD");
        assert_eq!(
            fs::read(static_dir.join("static-marker.txt")).unwrap(),
            b"old-static"
        );
        assert!(!bak.exists(), "binary bak should be removed");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn last_update_failed_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "ripley-lastfail-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        let install_str = root.to_string_lossy().to_string();
        assert!(read_last_update_failed(&install_str).is_none());
        let m = UpdateManifest {
            version: "0.3.0".to_string(),
            pkg_name: "signage".to_string(),
            build_time: chrono::Utc::now().to_rfc3339(),
            git_commit: "abc1234".to_string(),
            source_binary_path: "/tmp/x".to_string(),
            source_install_dir: "/tmp/y".to_string(),
            binary_sha256: "deadbeef".to_string(),
            binary_size_bytes: 100,
            static_sha256: "deadbeef".to_string(),
            has_binary: true,
            has_static: true,
            has_changelog: false,
            runtime_dependencies: RuntimeDeps::default(),
            exported_at: chrono::Utc::now().to_rfc3339(),
        };
        write_last_update_failed(
            &install_str,
            &m,
            "static swap failed",
            "/tmp/a.bak",
            "/tmp/static.bak",
        );
        let v = read_last_update_failed(&install_str).expect("present after write");
        assert_eq!(v["git_commit"], "abc1234");
        assert_eq!(v["error"], "static swap failed");
        clear_last_update_failed(&install_str);
        assert!(read_last_update_failed(&install_str).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
