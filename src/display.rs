use crate::models::{DisplayInfo, DisplayMode};
use std::fs;
use std::io::Read;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Cache: (result, timestamp). Refreshes if older than 5 seconds.
static DISPLAY_CACHE: OnceLock<(Vec<DisplayInfo>, Instant)> = OnceLock::new();
const CACHE_TTL: Duration = Duration::from_secs(5);
const MAX_SYSFS_VALUE_BYTES: usize = 4096;
const MAX_DISPLAY_RECORDS: usize = 32;
const MAX_DISPLAY_MODES: usize = 128;

/// Detect connected displays by reading /sys/class/drm entries.
/// Results are cached for 5 seconds to avoid hammering sysfs.
pub fn detect_displays() -> Vec<DisplayInfo> {
    let now = Instant::now();
    if let Some((cached, ts)) = DISPLAY_CACHE.get() {
        if now.duration_since(*ts) < CACHE_TTL {
            return cached.clone();
        }
    }
    let result = detect_displays_uncached();
    DISPLAY_CACHE.set((result.clone(), now)).ok();
    result
}

fn detect_displays_uncached() -> Vec<DisplayInfo> {
    let mut displays = Vec::new();
    let drm_path = "/sys/class/drm";

    if let Ok(entries) = fs::read_dir(drm_path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip card-level entries, only look at connectors (cardN-CONNECTOR)
            if !name.contains('-') || name.starts_with("card") && !name.contains("card") {
                continue;
            }
            // Filter: must start with "card" and have a dash (e.g. "card1-DP-1")
            if !name.starts_with("card") {
                continue;
            }

            let status = read_sysfs(&format!("{}/{}/status", drm_path, name));
            let enabled = read_sysfs(&format!("{}/{}/enabled", drm_path, name)) == "enabled";

            // Read physical dimensions
            let (width_mm, height_mm) = read_mm(&format!("{}/{}", drm_path, name));

            // Read supported modes
            let modes = read_modes(&format!("{}/{}/modes", drm_path, name));

            // Extract clean connector name before building display info
            let connector = extract_connector(&name);

            // Generate a human-friendly label
            let label = generate_label(&connector, &status, &modes);

            let display = DisplayInfo {
                connector,
                status,
                enabled,
                width_mm,
                height_mm,
                current_mode: modes.first().cloned(),
                modes,
                label,
                override_width: None,
                override_height: None,
            };

            displays.push(display);
            if displays.len() >= MAX_DISPLAY_RECORDS {
                break;
            }
        }
    }

    // Sort by connector name for consistent ordering
    displays.sort_by(|a, b| a.connector.cmp(&b.connector));
    displays
}

fn read_sysfs(path: &str) -> String {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let mut raw = Vec::new();
    if file
        .by_ref()
        .take((MAX_SYSFS_VALUE_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .is_err()
        || raw.len() > MAX_SYSFS_VALUE_BYTES
    {
        return String::new();
    }
    String::from_utf8_lossy(&raw).trim().to_string()
}

fn read_mm(base: &str) -> (u32, u32) {
    let width_mm = read_sysfs_property(base, "width_mm");
    let height_mm = read_sysfs_property(base, "height_mm");
    (width_mm, height_mm)
}

fn read_sysfs_property(base: &str, prop: &str) -> u32 {
    read_sysfs(&format!("{}/{}", base, prop))
        .parse()
        .unwrap_or(0)
}

fn read_modes(path: &str) -> Vec<DisplayMode> {
    read_bounded_text(path)
        .map(|content| read_modes_from_content(&content))
        .unwrap_or_default()
}

fn read_bounded_text(path: &str) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    file.take((MAX_SYSFS_VALUE_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > MAX_SYSFS_VALUE_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&raw).into_owned())
}

fn read_modes_from_content(content: &str) -> Vec<DisplayMode> {
    let mut seen = std::collections::HashSet::new();
    let mut modes = Vec::new();
    for line in content.lines() {
        if let Some((w, h)) = parse_mode_line(line) {
            let key = (w, h);
            if seen.insert(key) {
                modes.push(DisplayMode {
                    width: w,
                    height: h,
                    refresh_rate: 60.0,
                });
                if modes.len() >= MAX_DISPLAY_MODES {
                    break;
                }
            }
        }
    }
    modes
}

fn parse_mode_line(line: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = line.split('x').collect();
    if parts.len() == 2 {
        let w: u32 = parts[0].trim().parse().ok()?;
        let h: u32 = parts[1].trim().parse().ok()?;
        Some((w, h))
    } else {
        None
    }
}

/// Extract a clean connector name from a DRM sysfs entry name.
/// "card1-DP-1" -> "DP-1"
fn extract_connector(name: &str) -> String {
    // Find the first dash after "cardN"
    if let Some(pos) = name.find('-') {
        name[pos + 1..].to_string()
    } else {
        name.to_string()
    }
}

/// Generate a human-friendly label for a display.
fn generate_label(connector: &str, status: &str, modes: &[DisplayMode]) -> String {
    if status != "connected" {
        return format!("{} (disconnected)", connector);
    }

    let resolution = modes
        .first()
        .map(|m| format!("{}x{}", m.width, m.height))
        .unwrap_or_else(|| "Unknown".to_string());

    let display_type = if connector.starts_with("DP") {
        "DisplayPort"
    } else if connector.starts_with("HDMI") {
        "HDMI"
    } else if connector.starts_with("eDP") {
        "Embedded DisplayPort"
    } else if connector.starts_with("VGA") {
        "VGA"
    } else {
        "Display"
    };

    format!("{} {} ({})", display_type, connector, resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_mode_records_are_bounded() {
        let content = (0..(MAX_DISPLAY_MODES + 8))
            .map(|index| format!("{}x{}", 640 + index, 480 + index))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(read_modes_from_content(&content).len(), MAX_DISPLAY_MODES);
    }

    #[test]
    fn display_records_are_bounded() {
        assert_eq!(MAX_DISPLAY_RECORDS, 32);
    }
}
