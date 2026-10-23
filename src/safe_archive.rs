//! Safe gzip-compressed tar inspection and extraction.
//!
//! The system tar implementations available on supported targets do not all
//! accept the same path-safety flags.  Keep the security policy in-process so
//! archive handling does not depend on a particular tar CLI.

use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use tar::{Archive, EntryType};

const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_COMPRESSED_BYTES: u64 = 1 << 30;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 1 << 30;

struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive decompressed size limit exceeded",
            ));
        }
        let maximum = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..maximum])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn open_archive(path: &Path) -> Result<Archive<LimitedReader<GzDecoder<File>>>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| format!("inspect archive: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("archive is not a regular file".to_string());
    }
    if metadata.len() > MAX_ARCHIVE_COMPRESSED_BYTES {
        return Err(format!(
            "archive exceeds compressed size limit of {MAX_ARCHIVE_COMPRESSED_BYTES} bytes"
        ));
    }
    let file = File::open(path).map_err(|error| format!("open archive: {error}"))?;
    let decoder = GzDecoder::new(file);
    Ok(Archive::new(LimitedReader {
        inner: decoder,
        remaining: MAX_ARCHIVE_UNCOMPRESSED_BYTES + 1,
    }))
}

fn normalized_components(path: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "archive contains traversal path: {}",
                    path.display()
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "archive contains absolute path: {}",
                    path.display()
                ));
            }
        }
    }
    if components.is_empty() {
        return Err("archive contains an empty path".to_string());
    }
    Ok(components)
}

fn validate_entry_type(entry_type: EntryType, path: &Path) -> Result<(), String> {
    if entry_type.is_file() || entry_type.is_dir() {
        return Ok(());
    }
    Err(format!(
        "archive contains unsupported entry type at {}",
        path.display()
    ))
}

fn inspect_entries<R: std::io::Read>(archive: &mut Archive<R>) -> Result<Vec<String>, String> {
    let mut top_level = Vec::new();
    for (index, entry) in archive
        .entries()
        .map_err(|error| format!("read archive entries: {error}"))?
        .enumerate()
    {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(format!("archive contains more than {MAX_ARCHIVE_ENTRIES} entries"));
        }
        let entry = entry.map_err(|error| format!("read archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read archive path: {error}"))?
            .into_owned();
        let components = normalized_components(&path)?;
        validate_entry_type(entry.header().entry_type(), &path)?;
        if let Some(first) = components.first() {
            let first = first.to_string_lossy().to_string();
            if !top_level.contains(&first) {
                top_level.push(first);
            }
        }
    }
    top_level.sort();
    Ok(top_level)
}

/// Validate a gzip-compressed tar archive and return its top-level names.
pub fn inspect_tar_gz(path: &Path) -> Result<Vec<String>, String> {
    let mut archive = open_archive(path)?;
    inspect_entries(&mut archive)
}

fn archive_contains_path_matching(
    path: &Path,
    wanted_path: &Path,
    prefix: bool,
) -> Result<bool, String> {
    let wanted = normalized_components(wanted_path)?;
    let mut archive = open_archive(path)?;
    let mut found = false;
    for (index, entry) in archive
        .entries()
        .map_err(|error| format!("read archive entries: {error}"))?
        .enumerate()
    {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(format!("archive contains more than {MAX_ARCHIVE_ENTRIES} entries"));
        }
        let entry = entry.map_err(|error| format!("read archive entry: {error}"))?;
        let entry_path = entry
            .path()
            .map_err(|error| format!("read archive path: {error}"))?
            .into_owned();
        let components = normalized_components(&entry_path)?;
        validate_entry_type(entry.header().entry_type(), &entry_path)?;
        let matches = if prefix {
            components.len() >= wanted.len() && components[..wanted.len()] == wanted[..]
        } else {
            components == wanted
        };
        if matches {
            found = true;
        }
    }
    Ok(found)
}

/// Return whether a gzip-compressed tar archive contains an entry under a
/// normalized path prefix. Entry paths are validated with the same safety
/// policy as extraction.
pub fn archive_contains_path_prefix(path: &Path, prefix: &Path) -> Result<bool, String> {
    archive_contains_path_matching(path, prefix, true)
}

/// Return whether a gzip-compressed tar archive contains an exact normalized
/// path. Entry paths are validated with the same safety policy as extraction.
pub fn archive_contains_path(path: &Path, wanted: &Path) -> Result<bool, String> {
    archive_contains_path_matching(path, wanted, false)
}

fn ensure_safe_parent(root: &Path, parent: &Path) -> Result<(), String> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("inspect extraction root: {error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("extraction root is not a real directory".to_string());
    }

    let relative = parent
        .strip_prefix(root)
        .map_err(|_| "extraction path escaped root".to_string())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(value) = component else {
            return Err("extraction path contains an unsafe component".to_string());
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "extraction parent is not a real directory: {}",
                        current.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|create_error| {
                    format!("create extraction directory: {create_error}")
                })?;
            }
            Err(error) => return Err(format!("inspect extraction parent: {error}")),
        }
    }
    Ok(())
}

