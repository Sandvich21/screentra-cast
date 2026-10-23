use crate::display::detect_displays;
use crate::models::SystemStatus;
use std::fs;
use std::io::Read;
use sysinfo::System;

pub fn get_system_status() -> SystemStatus {
    let mut sys = System::new_all();
    sys.refresh_all();

    let hostname = System::host_name().unwrap_or_else(|| "unknown".to_string());

    // Read OS info from /etc/os-release
    let (os_name, os_version) = read_os_release();

    let uptime_seconds = System::uptime();

    // CPU usage — average across all cores
    let cpu_usage = sys.global_cpu_usage();
    let cpu_count = sys.cpus().len();

    let memory_total = sys.total_memory();
    let memory_used = sys.used_memory();

    // Disk info for root partition
    let (disk_total, disk_used) = get_disk_info();

    // CPU temperature
    let cpu_temp = read_cpu_temp();

    // GPU info

    let displays = detect_displays();

    SystemStatus {
        hostname,
        os_name,
        os_version,
        uptime_seconds,
        cpu_usage,
        cpu_count,
        memory_total,
        memory_used,
        disk_total,
        disk_used,
        cpu_temp,
        displays,
        service_status: "running".to_string(),
    }
}

fn read_bounded_text(path: &str, maximum_bytes: usize) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take((maximum_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > maximum_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_os_release() -> (String, String) {
    let content = read_bounded_text("/etc/os-release", 64 * 1024).unwrap_or_default();
    let mut name = String::from("Unknown");
    let mut version = String::from("Unknown");
    for line in content.lines() {
        if line.starts_with("NAME=") {
            name = line
                .trim_start_matches("NAME=")
                .trim_matches('"')
                .to_string();
        }
        if line.starts_with("VERSION=") {
            version = line
                .trim_start_matches("VERSION=")
                .trim_matches('"')
                .to_string();
        }
    }
    (name, version)
}

fn get_disk_info() -> (u64, u64) {
    // Use statvfs for root partition
    use std::ffi::CString;
    let path = CString::new("/").unwrap();

    // We'll read from /proc/mounts as a simpler approach
    let mounts = read_bounded_text("/proc/mounts", 1024 * 1024).unwrap_or_default();
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[1] == "/" && parts[2] == "ext4" {
            // Found root partition
            let _ = path;
            break;
        }
    }

    // Use sysinfo's Disks struct (0.33+ API)
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    for disk in disks.list() {
        if disk.mount_point().to_string_lossy() == "/" {
            return (
                disk.total_space(),
                disk.total_space() - disk.available_space(),
            );
        }
    }

    (0, 0)
}

fn read_cpu_temp() -> Option<f32> {
    // Try reading from hwmon (AMD systems)
    let temp_paths = [
        "/sys/class/hwmon/hwmon0/temp1_input",
        "/sys/class/hwmon/hwmon1/temp1_input",
        "/sys/class/hwmon/hwmon2/temp1_input",
        "/sys/class/thermal/thermal_zone0/temp",
    ];

    for path in &temp_paths {
        if let Ok(content) = fs::read_to_string(path) {
            if let Ok(temp) = content.trim().parse::<f32>() {
                return Some(temp / 1000.0); // Convert millidegree to degree
            }
        }
    }
    None
}
