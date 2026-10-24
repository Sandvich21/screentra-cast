//! Server-side Google Fonts catalog and download.
//!
//! Source for the catalog: the public `gwfh.mranftl.com` API (no key
//! required, returns the same ~1,955 fonts the Google Fonts site
//! shows). The catalog is cached in-process for 24 hours to avoid
//! hammering the upstream and to keep `/api/fonts/catalog` snappy.
//!
//! Downloads come from the same upstream and produce `.woff2` files
//! extracted into `./uploads/fonts/<family>-<weight>.woff2`.
//! woff2 is the modern web standard (all current browsers, plus
//! 22KB vs 168KB for the TTF Roboto) and is the format the
//! runtime loads via `@font-face`.

use std::io::Read;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

/// Public catalog entry returned to the editor's font picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontCatalogEntry {
    pub id: String,
    pub family: String,
    pub category: String,
    pub variants: Vec<String>,
    pub subsets: Vec<String>,
    pub popularity: u32,
    pub version: String,
}

/// Public record of an installed font file. The `family` is the
/// display name; `weight` is the font-weight CSS value (e.g. 400,
/// 700). The `url` is a server-relative path the editor and player
/// can drop straight into `src: url(...)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledFont {
    pub family: String,
    pub weight: u32,
    pub url: String,
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct UpstreamFont {
    id: String,
    family: String,
    category: String,
    variants: Vec<String>,
    subsets: Vec<String>,
    popularity: u32,
    version: String,
}

const CATALOG_URL: &str = "https://gwfh.mranftl.com/api/fonts";
const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CATALOG_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CATALOG_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_FONT_ZIP_BYTES: usize = 25 * 1024 * 1024;
const MAX_FONT_ZIP_ENTRIES: usize = 4096;
const MAX_FONT_EXTRACTED_BYTES: usize = 16 * 1024 * 1024;
const MAX_FONT_CSS_BYTES: usize = 1024 * 1024;
const MAX_WOFF2_BYTES: usize = 200_000;
const MAX_WOFF2_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const MAX_INSTALLED_FONT_ENTRIES: usize = 4096;
const MAX_PREVIEW_CACHE_ENTRIES: usize = 100_000;

fn validate_font_archive_limits(entry_count: usize, extracted_bytes: usize) -> Result<(), String> {
    if entry_count > MAX_FONT_ZIP_ENTRIES {
        return Err(format!(
            "font zip contains too many entries (max {MAX_FONT_ZIP_ENTRIES})"
        ));
    }
    if extracted_bytes > MAX_FONT_EXTRACTED_BYTES {
        return Err(format!(
            "font zip extracted data exceeds {MAX_FONT_EXTRACTED_BYTES} bytes"
        ));
    }
    Ok(())
}

async fn read_response_body_limited(
    response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > (maximum_bytes + 1) as u64)
    {
        return Err(format!("response body exceeds {} bytes", maximum_bytes));
    }
    let content_length = response.content_length();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::with_capacity(
        content_length
            .unwrap_or(0)
            .min(maximum_bytes as u64) as usize,
    );
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("response body read failed: {e}"))?;
        if bytes.len().saturating_add(chunk.len()) > maximum_bytes {
            return Err(format!("response body exceeds {} bytes", maximum_bytes));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Cached catalog. Initialized on first request; the `OnceLock` plus
/// an `Instant` timestamp gives us lazy refresh without a background
/// task. The size is ~1,955 entries * ~250 bytes = ~500KB so we
/// keep it in memory; for an offline kiosk with no network the
/// picker will just report the catalog fetch error to the user.
struct CachedCatalog {
    entries: Vec<FontCatalogEntry>,
    fetched_at: Instant,
}

static CATALOG_CACHE: OnceLock<std::sync::Mutex<Option<CachedCatalog>>> = OnceLock::new();

fn catalog_cache() -> &'static std::sync::Mutex<Option<CachedCatalog>> {
    CATALOG_CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Synchronous read of the catalog cache. Used by
/// `preview_status` (a sync handler) to look up the display name
/// for each family id without blocking on an .await. Returns
/// an empty Vec if the catalog hasn't been fetched yet -- in
/// that case the next request after the first async fetch will
/// see the real entries.
pub fn catalog_cache_get() -> Vec<FontCatalogEntry> {
    let cache = catalog_cache().lock().expect("catalog cache poisoned");
    match cache.as_ref() {
        Some(c) => c.entries.clone(),
        None => Vec::new(),
    }
}

/// Fetch the upstream catalog, returning a fresh entry list.
async fn fetch_catalog_from_upstream() -> Result<Vec<FontCatalogEntry>, String> {
    let client = reqwest::Client::builder()
        .timeout(CATALOG_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("catalog client init failed: {e}"))?;
    let resp = client
        .get(CATALOG_URL)
        .header("User-Agent", "Screentra Cast/1.0")
        .send()
        .await
        .map_err(|e| format!("catalog request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("catalog returned HTTP {}", resp.status()));
    }
    let body = read_response_body_limited(resp, MAX_CATALOG_BODY_BYTES).await?;
    let raw: Vec<UpstreamFont> = serde_json::from_slice(&body)
        .map_err(|e| format!("catalog JSON decode failed: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|u| FontCatalogEntry {
            id: u.id,
            family: u.family,
            category: u.category,
            variants: u.variants,
            subsets: u.subsets,
            popularity: u.popularity,
            version: u.version,
        })
        .collect())
}

/// Return the cached catalog, refreshing from upstream if stale or
/// never fetched. A network failure here returns the stale cache
/// if we have one (better to show last-known data than nothing);
/// otherwise the error bubbles up to the API handler.
pub async fn get_cached_catalog() -> Result<Vec<FontCatalogEntry>, String> {
    {
        let cache = catalog_cache().lock().expect("catalog cache poisoned");
        if let Some(c) = cache.as_ref() {
            if c.fetched_at.elapsed() < CATALOG_TTL {
                return Ok(c.entries.clone());
            }
        }
    }
    match fetch_catalog_from_upstream().await {
        Ok(entries) => {
            let mut cache = catalog_cache().lock().expect("catalog cache poisoned");
            *cache = Some(CachedCatalog {
                entries: entries.clone(),
                fetched_at: Instant::now(),
            });
            Ok(entries)
        }
        Err(e) => {
            // Stale-but-usable fallback.
            let cache = catalog_cache().lock().expect("catalog cache poisoned");
            if let Some(c) = cache.as_ref() {
                log::warn!("Font catalog fetch failed ({e}); serving stale cache");
                return Ok(c.entries.clone());
            }
            Err(e)
        }
    }
}

/// Get the directory where downloaded font files live.
/// Mirrors the `uploads_dir` helper in `api.rs` for the `fonts`
/// category. We keep this here so the module is self-contained
/// (the existing `uploads_dir` is private to `api.rs`).
pub fn fonts_dir() -> Result<std::path::PathBuf, String> {
    const ALLOWED: &[&str] = &["fonts"];
    // We re-use the same allowlist convention; only "fonts" is valid
    // for this module.
    let _ = ALLOWED;
    let root = std::env::current_dir()
        .map_err(|e| format!("cwd failed: {e}"))?
        .join("uploads");
    let dir = root.join("fonts");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("create fonts dir failed: {e}"));
    }
    // Canonical containment check: the fonts dir must live under
    // the uploads root so a symlink or weird cwd can't escape.
    let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    if !canonical.starts_with(&canonical_root) {
        return Err("fonts dir escaped uploads root".to_string());
    }
    Ok(dir)
}


/// Safe filename: lowercase ASCII family id + dash + weight. The
/// upstream `id` is already safe (lowercase, hyphenated) per
/// Google Fonts conventions but we sanitize defensively.
fn safe_font_basename(family_id: &str, weight: u32) -> String {
    let cleaned: String = family_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("{}-{}", cleaned.to_lowercase(), weight)
}