/// Validate and extract a gzip-compressed tar archive.
///
/// `strip_components` removes that many leading normal path components after
/// validation. Entries that become an empty path are allowed only for
/// directories (for example, the archive's `app/` root when stripping one
/// component) and are skipped.
pub fn extract_tar_gz(
    path: &Path,
    destination: &Path,
    strip_components: usize,
) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| format!("create extraction root: {error}"))?;
    let metadata = fs::symlink_metadata(destination)
        .map_err(|error| format!("inspect extraction root: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("extraction root is not a real directory".to_string());
    }

    let mut archive = open_archive(path)?;
    for (index, entry) in archive
        .entries()
        .map_err(|error| format!("read archive entries: {error}"))?
        .enumerate()
    {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(format!(
                "archive contains more than {MAX_ARCHIVE_ENTRIES} entries"
            ));
        }
        let mut entry = entry.map_err(|error| format!("read archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read archive path: {error}"))?
            .into_owned();
        let components = normalized_components(&path)?;
        validate_entry_type(entry.header().entry_type(), &path)?;
        if components.len() < strip_components {
            if entry.header().entry_type().is_dir() {
                continue;
            }
            return Err(format!(
                "archive file has no path after stripping: {}",
                path.display()
            ));
        }
        let output_components = &components[strip_components..];
        if output_components.is_empty() {
            if entry.header().entry_type().is_dir() {
                continue;
            }
            return Err(format!(
                "archive file has an empty output path: {}",
                path.display()
            ));
        }

        let mut output = PathBuf::from(destination);
        for component in output_components {
            output.push(component);
        }
        let parent = output
            .parent()
            .ok_or_else(|| "archive output has no parent".to_string())?;
        ensure_safe_parent(destination, parent)?;
        if let Ok(metadata) = fs::symlink_metadata(&output) {
            let file_type = metadata.file_type();
            if file_type.is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
                return Err(format!(
                    "archive would overwrite an unsafe filesystem entry: {}",
                    output.display()
                ));
            }
        }

        if entry.header().entry_type().is_dir() {
            if let Ok(metadata) = fs::symlink_metadata(&output) {
                if !metadata.is_dir() {
                    return Err(format!(
                        "archive directory conflicts with a file: {}",
                        output.display()
                    ));
                }
            } else {
                fs::create_dir(&output)
                    .map_err(|error| format!("create extracted directory: {error}"))?;
            }
        } else {
            entry
                .unpack(&output)
                .map_err(|error| format!("extract {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use tar::Builder;

    fn archive_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "screentra-safe-archive-{label}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn rejects_oversized_compressed_archives() {
        let path = archive_path("oversized");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_ARCHIVE_COMPRESSED_BYTES + 1).unwrap();
        let error = inspect_tar_gz(&path).unwrap_err();
        assert!(error.contains("compressed size limit"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn limited_reader_rejects_decompressed_overflow() {
        let mut reader = LimitedReader {
            inner: io::Cursor::new(vec![b'x'; 4]),
            remaining: 3,
        };
        let mut bytes = Vec::new();
        let error = reader.read_to_end(&mut bytes).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("decompressed size limit"));
        assert_eq!(bytes, b"xxx");
    }

    fn make_archive<F>(label: &str, build: F) -> PathBuf
    where
        F: FnOnce(&mut Builder<GzEncoder<File>>),
    {
        let path = archive_path(label);
        let file = File::create(&path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);
        build(&mut builder);
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn detects_nested_backup_state_prefixes() {
        let archive = make_archive("backup-state", |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "data/addon_storage/widget/state.json", &b"x"[..])
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    "data/trusted-services/example.weather/manifest.json",
                    &b"x"[..],
                )
                .unwrap();
        });
        assert!(archive_contains_path_prefix(
            &archive,
            Path::new("data/addon_storage")
        )
        .unwrap());
        assert!(archive_contains_path_prefix(
            &archive,
            Path::new("data/trusted-services")
        )
        .unwrap());
        assert!(!archive_contains_path_prefix(
            &archive,
            Path::new("data/missing")
        )
        .unwrap());
        let _ = fs::remove_file(archive);
    }

    #[test]
    fn extracts_normal_archive_after_stripping_root() {
        let archive = make_archive("normal", |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "app/static/index.html", &b"hello"[..])
                .unwrap();
        });
        let destination = archive_path("normal-destination");
        extract_tar_gz(&archive, &destination, 1).unwrap();
        assert_eq!(
            fs::read(destination.join("static/index.html")).unwrap(),
            b"hello"
        );
        let _ = fs::remove_file(archive);
        let _ = fs::remove_dir_all(destination);
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for path in ["app/../../escape", "/etc/passwd", "./app/../escape"] {
            assert!(
                normalized_components(Path::new(path)).is_err(),
                "must reject {path}"
            );
        }
    }

    #[test]
    fn rejects_symlinks_and_hardlinks() {
        for (label, entry_type) in [
            ("symlink", EntryType::symlink()),
            ("hardlink", EntryType::new(b'1')),
        ] {
            let archive = make_archive(label, |builder| {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(entry_type);
                header.set_size(0);
                header.set_link_name("target").unwrap();
                header.set_cksum();
                builder
                    .append_data(&mut header, "app/link", &[][..])
                    .unwrap();
            });
            assert!(inspect_tar_gz(&archive).is_err(), "must reject {label}");
            let _ = fs::remove_file(archive);
        }
    }

    #[test]
    fn rejects_device_entries() {
        let archive = make_archive("device", |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(EntryType::character_special());
            header.set_size(0);
            header.set_device_major(1).unwrap();
            header.set_device_minor(3).unwrap();
            header.set_cksum();
            builder
                .append_data(&mut header, "app/device", &[][..])
                .unwrap();
        });
        assert!(inspect_tar_gz(&archive).is_err());
        let _ = fs::remove_file(archive);
    }
}
