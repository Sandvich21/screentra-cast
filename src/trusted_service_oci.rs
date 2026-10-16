use std::collections::BTreeSet;
use std::path::Path;

pub const SUPPORTED_ROOTLESS_RUNTIMES: &[&str] = &["podman", "nerdctl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciRuntimeProbeReport {
    pub runtime_name: Option<String>,
    pub executable_present: bool,
    pub can_start: bool,
    pub blocked_by: Vec<String>,
}

pub fn probe_oci_runtime(path: &Path) -> OciRuntimeProbeReport {
    let runtime_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string);
    let mut blocked_by = Vec::new();
    if !runtime_name
        .as_deref()
        .is_some_and(|name| SUPPORTED_ROOTLESS_RUNTIMES.contains(&name))
    {
        blocked_by.push("unsupported rootless runtime".to_string());
    }
    let executable_present = path.is_file() && is_executable(path);
    if !executable_present {
        blocked_by.push("runtime executable unavailable".to_string());
    }
    // Capability probing and execution remain intentionally disabled until the
    // separately reviewed OCI supervisor, gateway, and resource policy exist.
    blocked_by.push("OCI capability probe is not enabled".to_string());
    OciRuntimeProbeReport {
        runtime_name,
        executable_present,
        can_start: false,
        blocked_by,
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciLaunchPolicy {
    pub rootless: bool,
    pub network_disabled: bool,
    pub read_only_rootfs: bool,
    pub no_new_privileges: bool,
    pub drop_all_capabilities: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciResourceLimits {
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub writable_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciLaunchSpec {
    pub addon_id: String,
    pub activation_instance: String,
    pub image: String,
    pub image_digest: String,
    pub capabilities: Vec<String>,
    pub storage_namespace: String,
    pub api_namespace: String,
    pub command: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub host_mounts: Vec<String>,
    pub published_ports: Vec<u16>,
    pub policy: OciLaunchPolicy,
    pub resources: OciResourceLimits,
}

pub fn validate_oci_launch_spec(spec: &OciLaunchSpec) -> Result<(), String> {
    crate::addons::validate_addon_storage_namespace(&spec.addon_id)?;
    crate::addons::validate_addon_storage_namespace(&spec.activation_instance)
        .map_err(|_| "Invalid activation instance".to_string())?;
    if spec.image.is_empty()
        || spec.image.len() > 512
        || spec.image.contains(':')
        || spec.image.contains('@')
        || spec.image.chars().any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err("OCI image must be an immutable untagged reference".to_string());
    }
    let digest = spec
        .image_digest
        .strip_prefix("sha256:")
        .ok_or_else(|| "OCI image digest must use lowercase sha256".to_string())?;
    if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()) {
        return Err("OCI image digest must be lowercase sha256".to_string());
    }
    if spec.storage_namespace != spec.addon_id || spec.api_namespace != spec.addon_id {
        return Err("OCI namespaces must bind to the addon identity".to_string());
    }
    crate::addons::validate_addon_storage_namespace(&spec.storage_namespace)?;
    crate::addons::validate_addon_storage_namespace(&spec.api_namespace)?;
    let mut seen = BTreeSet::new();
    for capability in &spec.capabilities {
        if !crate::trusted_service_runtime::TRUSTED_SERVICE_CAPABILITIES
            .contains(&capability.as_str())
            || !seen.insert(capability)
        {
            return Err("OCI capabilities are unsupported or duplicated".to_string());
        }
    }
    if spec.command.is_empty()
        || spec.command.iter().any(|part| {
            part.is_empty()
                || part.chars().any(|ch| ch.is_control())
                || matches!(part.as_str(), "sh" | "bash" | "dash" | "/bin/sh" | "/bin/bash" | "-c" | "-lc")
        })
    {
        return Err("OCI command must not invoke a shell".to_string());
    }
    if !spec.environment.is_empty() || !spec.host_mounts.is_empty() || !spec.published_ports.is_empty() {
        return Err("OCI launch spec exposes a forbidden host boundary".to_string());
    }
    if !spec.policy.rootless
        || !spec.policy.network_disabled
        || !spec.policy.read_only_rootfs
        || !spec.policy.no_new_privileges
        || !spec.policy.drop_all_capabilities
    {
        return Err("OCI isolation policy is incomplete".to_string());
    }
    if spec.resources.memory_bytes == 0
        || spec.resources.memory_bytes > 4 * 1024 * 1024 * 1024
        || spec.resources.cpu_millis == 0
        || spec.resources.cpu_millis > 16_000
        || spec.resources.pids == 0
        || spec.resources.pids > 4_096
        || spec.resources.writable_bytes == 0
        || spec.resources.writable_bytes > 1024 * 1024 * 1024
    {
        return Err("OCI resource limits are invalid".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        probe_oci_runtime, validate_oci_launch_spec, OciLaunchPolicy, OciLaunchSpec,
        OciResourceLimits,
    };

    fn safe_spec() -> OciLaunchSpec {
        OciLaunchSpec {
            addon_id: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
            image: "registry.example/weather".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.weather".to_string(),
            api_namespace: "example.weather".to_string(),
            command: vec!["/service".to_string()],
            environment: Vec::new(),
            host_mounts: Vec::new(),
            published_ports: Vec::new(),
            policy: OciLaunchPolicy {
                rootless: true,
                network_disabled: true,
                read_only_rootfs: true,
                no_new_privileges: true,
                drop_all_capabilities: true,
            },
            resources: OciResourceLimits {
                memory_bytes: 256 * 1024 * 1024,
                cpu_millis: 500,
                pids: 64,
                writable_bytes: 16 * 1024 * 1024,
            },
        }
    }

    #[test]
    fn accepts_immutable_rootless_launch_spec() {
        validate_oci_launch_spec(&safe_spec()).expect("safe OCI spec should validate");
    }

    #[test]
    fn rejects_mutable_image_tags_and_noncanonical_digests() {
        let mut mutable = safe_spec();
        mutable.image = "registry.example/weather:latest".to_string();
        assert!(validate_oci_launch_spec(&mutable).is_err());

        let mut uppercase = safe_spec();
        uppercase.image_digest = format!("sha256:{}", "A".repeat(64));
        assert!(validate_oci_launch_spec(&uppercase).is_err());
    }

    #[test]
    fn rejects_host_exposure_shell_execution_and_environment() {
        let mut exposed = safe_spec();
        exposed.host_mounts.push("/etc:/etc".to_string());
        assert!(validate_oci_launch_spec(&exposed).is_err());

        let mut port = safe_spec();
        port.published_ports.push(8080);
        assert!(validate_oci_launch_spec(&port).is_err());

        let mut shell = safe_spec();
        shell.command = vec!["/bin/sh".to_string(), "-c".to_string(), "run".to_string()];
        assert!(validate_oci_launch_spec(&shell).is_err());

        let mut environment = safe_spec();
        environment.environment.push(("TOKEN".to_string(), "secret".to_string()));
        assert!(validate_oci_launch_spec(&environment).is_err());
    }

    #[test]
    fn rejects_duplicate_capabilities_and_invalid_identity() {
        let mut duplicate = safe_spec();
        duplicate.capabilities.push("storage".to_string());
        assert!(validate_oci_launch_spec(&duplicate).is_err());

        let mut invalid = safe_spec();
        invalid.activation_instance = "../other".to_string();
        assert!(validate_oci_launch_spec(&invalid).is_err());
    }

    #[test]
    fn missing_oci_runtime_is_blocked_without_fallback() {
        let report = probe_oci_runtime(Path::new("/definitely/missing/screentra-oci-runtime"));
        assert!(!report.executable_present);
        assert!(!report.can_start);
        assert!(report
            .blocked_by
            .iter()
            .any(|reason| reason == "runtime executable unavailable"));
    }
}