/// Download a font family from the upstream `gwfh.mranftl.com` zip
/// endpoint, extract the woff2 files, and save them into the
/// fonts directory. Returns the list of files written.
pub async fn download_family(family_id: &str, weights: &[u32]) -> Result<Vec<InstalledFont>, String> {
    if weights.is_empty() {
        return Err("at least one weight is required".to_string());
    }
    // Cap weights to common ones. Google Fonts exposes up to 18
    // variants per family but anything past 4 weights adds no
    // user-visible value on a signage screen.
    if weights.len() > 6 {
        return Err("at most 6 weights per family".to_string());
    }
    // Validate family_id defensively. The upstream API uses
    // hyphenated lowercase ids (e.g. "roboto", "open-sans").
    if family_id.is_empty() || family_id.len() > 64 {
        return Err("invalid family id length".to_string());
    }
    if !family_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid characters in family id".to_string());
    }

    // Build a comma-list of variant slugs. We always pull
    // "regular" + the requested weights so the user gets at
    // least one file even if they ask for exotic weights that
    // don't exist.
    let mut variant_slugs: Vec<String> = vec!["regular".to_string()];
    for w in weights {
        let slug = match w {
            100 => "100",
            200 => "200",
            300 => "300",
            400 => "regular",
            500 => "500",
            600 => "600",
            700 => "700",
            800 => "800",
            900 => "900",
            _ => return Err(format!("unsupported weight {w}")),
        };
        if !variant_slugs.iter().any(|s| s == slug) {
            variant_slugs.push(slug.to_string());
        }
    }
    let url = format!(
        "https://gwfh.mranftl.com/api/fonts/{}?download=zip&subsets=latin&variants={}",
        family_id,
        variant_slugs.join(",")
    );

    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("download client init failed: {e}"))?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Screentra Cast/1.0")
        .send()
        .await
        .map_err(|e| format!("download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download returned HTTP {}", resp.status()));
    }
    let bytes = read_response_body_limited(resp, MAX_FONT_ZIP_BYTES).await?;
    // Sanity cap: 25 MB raw zip. Largest Google Fonts family is
    // ~3MB; anything bigger is probably garbage.
    if bytes.len() > MAX_FONT_ZIP_BYTES {
        return Err("downloaded zip too large".to_string());
    }

    let dir = fonts_dir()?;
    let mut installed = Vec::new();
    let cursor = std::io::Cursor::new(&bytes[..]);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| format!("zip decode failed: {e}"))?;
    let archive_entry_count = archive.len();
    validate_font_archive_limits(archive_entry_count, 0)?;
    let mut extracted_bytes = 0usize;

    for i in 0..archive_entry_count {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i} read failed: {e}"))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        // We only keep woff2 files. The zip contains ttf/woff/eot/svg
        // duplicates and we don't need them; the player only loads
        // woff2 via @font-face.
        if !name.ends_with(".woff2") {
            continue;
        }
        // Parse the weight out of the upstream filename. Format:
        //   roboto-v51-latin-regular.woff2
        //   roboto-v51-latin-700.woff2
        let stem = name.trim_end_matches(".woff2");
        let last_dash = stem.rfind('-').ok_or_else(|| {
            format!("could not parse weight from upstream filename: {name}")
        })?;
        let variant_part = &stem[last_dash + 1..];
        let weight = match variant_to_weight(variant_part) {
            Some(w) => w,
            None => {
                log::warn!("Skipping unparsed font variant: {name}");
                continue;
            }
        };
        let safe = safe_font_basename(family_id, weight);
        let filename = format!("{safe}.woff2");
        let dest = dir.join(&filename);
        // Read entry bytes and write atomically (write-to-temp-then-rename)
        // so a half-written file is never visible to the static server.
        if entry.size() > MAX_WOFF2_ENTRY_BYTES as u64 {
            return Err(format!("font zip entry is too large: {name}"));
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        std::io::Read::take(&mut entry, (MAX_WOFF2_ENTRY_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("zip entry read failed: {e}"))?;
        if buf.len() > MAX_WOFF2_ENTRY_BYTES {
            return Err(format!("font zip entry is too large: {name}"));
        }
        extracted_bytes = extracted_bytes
            .checked_add(buf.len())
            .ok_or_else(|| "font zip extracted data size overflowed".to_string())?;
        validate_font_archive_limits(archive_entry_count, extracted_bytes)?;
        let tmp = dir.join(format!("{filename}.tmp"));
        std::fs::write(&tmp, &buf)
            .map_err(|e| format!("temp write failed: {e}"))?;
        std::fs::rename(&tmp, &dest)
            .map_err(|e| format!("rename to final failed: {e}"))?;
        installed.push(InstalledFont {
            family: derive_display_family(family_id),
            weight,
            url: format!("/uploads/fonts/{filename}"),
            filename,
            size_bytes: buf.len() as u64,
        });
    }
    if installed.is_empty() {
        return Err("zip contained no woff2 files".to_string());
    }
    Ok(installed)
}

/// Turn a hyphenated or underscored family id like "open-sans" or
/// "dm_serif_display" into a display label "Open Sans" / "Dm Serif
/// Display". For ids with a single word like "roboto" the titlecase
/// gives "Roboto". Both `-` and `_` are accepted because Google
/// Fonts uses hyphens while some downstream tools use underscores.
fn derive_display_family(family_id: &str) -> String {
    let mut out = String::new();
    for (i, part) in family_id.split(|c: char| c == '-' || c == '_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// List all installed fonts by reading the fonts directory.
/// Returns one entry per woff2 file; same family + multiple weights
/// produces multiple entries (the player only loads what's
/// referenced; the editor dedupes by family in the UI).
pub fn list_installed() -> Result<Vec<InstalledFont>, String> {
    let dir = fonts_dir()?;
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("read fonts dir failed: {e}"))?;
    let mut scanned_entries = 0usize;
    for e in entries {
        scanned_entries = scanned_entries.saturating_add(1);
        if scanned_entries > MAX_INSTALLED_FONT_ENTRIES {
            return Err("too many installed font entries".to_string());
        }
        let e = match e {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !filename.ends_with(".woff2") {
            continue;
        }
        // Parse "<family>-<weight>.woff2"
        let stem = filename.trim_end_matches(".woff2");
        let last_dash = match stem.rfind('-') {
            Some(i) => i,
            None => continue,
        };
        let family_id = &stem[..last_dash];
        let weight = match stem[last_dash + 1..].parse::<u32>() {
            Ok(w) => w,
            Err(_) => continue,
        };
        let metadata = std::fs::metadata(&path).ok();
        let size = metadata.map(|m| m.len()).unwrap_or(0);
        out.push(InstalledFont {
            family: derive_display_family(family_id),
            weight,
            url: format!("/uploads/fonts/{filename}"),
            filename,
            size_bytes: size,
        });
    }
    out.sort_by(|a, b| a.family.cmp(&b.family).then(a.weight.cmp(&b.weight)));
    Ok(out)
}

/// Delete a single installed font file. `filename` is the bare
/// filename (e.g. "roboto-400.woff2") and must live in the fonts
/// dir; we canonicalize both sides and reject if the result
/// isn't inside the dir.
pub fn delete_font(filename: &str) -> Result<(), String> {
    // Same defensive sanitization as uploads.
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
        || filename.contains('\0')
    {
        return Err("invalid filename".to_string());
    }
    if !filename.ends_with(".woff2") {
        return Err("only .woff2 files can be deleted via this endpoint".to_string());
    }
    let dir = fonts_dir()?;
    let target = dir.join(filename);
    let canonical_target = std::fs::canonicalize(&target)
        .map_err(|_| "font file not found".to_string())?;
    let canonical_dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    if !canonical_target.starts_with(&canonical_dir) {
        return Err("path traversal rejected".to_string());
    }
    std::fs::remove_file(&canonical_target)
        .map_err(|e| format!("delete failed: {e}"))?;
    Ok(())
}

// ============================================================
// Google Fonts preview cache
//
// The font picker shows 1,955 Google Fonts. Without a preview cache
// every preview renders with the browser's default sans-serif
// because the @font-face is only injected for installed fonts.
// That makes the picker useless for browsing: every un-installed
// font's preview looks identical.
//
// The fix is to download ONE weight (400) of every family ahead
// of time into ./uploads/google-fonts-cache/<family>-400.woff2 and
// let the picker inject @font-face for each cached family at
// render time. The download runs as a background task in main.rs
// (see `run_preview_cache_loop`) so the picker is usable as soon
// as the server starts, even with an empty cache.
//
// The woff2 files are served by the existing `nest_service("/uploads")`
// static handler in main.rs, so the picker can reference them as
// /uploads/google-fonts-cache/<family>-400.woff2.
// ============================================================

/// Returns the absolute path to the preview cache directory,
/// creating it if it doesn't exist. Canonical containment check
/// ensures the dir stays under ./uploads/ even if cwd is weird.
pub fn preview_cache_dir() -> Result<std::path::PathBuf, String> {
    let root = std::env::current_dir()
        .map_err(|e| format!("cwd failed: {e}"))?
        .join("uploads");
    let dir = root.join("google-fonts-cache");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("create preview cache dir failed: {e}"));
    }
    let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    if !canonical.starts_with(&canonical_root) {
        return Err("preview cache dir escaped uploads root".to_string());
    }
    Ok(dir)
}

/// Build the path to a single cached preview file. Uses the same
/// `family_id` validation as the install endpoint so a malicious
/// caller can't write outside the cache dir.
pub fn preview_cache_path(family_id: &str, weight: u32) -> Result<std::path::PathBuf, String> {
    if family_id.is_empty() || family_id.len() > 64 {
        return Err("invalid family id length".to_string());
    }
    if !family_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid characters in family id".to_string());
    }
    let dir = preview_cache_dir()?;
    Ok(dir.join(format!("{}-{}.woff2", family_id, weight)))
}

/// The URL the picker references in its @font-face injection. The
/// existing `nest_service("/uploads", ...)` in main.rs serves this
/// directly. Same-origin so it satisfies our strict `font-src 'self'`
/// CSP without relaxing it.
pub fn preview_cache_url(family_id: &str, weight: u32) -> String {
    format!("/uploads/google-fonts-cache/{}-{}.woff2", family_id, weight)
}

/// Returns Some(path) if the preview is already cached, None otherwise.
/// Does NOT trigger a download.
/// Is this family cached (i.e. do we have at least one subset's
/// woff2 on disk for this weight)? Used by the picker loop to
/// decide whether to re-fetch.
///
/// Originally this only checked the legacy `<id>-<weight>.woff2`
/// filename (the latin hardlink), but icon fonts like the Rubik
/// doodle variants have a latin subset >200KB which we can't
/// cache. Falling back to "any subset is cached" lets the picker
/// see those families as "done" once we've downloaded any
/// subset for them.
pub fn preview_cached(family_id: &str, weight: u32) -> Option<std::path::PathBuf> {
    // Fast path: legacy latin file (most families have this).
    if let Ok(p) = preview_cache_path(family_id, weight) {
        if p.exists() {
            return Some(p);
        }
    }
    // Fallback: any subset. Pick the first one we find
    // alphabetically -- stable, no need to sort.
    if let Ok(dir) = preview_cache_dir() {
        let prefix = format!("{}-{}", family_id, weight);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten().take(MAX_PREVIEW_CACHE_ENTRIES) {
                let name = match e.file_name().into_string() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if name.starts_with(&prefix)
                    && name.ends_with(".woff2")
                {
                    return Some(e.path());
                }
            }
        }
    }
    None
}

/// Find the weight of an already-cached preview for the given id
/// by scanning the cache directory. Used by preview_status so
/// the URL it returns matches the file that's actually on disk --
/// the cache loop picks the closest-available weight to 400 per
/// family, so families without a 400 weight (Sunflower, Buda,
/// Molle, etc.) end up at <id>-500.woff2 or similar.
///
/// Returns None if no cached file exists for the id.
pub fn preview_cached_weight(family_id: &str) -> Option<u32> {
    let dir = preview_cache_dir().ok()?;
    let entries = std::fs::read_dir(&dir).ok()?;
    for e in entries.flatten().take(MAX_PREVIEW_CACHE_ENTRIES) {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("woff2") {
            continue;
        }
        let stem = p.file_stem()?.to_str()?;
        // rsplitn(2, '-') yields ["<weight>", "<id>"]; nth(0) is
        // the weight, nth(1) is the id.
        let mut parts = stem.rsplitn(2, '-');
        let weight_str = parts.next()?;
        let id = parts.next()?;
        if id == family_id {
            if let Ok(w) = weight_str.parse::<u32>() {
                return Some(w);
            }
        }
    }
    None
}

/// Download one weight of a family from Google Fonts and save to the
/// preview cache. Returns the URL the picker should reference.
///
/// We hit `https://fonts.googleapis.com/css2?family=<id>:wght@<w>`
/// with a modern browser User-Agent so Google returns woff2 (not
/// the larger TTF it serves to curl/UA-less clients). The CSS body
/// contains one or more @font-face rules; we extract the latin
/// subset's woff2 URL and download that.
///
/// The Google Fonts CSS API requires a real browser UA; using curl
/// defaults makes Google return TTF.
pub async fn ensure_preview_cached(
    family_id: &str,
    style: &str,
    weight: u32,
) -> Result<String, String> {
    let path = preview_cache_path(family_id, weight)?;
    if path.exists() {
        return Ok(preview_cache_url(family_id, weight));
    }
    // Resolve the proper-case family name from the catalog. Google
    // Fonts is case-sensitive: `family=roboto` returns 400 while
    // `family=Roboto` returns 200. The catalog uses lowercase id
    // slugs (e.g. "open-sans") that must be mapped to the display
    // name (e.g. "Open Sans") before hitting Google's CSS API.
    let family_name = match get_cached_catalog().await {
        Ok(entries) => entries
            .iter()
            .find(|e| e.id == family_id)
            .map(|e| e.family.clone())
            .unwrap_or_else(|| family_id.to_string()),
        Err(_) => family_id.to_string(),
    };
    // The Google Fonts CSS2 API takes a URL-encoded family name.
    // Spaces become +, special chars become %XX.
    let url = format!(
        "https://fonts.googleapis.com/css2?family={}{}&display=swap",
        urlencode_google_family(&family_name),
        preview_axis_suffix(&style, weight)
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36              (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    let css_resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("css fetch failed: {e}"))?;
    if !css_resp.status().is_success() {
        return Err(format!(
            "Google Fonts CSS returned HTTP {} for {}",
            css_resp.status(),
            family_id
        ));
    }
    let css = String::from_utf8(read_response_body_limited(css_resp, MAX_FONT_CSS_BYTES).await?)
        .map_err(|e| format!("css read failed: {e}"))?;
    // Extract the latin (U+0000-00FF) subset's woff2 URL. If
    // multiple subsets are present we pick the latin one because
    // the preview text ("The quick brown fox 123") only uses
    // basic Latin glyphs. Fall back to the first woff2 URL we
    // find if the latin subset isn't there.
    let woff2_url = extract_latin_woff2(&css)
        .or_else(|| extract_first_woff2(&css))
        .ok_or_else(|| {
            format!(
                "no woff2 url found in Google Fonts CSS for {}",
                family_id
            )
        })?;
    let woff2_resp = client
        .get(&woff2_url)
        .send()
        .await
        .map_err(|e| format!("woff2 fetch failed: {e}"))?;
    if !woff2_resp.status().is_success() {
        return Err(format!(
            "woff2 download returned HTTP {} for {}",
            woff2_resp.status(),
            family_id
        ));
    }
    let bytes = read_response_body_limited(woff2_resp, MAX_WOFF2_BYTES).await?;
    if bytes.len() < 100 {
        return Err(format!(
            "woff2 for {} is suspiciously small ({} bytes)",
            family_id,
            bytes.len()
        ));
    }
    if bytes.len() > 200_000 {
        return Err(format!(
            "woff2 for {} is too large ({} bytes)",
            family_id,
            bytes.len()
        ));
    }
    // Write atomically: write to .tmp, then rename. Prevents a
    // partial file from being served if the process dies mid-write.
    let tmp = path.with_extension("woff2.tmp");
    std::fs::write(&tmp, &bytes)
        .map_err(|e| format!("woff2 write failed: {e}"))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("woff2 rename failed: {e}"))?;
    Ok(preview_cache_url(family_id, weight))
}

/// Build the on-disk path for a specific subset's woff2.
/// <preview_cache_dir>/<id>-<weight>-<subset>.woff2
pub fn preview_subset_path(
    family_id: &str,
    weight: u32,
    subset: &str,
) -> Result<std::path::PathBuf, String> {
    if family_id.is_empty() || family_id.len() > 64 {
        return Err("invalid family id length".to_string());
    }
    if !family_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid characters in family id".to_string());
    }
    if subset.is_empty() || subset.len() > 32 {
        return Err("invalid subset name length".to_string());
    }
    if !subset
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("invalid characters in subset name".to_string());
    }
    let dir = preview_cache_dir()?;
    Ok(dir.join(format!("{}-{}-{}.woff2", family_id, weight, subset)))
}

/// Build the URL the picker / runtime should use to fetch a
/// specific subset's woff2 from the local server.
/// /uploads/google-fonts-cache/<id>-<weight>-<subset>.woff2
pub fn preview_subset_url(
    family_id: &str,
    weight: u32,
    subset: &str,
) -> String {
    format!(
        "/uploads/google-fonts-cache/{}-{}-{}.woff2",
        family_id, weight, subset
    )
}

/// List all the subset woff2 files we have on disk for a given
/// family + weight. Used by the runtime @font-face injector to
/// know which subsets are available locally.
pub fn list_cached_subsets(
    family_id: &str,
    weight: u32,
) -> Vec<String> {
    let dir = match preview_cache_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let prefix = format!("{}-{}", family_id, weight);
    let _ = prefix; // prefix not used (we use rsplitn below)
    let mut out: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten().take(MAX_PREVIEW_CACHE_ENTRIES) {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("woff2") {
                continue;
            }
            let stem = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            // stem is "<id>-<weight>-<subset>". rsplitn(3, '-')
            // yields at most 3 elements right-to-left:
            // ["<subset>", "<weight>", "<id>"]. The first
            // element is the subset (when the id and weight
            // both parse correctly).
            let mut parts = stem.rsplitn(3, '-');
            let subset = match (parts.next(), parts.next(), parts.next()) {
                (Some(subset), Some(w_str), Some(id))
                    if id == family_id
                        && w_str.parse::<u32>().ok() == Some(weight) =>
                {
                    subset.to_string()
                }
                _ => continue,
            };
            out.push(subset);
        }
    }
    out.sort();
    out
}

/// Check if a specific subset is on disk.
pub fn preview_subset_cached(
    family_id: &str,
    weight: u32,
    subset: &str,
) -> Option<std::path::PathBuf> {
    preview_subset_path(family_id, weight, subset)
        .ok()
        .filter(|p| p.exists())
}

/// Mark a family as "permanently failed" so the cache loop
/// doesn't re-attempt to fetch it every 60 seconds. We write
/// a tiny `.failed` marker file with a human-readable reason
/// inside the preview cache directory.
fn write_failure_marker(family_id: &str, reason: &str) {
    if let Ok(dir) = preview_cache_dir() {
        let p = dir.join(format!("{}.failed", family_id));
        let _ = std::fs::write(&p, reason.as_bytes());
    }
}

/// Has this family been marked as permanently failed?
pub fn preview_cache_failed(family_id: &str) -> bool {
    if let Ok(dir) = preview_cache_dir() {
        let p = dir.join(format!("{}.failed", family_id));
        return p.exists();
    }
    false
}

/// Clear the failure marker (used when a previously-failed
/// family is re-attempted after a catalog refresh).
pub fn clear_failure_marker(family_id: &str) {
    if let Ok(dir) = preview_cache_dir() {
        let p = dir.join(format!("{}.failed", family_id));
        let _ = std::fs::remove_file(&p);
    }
}

/// Cache every subset Google Fonts ships for one family + weight.
/// Writes each subset to <id>-<weight>-<subset>.woff2 and ALSO
/// hardlinks the latin file to <id>-<weight>.woff2 so the
/// existing picker preview path (which looks for the no-suffix
/// filename) keeps working.
///
/// Returns the number of subsets actually downloaded. Returns 0
/// if everything is already on disk. Returns an Err if the
/// initial CSS fetch failed; partial subsets already on disk
/// are preserved (this function is idempotent -- safe to retry).
pub async fn ensure_full_preview_cached(
    family_id: &str,
    style: &str,
    weight: u32,
) -> Result<usize, String> {
    // If we already have the latin file, assume the full set is
    // in place. (The loop will re-fetch if a subset is missing
    // for a different reason.)
    if preview_cached(family_id, weight).is_some()
        && preview_subset_cached(family_id, weight, "latin").is_some()
    {
        return Ok(0);
    }
    // Resolve the proper-case family name from the catalog.
    let family_name = match get_cached_catalog().await {
        Ok(entries) => entries
            .iter()
            .find(|e| e.id == family_id)
            .map(|e| e.family.clone())
            .unwrap_or_else(|| family_id.to_string()),
        Err(_) => family_id.to_string(),
    };
    let url = format!(
        "https://fonts.googleapis.com/css2?family={}{}&display=swap",
        urlencode_google_family(&family_name),
        preview_axis_suffix(&style, weight)
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    let css_resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("css fetch failed: {e}"))?;
    if !css_resp.status().is_success() {
        return Err(format!(
            "Google Fonts CSS returned HTTP {} for {}",
            css_resp.status(),
            family_id
        ));
    }
    let css = String::from_utf8(read_response_body_limited(css_resp, MAX_FONT_CSS_BYTES).await?)
        .map_err(|e| format!("css read failed: {e}"))?;
    let subsets = extract_all_subsets(&css);
    if subsets.is_empty() {
        // Mark this family as permanently failed (no @font-face
        // blocks in the Google response -- usually icon fonts
        // that ship their glyphs through a different
        // mechanism, or families Google has removed). The
        // cache loop checks this marker so we don't re-fetch
        // the same broken family every 60 seconds.
        write_failure_marker(family_id, "no @font-face blocks");
        return Err(format!(
            "no @font-face blocks found in Google CSS for {}",
            family_id
        ));
    }
    let mut downloaded = 0usize;
    for (subset, woff2_url) in &subsets {
        let path = preview_subset_path(family_id, weight, subset)?;
        if path.exists() {
            downloaded += 1;
            continue;
        }
        // Skip subsets whose woff2 is implausibly large (icon
        // fonts like Material Symbols can be 300+ KB). We
        // keep them in the catalog but don't cache the bytes.
        let woff2_resp = match client.get(woff2_url).send().await {
            Ok(r) => r,
            Err(e) => {
                log::debug!(
                    "preview cache: {} {} fetch failed: {}",
                    family_id, subset, e
                );
                continue;
            }
        };
        if !woff2_resp.status().is_success() {
            log::debug!(
                "preview cache: {} {} returned HTTP {}",
                family_id, subset, woff2_resp.status()
            );
            continue;
        }
        let bytes = match read_response_body_limited(woff2_resp, MAX_WOFF2_BYTES).await {
            Ok(b) => b,
            Err(e) => {
                log::debug!(
                    "preview cache: {} {} read failed: {}",
                    family_id, subset, e
                );
                continue;
            }
        };
        if bytes.len() < 100 {
            log::debug!(
                "preview cache: {} {} suspiciously small ({} bytes)",
                family_id, subset, bytes.len()
            );
            continue;
        }
        if bytes.len() > MAX_WOFF2_BYTES {
            log::debug!(
                "preview cache: {} {} too large ({} bytes)",
                family_id, subset, bytes.len()
            );
            continue;
        }
        // Write atomically: tmp then rename.
        let tmp = path.with_extension("woff2.tmp");
        if let Err(e) = std::fs::write(&tmp, &bytes) {
            log::debug!(
                "preview cache: {} {} write failed: {}",
                family_id, subset, e
            );
            continue;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            log::debug!(
                "preview cache: {} {} rename failed: {}",
                family_id, subset, e
            );
            continue;
        }
        downloaded += 1;
    }
    // Hardlink the latin file to the legacy <id>-<weight>.woff2
    // name so the picker's existing preview_status path
    // (which looks for that filename) keeps working.
    if let Ok(latin_path) = preview_subset_path(family_id, weight, "latin") {
        if latin_path.exists() {
            let legacy = match preview_cache_path(family_id, weight) {
                Ok(p) => p,
                Err(_) => return Ok(downloaded),
            };
            if !legacy.exists() {
                // Best effort -- the hardlink is an optimization
                // for the picker, not a correctness requirement.
                let _ = std::fs::hard_link(&latin_path, &legacy);
            }
        }
    }
    Ok(downloaded)
}

/// Parse a Google Fonts variant string to a numeric weight.
///
/// "regular" / "italic" / "" map to 400 (Google Fonts treats them
/// as the same weight as "400"). "100italic" / "100i" -> 100. Bare
/// numbers like "700" pass through. Unknown non-numeric strings
/// return None.
fn variant_to_weight(v: &str) -> Option<u32> {
    let v0 = v.trim().trim_end_matches("italic").trim_end_matches('i');
    match v0 {
        "thin" => Some(100),
        "extralight" => Some(200),
        "light" => Some(300),
        "regular" | "normal" | "" => Some(400),
        "medium" => Some(500),
        "semibold" => Some(600),
        "bold" => Some(700),
        "extrabold" => Some(800),
        "black" => Some(900),
        _ => {
            // Bare numeric weight like "700" or "100" parses
            // straight through. Anything else that doesn't look
            // like a number is rejected.
            v0.parse::<u32>().ok()
        }
    }
}

/// Pick the (style, weight) for a preview, preferring a non-italic
/// weight closest to 400. Falls back to italic 400 if the family
/// is italic-only (e.g. Molle). Returns ("normal", 400) if the
/// variant list is empty / unparseable.
///
/// The Google Fonts CSS2 API axis syntax is:
///   family=<name>:wght@<weight>            -- normal style
///   family=<name>:ital,wght@<style>,<weight> -- italic style
/// (or, equivalently, both axes together when both are needed).
///
/// We expose two accessors: `pick_preview_weight` for back-compat
/// (just the numeric weight, used by the Installed tab tests) and
/// `pick_preview_style_and_weight` for the cache loop. The cache
/// loop's italic handling lives here so the picker URL syntax
/// matches what Google returns.
fn pick_preview_weight(variants: &[String]) -> u32 {
    let (_style, weight) = pick_preview_style_and_weight(variants);
    weight
}

fn pick_preview_style_and_weight(variants: &[String]) -> (String, u32) {
    let target: u32 = 400;
    // Parse each variant into (is_italic, weight).
    let mut pairs: Vec<(bool, u32)> = Vec::new();
    for v in variants {
        let lower = v.to_lowercase();
        let is_italic = lower.ends_with("italic") || lower.ends_with('i');
        let base = lower.trim_end_matches("italic").trim_end_matches('i');
        let weight_opt = match base {
            "thin" => Some(100),
            "extralight" => Some(200),
            "light" => Some(300),
            "regular" | "normal" | "" => Some(400),
            "medium" => Some(500),
            "semibold" => Some(600),
            "bold" => Some(700),
            "extrabold" => Some(800),
            "black" => Some(900),
            _ => base.parse::<u32>().ok(),
        };
        if let Some(w) = weight_opt {
            pairs.push((is_italic, w));
        }
    }
    if pairs.is_empty() {
        return ("normal".to_string(), 400);
    }
    // Prefer non-italic first (matches what the picker's @font-face
    // requests -- no font-style is set, so normal applies). Within
    // each style bucket, pick the weight closest to 400; ties
    // (e.g. 300 and 500 are both 100 away from 400) go to the
    // higher weight because 400 is "regular" body weight and 500
    // reads as more readable than 300 at preview sizes.
    pairs.sort_by(|a, b| {
        let (ai, aw) = a;
        let (bi, bw) = b;
        // false (normal) before true (italic).
        ai.cmp(bi)
            // smaller diff is closer to 400.
            .then(aw.abs_diff(target).cmp(&bw.abs_diff(target)))
            // ties: heavier weight wins.
            .then(b.cmp(a))
    });
    let (is_italic, weight) = pairs[0];
    (if is_italic { "italic" } else { "normal" }.to_string(), weight)
}

/// Build the Google Fonts CSS API axis string for the picker
/// preview. Returns `:wght@<weight>` for normal, or
/// `:ital,wght@1,<weight>` for italic. Used by ensure_preview_cached.
fn preview_axis_suffix(style: &str, weight: u32) -> String {
    match style {
        "italic" => format!(":ital,wght@1,{}", weight),
        _ => format!(":wght@{}", weight),
    }
}

#[cfg(test)]
mod pick_preview_weight_tests {
    use super::*;

    fn vs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn picks_400_when_available() {
        // Has 400 directly.
        assert_eq!(pick_preview_weight(&vs(&["regular", "500", "700"])), 400);
    }

    #[test]
    fn picks_closest_to_400_when_no_400() {
        // Sunflower has only 300/500/700 -> closest to 400 is 500.
        assert_eq!(pick_preview_weight(&vs(&["300", "500", "700"])), 500);
    }

    #[test]
    fn picks_400_from_italics() {
        // Many families list "400italic" but no plain 400.
        // We strip italic and pick 400 (closest match = 0 diff).
        assert_eq!(pick_preview_weight(&vs(&["regularitalic", "500italic"])), 400);
    }

    #[test]
    fn falls_back_to_400_for_empty_input() {
        assert_eq!(pick_preview_weight(&vs(&[])), 400);
    }

    #[test]
    fn falls_back_to_400_for_garbage_input() {
        // Unparseable strings get skipped, then we fall back to 400.
        assert_eq!(pick_preview_weight(&vs(&["??", "##"])), 400);
    }

    #[test]
    fn handles_real_google_variants() {
        // Real catalog entries from gwfh.mranftl.com:
        // Sunflower: ["300", "500", "700"] -> 500
        // Roboto:    ["100", "200", "300", "regular", "500", ...] -> 400
        // Exile:     ["regular"] -> 400
        assert_eq!(pick_preview_weight(&vs(&["300", "500", "700"])), 500);
        assert_eq!(pick_preview_weight(&vs(&["100", "200", "300", "regular", "500", "700"])), 400);
        assert_eq!(pick_preview_weight(&vs(&["regular"])), 400);
    }

    // pick_preview_style_and_weight: italic handling.

    #[test]
    fn style_and_weight_picks_normal_400_when_both_available() {
        // Roboto-style: has regular and italic; normal 400 is the
        // first match in the sort.
        let (style, weight) = pick_preview_style_and_weight(&vs(&[
            "100", "100italic", "300", "regular", "italic", "700",
        ]));
        assert_eq!(style, "normal");
        assert_eq!(weight, 400);
    }

    #[test]
    fn style_and_weight_falls_back_to_italic_for_italic_only_family() {
        // Molle: only "italic" in its variant list. Google Fonts
        // requires the ital axis in the URL.
        let (style, weight) = pick_preview_style_and_weight(&vs(&["italic"]));
        assert_eq!(style, "italic");
        assert_eq!(weight, 400);
    }

    #[test]
    fn style_and_weight_keeps_italic_when_no_normal_variant() {
        // Family only has italic at multiple weights; pick the
        // closest italic weight to 400.
        let (style, weight) = pick_preview_style_and_weight(&vs(&[
            "300italic", "500italic", "700italic",
        ]));
        assert_eq!(style, "italic");
        assert_eq!(weight, 500);
    }

    #[test]
    fn preview_axis_suffix_uses_ital_for_italic() {
        assert_eq!(preview_axis_suffix("italic", 400), ":ital,wght@1,400");
        assert_eq!(preview_axis_suffix("normal", 400), ":wght@400");
        assert_eq!(preview_axis_suffix("", 400), ":wght@400");
        assert_eq!(preview_axis_suffix("anything-else", 700), ":wght@700");
    }
}

#[cfg(test)]
mod extract_all_subsets_tests {
    use super::*;

    // Sample CSS roughly matching what Google Fonts returns.
    // The real response is longer and uses more subsets, but
    // the structure is identical.
    const SAMPLE_CSS: &str = r#"
/* cyrillic-ext */
@font-face {
  font-family: 'Roboto';
  font-style: normal;
  font-weight: 400;
  src: url(https://fonts.gstatic.com/s/roboto/v32/cyrillic-ext.woff2) format('woff2');
  unicode-range: U+0460-052F, U+1C80-1C8A;
}
/* cyrillic */
@font-face {
  font-family: 'Roboto';
  font-style: normal;
  font-weight: 400;
  src: url(https://fonts.gstatic.com/s/roboto/v32/cyrillic.woff2) format('woff2');
  unicode-range: U+0301, U+0400-045F;
}
/* latin */
@font-face {
  font-family: 'Roboto';
  font-style: normal;
  font-weight: 400;
  src: url(https://fonts.gstatic.com/s/roboto/v32/latin.woff2) format('woff2');
  unicode-range: U+0000-00FF, U+0131;
}
"#;

    #[test]
    fn finds_all_subsets_in_google_response() {
        let subsets = extract_all_subsets(SAMPLE_CSS);
        let names: Vec<&str> = subsets.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["cyrillic-ext", "cyrillic", "latin"]);
        // The URLs should be the ones in the CSS.
        let urls: Vec<&str> = subsets.iter().map(|(_, u)| u.as_str()).collect();
        assert!(urls[0].ends_with("cyrillic-ext.woff2"));
        assert!(urls[1].ends_with("cyrillic.woff2"));
        assert!(urls[2].ends_with("latin.woff2"));
    }

    #[test]
    fn handles_single_subset_response() {
        let css = r#"
/* latin */
@font-face {
  font-family: 'Exile';
  src: url(https://fonts.gstatic.com/s/exile/v1/latin.woff2) format('woff2');
  unicode-range: U+0000-00FF;
}
"#;
        let subsets = extract_all_subsets(css);
        assert_eq!(subsets.len(), 1);
        assert_eq!(subsets[0].0, "latin");
        assert!(subsets[0].1.ends_with("latin.woff2"));
    }

    #[test]
    fn returns_empty_for_empty_css() {
        let subsets = extract_all_subsets("");
        assert_eq!(subsets.len(), 0);
    }

    #[test]
    fn skips_comments_without_font_face() {
        // A comment that's just a description (followed by
        // another comment, not @font-face) should be skipped.
        let css = r#"
/* Not a subset, just a description */
/* latin */
@font-face {
  font-family: 'Foo';
  src: url(https://fonts.gstatic.com/s/foo/latin.woff2) format('woff2');
  unicode-range: U+0000-00FF;
}
"#;
        let subsets = extract_all_subsets(css);
        assert_eq!(subsets.len(), 1);
        assert_eq!(subsets[0].0, "latin");
    }

    #[test]
    fn strips_quotes_around_url() {
        let css = r#"
/* latin */
@font-face {
  src: url('https://fonts.gstatic.com/s/foo/latin.woff2') format('woff2');
  unicode-range: U+0000-00FF;
}
"#;
        let subsets = extract_all_subsets(css);
        assert_eq!(subsets[0].1, "https://fonts.gstatic.com/s/foo/latin.woff2");
    }
}

#[cfg(test)]
mod sanitize_subset_name_tests {
    use super::*;

    #[test]
    fn converts_latin_ext() {
        assert_eq!(sanitize_subset_name("Latin Ext"), "latin-ext");
    }

    #[test]
    fn converts_simple_latin() {
        assert_eq!(sanitize_subset_name("latin"), "latin");
    }

    #[test]
    fn converts_cyrillic() {
        assert_eq!(sanitize_subset_name("Cyrillic"), "cyrillic");
    }

    #[test]
    fn handles_extra_whitespace() {
        assert_eq!(sanitize_subset_name("  Vietnamese  "), "vietnamese");
    }

    #[test]
    fn drops_punctuation() {
        assert_eq!(sanitize_subset_name("[100]"), "100");
    }

    #[test]
    fn empty_for_empty_input() {
        assert_eq!(sanitize_subset_name(""), "");
        assert_eq!(sanitize_subset_name("   "), "");
    }
}

#[cfg(test)]
mod preview_subset_path_tests {
    use super::*;

    #[test]
    fn builds_subset_path() {
        // preview_subset_path needs preview_cache_dir which
        // requires cwd. We can't easily test the absolute
        // path here, so just verify the validation logic
        // and the URL builder.
        assert!(preview_subset_path("", 400, "latin").is_err());
        assert!(preview_subset_path("roboto", 400, "").is_err());
        assert!(preview_subset_path("roboto", 400, "../etc/passwd").is_err());
    }

    #[test]
    fn url_format_matches_filename() {
        let url = preview_subset_url("roboto", 400, "latin");
        assert_eq!(url, "/uploads/google-fonts-cache/roboto-400-latin.woff2");
    }
}

/// Pull the woff2 URL out of the @font-face block whose
/// `unicode-range` covers U+0000-00FF (basic Latin). The CSS
/// Google serves has one @font-face per subset; latin is what
/// the preview text uses.
fn extract_latin_woff2(css: &str) -> Option<String> {
    // Split into @font-face blocks.
    for block in css.split("@font-face") {
        if !block.contains("U+0000-00FF") {
            continue;
        }
        if let Some(url) = extract_first_woff2(block) {
            return Some(url);
        }
    }
    None
}

/// Find the first `url(https://...woff2)` in a CSS block.
fn extract_first_woff2(css: &str) -> Option<String> {
    let start = css.find("url(")?;
    let after = &css[start + 4..];
    let end = after.find(')')?;
    let raw = after[..end].trim();
    // Strip surrounding quotes; Google wraps the URL in url(...).
    Some(raw.trim_matches('\'').trim_matches('"').to_string())
}

/// Parse the Google Fonts CSS response into one entry per subset.
///
/// The CSS has the shape:
///   /* latin-ext */
///   @font-face { ... src: url(https://fonts.gstatic.com/.../latin-ext.woff2) ... }
///   /* latin */
///   @font-face { ... src: url(https://fonts.gstatic.com/.../latin.woff2) ... }
///
/// We split on `/* ... */` comments to recover the subset name,
/// then look for the `src: url(...)` within the next @font-face
/// block. Returns Vec<(subset_name, url)> in source order (which
/// is also the order Google recommends browsers try them in).
///
/// Subset names are sanitized to lowercase ASCII + hyphens so
/// they can be used as filename components. "Latin Ext" -> "latin-ext".
fn extract_all_subsets(css: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // Walk through the CSS finding /* ... */ comments. For each
    // comment, find the next @font-face block (or the next
    // comment, whichever comes first) and pull the URL out of
    // its src: url(...) declaration.
    let mut cursor = 0usize;
    while cursor < css.len() {
        let Some(rel) = css[cursor..].find("/*") else { break };
        let abs_comment = cursor + rel;
        let Some(rel_end) = css[abs_comment + 2..].find("*/") else { break };
        let abs_end = abs_comment + 2 + rel_end;
        let raw_name = css[abs_comment + 2..abs_end].trim();
        let subset = sanitize_subset_name(raw_name);
        if subset.is_empty() {
            cursor = abs_end + 2;
            continue;
        }
        // Look for the next @font-face after this comment. If we
        // hit another comment first, this comment was a
        // description rather than a subset marker, so skip.
        let after_comment = abs_end + 2;
        let next_block = css[after_comment..]
            .find("@font-face")
            .map(|p| after_comment + p);
        let next_comment = css[after_comment..]
            .find("/*")
            .map(|p| after_comment + p);
        let Some(block_start) = next_block else {
            cursor = after_comment;
            continue;
        };
        // If there's a closer comment than @font-face, the comment
        // is just a description. Skip past this comment (after
        // the closing */) and continue looking.
        if let Some(c) = next_comment {
            if c < block_start {
                cursor = abs_end + 2;
                continue;
            }
        }
        // Find the matching close of this @font-face block. The
        // CSS uses balanced braces; the URL we want is the
        // `src: url(...)` inside this block.
        let Some(open) = css[block_start..].find('{').map(|p| block_start + p) else {
            cursor = after_comment;
            continue;
        };
        let Some(close) = css[open..].find('}').map(|p| open + p) else {
            cursor = after_comment;
            continue;
        };
        let block = &css[open..=close];
        if let Some(url) = extract_first_woff2(block) {
            out.push((subset, url));
        }
        cursor = close + 1;
    }
    out
}

/// Sanitize a Google Fonts subset comment to a filename-safe
/// token. "Latin Ext" -> "latin-ext", "Cyrillic Ext" ->
/// "cyrillic-ext", "Vietnamese" -> "vietnamese", etc.
fn sanitize_subset_name(raw: &str) -> String {
    let lower = raw.trim().to_lowercase();
    // Replace whitespace with hyphens, then collapse multiple
    // hyphens, then trim leading/trailing hyphens.
    let mut s = String::with_capacity(lower.len());
    let mut last_was_dash = true;
    for c in lower.chars() {
        if c.is_whitespace() || c == '-' {
            if !last_was_dash {
                s.push('-');
                last_was_dash = true;
            }
        } else if c.is_ascii_alphanumeric() {
            s.push(c);
            last_was_dash = false;
        }
        // Skip anything else (punctuation etc).
    }
    s.trim_matches('-').to_string()
}

/// URL-encode a Google Fonts family name for the css2 API.
/// Spaces become + (Google's css2 style), and special chars
/// become %XX. We do this by hand instead of pulling in a
/// url-encoding crate because the rules are simple and the
/// set of chars in font family names is small.
fn urlencode_google_family(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if c == ' ' {
            // Google Fonts css2 uses + for spaces in the family
            // segment (e.g. `family=Open+Sans:wght@400`).
            out.push('+');
        } else {
            // Anything else (emoji, non-ASCII letters, etc.)
            // gets percent-encoded. c.is_ascii_alphanumeric
            // already returned false so we know c is at least
            // 2 bytes of UTF-8.
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            for b in bytes {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod urlencode_tests {
    use super::*;
    #[test]
    fn urlencodes_basic() {
        assert_eq!(urlencode_google_family("Roboto"), "Roboto");
        assert_eq!(urlencode_google_family("Open Sans"), "Open+Sans");
        assert_eq!(urlencode_google_family("DM Serif Display"), "DM+Serif+Display");
        assert_eq!(urlencode_google_family("Noto Sans JP"), "Noto+Sans+JP");
    }
    #[test]
    fn urlencodes_special_chars() {
        // ampersand, slash, etc.
        assert_eq!(urlencode_google_family("Foo&Bar"), "Foo%26Bar");
    }
}

/// Build the URL map the picker needs to inject @font-face for
/// only the families that are already cached. Uncached families
/// are simply omitted from the response - the picker will fall
/// back to sans-serif for those (same as today's behavior).
///
/// The input `families` are the user-facing display names
/// ("Roboto", "Open Sans", etc.) that the picker shows in card
/// headers. The disk file lives at
/// `./uploads/google-fonts-cache/<id>-<weight>.woff2` where `id`
/// is the lowercase slug. We accept either form on input (so a
/// stale picker or an admin script can call us with whatever
/// naming convention they have) and we always return the
/// display name in the response key, so the picker can match
/// against its `item.family` catalog field without further
/// transformations.
pub fn preview_status(families: &[String]) -> Vec<(String, String)> {
    // The disk cache is keyed by family id (lowercase slug). The
    // picker thinks in display names ("Roboto"). We need a map
    // id -> display_name so we can convert. The catalog has
    // ~1,955 entries; building a HashMap on every request is
    // cheap (~50us) and avoids needing a global lock or
    // synchronization.
    let catalog = crate::fonts::catalog_cache_get();
    let id_to_display: std::collections::HashMap<&str, &str> = catalog
        .iter()
        .map(|e| (e.id.as_str(), e.family.as_str()))
        .collect();
    // The disk filenames map: <id>-400.woff2. Build a map from
    // id (lowercased) -> is_cached.
    let dir = match preview_cache_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let cached_ids: std::collections::HashSet<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .take(MAX_PREVIEW_CACHE_ENTRIES)
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("woff2") {
                        return None;
                    }
                    let stem = p.file_stem()?.to_str()?;
                    // stem = "<id>-400" (or "<id>-..."). The
                    // family id never ends in a number, so the
                    // last "-" in the stem always separates the
                    // id from the weight.
                    //
                    // rsplitn(2, '-') yields at most two chunks
                    // right-to-left: ["<weight>", "<id>"]. We
                    // want the second element (nth(1)) which is
                    // the id.
                    let id = stem.rsplitn(2, '-').nth(1)?;
                    Some(id.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::with_capacity(families.len());
    for f in families {
        let lookup = f.to_lowercase();
        // The picker might have sent either the id ("roboto")
        // or the display name ("Roboto" / "Open Sans"). Try both.
        //
        // CRITICAL: the second branch (catalog lookup) MUST
        // verify the resolved id is actually in `cached_ids`.
        // Without that check we'd return a URL for any
        // catalog family whose file failed to download (size
        // limit exceeded, network error, etc.) and the
        // browser would log 404s for every Material Symbols
        // card. The catalog has hundreds of families whose
        // woff2 is too big for the cache; we don't want to
        // promise them to the picker.
        let id = if cached_ids.contains(&lookup) {
            lookup.clone()
        } else {
            // Look up the id by matching display name, then
            // verify the resolved id is actually on disk.
            // Without that check we'd return a URL for any
            // catalog family whose file failed to download
            // (size limit exceeded, network error, etc.) and
            // the browser would log 404s for every Material
            // Symbols card.
            let mut found = None;
            for (id, family) in id_to_display.iter() {
                if family.to_lowercase() == lookup
                    && cached_ids.contains(*id)
                {
                    found = Some(id.to_string());
                    break;
                }
            }
            match found {
                Some(id) => id,
                None => continue,
            }
        };
        // Return the display name as the key so the picker can
        // match it against `item.family` directly. The URL is
        // keyed by the id (lowercase slug) because that's the
        // filename on disk.
        //
        // We use preview_cached_weight(id) -- not a hardcoded
        // 400 -- because some families (Sunflower, Buda, Molle,
        // UnifrakturCook, etc.) only ship 300/500/700 weights,
        // so the cache loop picked the closest available weight
        // to 400 instead. Hardcoding 400 here would point the
        // picker at a non-existent file and 404.
        let display = id_to_display
            .get(id.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| id.clone());
        let weight = preview_cached_weight(&id).unwrap_or(400);
        out.push((display, preview_cache_url(&id, weight)));
    }
    out
}


/// Count cached previews vs catalog size for the progress indicator.
/// Returns (cached, on_disk_bytes).
pub fn preview_cache_stats() -> (usize, u64) {
    let dir = match preview_cache_dir() {
        Ok(d) => d,
        Err(_) => return (0, 0),
    };
    let mut count = 0usize;
    let mut total_bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten().take(MAX_PREVIEW_CACHE_ENTRIES) {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("woff2") {
                count += 1;
                if let Ok(meta) = entry.metadata() {
                    total_bytes += meta.len();
                }
            }
        }
    }
    (count, total_bytes)
}


/// Background loop that pre-caches one weight (400) of every Google
/// Font family for picker previews. Runs every 60s, but only
/// re-tries families that aren't already cached. The first run
/// downloads all ~1,955 families; subsequent runs are no-ops.
///
/// Rate-limited to ~10 families/second to be polite to Google. At
/// that rate the full catalog takes ~3 minutes; we don't want to
/// slow it down further because the picker is useless while the
/// cache is empty. The 60s outer loop catches up after a restart
/// if we crashed mid-cache.
pub async fn run_preview_cache_loop() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        // Fetch the catalog. Cached internally for 24h so this is
        // a single map lookup after the first call.
        let catalog = match get_cached_catalog().await {
            Ok(c) => c,
            Err(e) => {
                log::warn!("preview cache: catalog fetch failed: {}", e);
                continue;
            }
        };
        // Find families that aren't cached yet AND haven't been
        // marked as permanently failed. For each entry, pick the
        // (style, weight) closest to (normal, 400) from the
        // variant list -- some fonts only ship 300/500/700,
        // some are italic-only, and Google returns HTTP 400 if
        // we ask for an unavailable combination.
        let mut to_fetch: Vec<(String, String, u32)> = Vec::new();
        for entry in &catalog {
            // Skip families we've already given up on (icon
            // fonts Google won't serve woff2 for, or families
            // that 400 every time we ask). The .failed marker
            // is written by ensure_full_preview_cached when it
            // returns Err -- so the loop doesn't hammer them
            // every 60 seconds.
            if preview_cache_failed(&entry.id) {
                continue;
            }
            let (style, weight) = pick_preview_style_and_weight(&entry.variants);
            if preview_cached(&entry.id, weight).is_none() {
                to_fetch.push((entry.id.clone(), style, weight));
            }
        }
        if to_fetch.is_empty() {
            let (count, bytes) = preview_cache_stats();
            log::debug!(
                "preview cache: {} families cached ({} KB), nothing to do",
                count,
                bytes / 1024
            );
            continue;
        }
        log::info!(
            "preview cache: {} of {} families need caching, starting batch",
            to_fetch.len(),
            catalog.len()
        );
        let mut cached = 0usize;
        let mut errors = 0usize;
        let total = to_fetch.len();
        for (family_id, style, weight) in to_fetch {
            // Skip if a concurrent process beat us to it.
            if preview_cached(&family_id, weight).is_some() {
                continue;
            }
            match ensure_full_preview_cached(&family_id, &style, weight).await {
                Ok(n_subsets) => {
                    if n_subsets > 0 {
                        cached += 1;
                    }
                }
                Err(e) => {
                    errors += 1;
                    // Only log at debug for individual failures; the
                    // log spam from 1,955 retries would be enormous.
                    log::debug!("preview cache: {} failed: {}", family_id, e);
                }
            }
            // 100ms between requests = ~10 req/s. Well under any
            // reasonable Google Fonts rate limit.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        log::info!(
            "preview cache: pass complete cached={} errors={} total={}",
            cached,
            errors,
            total
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_archive_limits_reject_entry_and_output_amplification() {
        assert!(validate_font_archive_limits(MAX_FONT_ZIP_ENTRIES, MAX_FONT_EXTRACTED_BYTES).is_ok());
        assert!(validate_font_archive_limits(MAX_FONT_ZIP_ENTRIES + 1, 0).is_err());
        assert!(validate_font_archive_limits(0, MAX_FONT_EXTRACTED_BYTES + 1).is_err());
    }
    #[test]
    fn variant_to_weight_known_values() {
        assert_eq!(variant_to_weight("regular"), Some(400));
        assert_eq!(variant_to_weight(""), Some(400));
        assert_eq!(variant_to_weight("400"), Some(400));
        assert_eq!(variant_to_weight("700"), Some(700));
        assert_eq!(variant_to_weight("700italic"), Some(700));
        assert_eq!(variant_to_weight("italic"), Some(400));
        assert_eq!(variant_to_weight("thin"), Some(100));
        assert_eq!(variant_to_weight("black"), Some(900));
        assert_eq!(variant_to_weight("extrabold"), Some(800));
    }

    #[test]
    fn derive_display_family_titlecases_and_unjoins() {
        assert_eq!(derive_display_family("roboto"), "Roboto");
        assert_eq!(derive_display_family("open-sans"), "Open Sans");
        assert_eq!(derive_display_family("noto-sans-jp"), "Noto Sans Jp");
    }

    #[test]
    fn safe_font_basename_sanitizes() {
        assert_eq!(safe_font_basename("Roboto", 400), "roboto-400");
        assert_eq!(safe_font_basename("open-sans", 700), "open-sans-700");
        // We never see this in practice, but be defensive.
        assert_eq!(safe_font_basename("Weird/Font", 400), "weird-font-400");
    }

    #[test]
    fn derive_display_family_handles_underscores() {
        // Family ids from upstream only use hyphens, but be defensive.
        assert_eq!(derive_display_family("dm_serif_display"), "Dm Serif Display");
    }

    // ============================================================
    // Preview cache parser tests
    // ============================================================

    #[test]
    fn extract_first_woff2_finds_url() {
        let css = "src: url(https://fonts.gstatic.com/s/roboto/v1/foo.woff2) format('woff2');";
        assert_eq!(
            extract_first_woff2(css),
            Some("https://fonts.gstatic.com/s/roboto/v1/foo.woff2".to_string())
        );
    }

    #[test]
    fn extract_first_woff2_strips_quotes() {
        let css = r#"src: url('https://fonts.gstatic.com/foo.woff2') format('woff2');"#;
        assert_eq!(
            extract_first_woff2(css),
            Some("https://fonts.gstatic.com/foo.woff2".to_string())
        );
    }

    #[test]
    fn extract_first_woff2_returns_none_when_missing() {
        assert_eq!(extract_first_woff2("no url here"), None);
    }

    #[test]
    fn extract_latin_woff2_picks_latin_subset() {
        // Real-world Google Fonts CSS shape: multiple @font-face
        // blocks per family, one per subset.
        let css = r#"
@font-face {
  font-family: 'Roboto';
  font-style: normal;
  font-weight: 400;
  src: url(https://fonts.gstatic.com/cyrillic.woff2) format('woff2');
  unicode-range: U+0400-04FF;
}
@font-face {
  font-family: 'Roboto';
  font-style: normal;
  font-weight: 400;
  src: url(https://fonts.gstatic.com/latin.woff2) format('woff2');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153;
}
"#;
        assert_eq!(
            extract_latin_woff2(css),
            Some("https://fonts.gstatic.com/latin.woff2".to_string())
        );
    }

    #[test]
    fn extract_latin_woff2_returns_none_when_no_latin_subset() {
        // The latin-only fallback is handled by the caller
        // (ensure_preview_cached) chaining extract_first_woff2.
        // The helper itself only returns Some for an actual
        // latin subset block.
        let css = r#"
@font-face {
  font-family: 'Noto Sans JP';
  src: url(https://fonts.gstatic.com/jp.woff2) format('woff2');
  unicode-range: U+3000-30FF;
}
"#;
        assert_eq!(extract_latin_woff2(css), None);
        // And the chain (extract_latin_woff2 + extract_first_woff2)
        // recovers the URL:
        let recovered = extract_latin_woff2(css)
            .or_else(|| extract_first_woff2(css));
        assert_eq!(
            recovered,
            Some("https://fonts.gstatic.com/jp.woff2".to_string())
        );
    }

    #[test]
    fn preview_cache_path_validates_family_id() {
        assert!(preview_cache_path("", 400).is_err());
        assert!(preview_cache_path(&"a".repeat(65), 400).is_err());
        assert!(preview_cache_path("evil/path", 400).is_err());
        assert!(preview_cache_path("evil..path", 400).is_err());
        assert!(preview_cache_path("roboto", 400).is_ok());
        assert!(preview_cache_path("open-sans", 400).is_ok());
    }

    // ============================================================
    // Filename parsing for the preview cache
    // ============================================================

    #[test]
    fn filename_stem_to_id_handles_simple_family() {
        // "roboto-400" -> "roboto"
        let s = "roboto-400";
        let id = s.rsplitn(2, '-').nth(1);
        assert_eq!(id, Some("roboto"));
    }

    #[test]
    fn filename_stem_to_id_handles_hyphenated_family() {
        // "open-sans-400" -> "open-sans" (NOT "sans")
        let s = "open-sans-400";
        let id = s.rsplitn(2, '-').nth(1);
        assert_eq!(id, Some("open-sans"));
    }

    #[test]
    fn filename_stem_to_id_handles_multi_hyphenated_family() {
        // "noto-sans-jp-400" -> "noto-sans-jp"
        let s = "noto-sans-jp-400";
        let id = s.rsplitn(2, '-').nth(1);
        assert_eq!(id, Some("noto-sans-jp"));
    }

    // ============================================================
    // preview_status: must not promise files that aren't on disk.
    //
    // The catalog has families whose woff2 exceeds the 200KB
    // cache cap (Material Symbols: 322KB, Material Symbols
    // Rounded: 372KB, etc.) so their preview never gets
    // written. preview_status() has to verify the file
    // actually exists before returning its URL -- otherwise
    // the browser logs 404s for every visible card.
    //
    // We can't unit-test preview_status directly because it
    // reads from disk. The behavior is verified by the live
    // API test (curl /api/fonts/preview-status with a
    // Material Symbols family and confirm the response is
    // empty).
    // ============================================================
}
