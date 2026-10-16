use crate::trusted_service_runtime::{
    ActivationGate, TrustedServiceRuntimeRecord, EXECUTION_ENABLED,
};
use std::collections::BTreeSet;
use std::path::Path;

pub const SUPPORTED_ROOTLESS_RUNTIMES: &[&str] = &["podman", "nerdctl"];
pub const REQUIRED_OCI_ACTIVATION_GATES: &[&str] = &[
    "manifest_signature",
    "image_signature",
    "administrator_approval",
    "execution_backend",
    "credential_broker",
    "database_boundary",
    "controlled_gateway",
    "resource_policy",
];

pub fn validate_oci_activation_gates(gates: &[ActivationGate]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    if gates.iter().any(|gate| {
        gate.name.is_empty()
            || gate.name.len() > 128
            || gate.name.chars().any(|character| character.is_control())
            || gate.reason.is_empty()
            || gate.reason.len() > 512
            || gate.reason.chars().any(|character| character.is_control())
            || !REQUIRED_OCI_ACTIVATION_GATES.contains(&gate.name.as_str())
            || !seen.insert(&gate.name)
    }) {
        return Err("OCI activation gates are malformed or duplicated".to_string());
    }
    if REQUIRED_OCI_ACTIVATION_GATES.iter().any(|required| {
        !gates
            .iter()
            .any(|gate| gate.name == *required && gate.passed)
    }) {
        return Err("Trusted service activation gates are incomplete".to_string());
    }
    Ok(())
}

pub fn validate_oci_approval_metadata(record: &TrustedServiceRuntimeRecord) -> Result<(), String> {
    let approver = record
        .approved_by
        .as_deref()
        .ok_or_else(|| "Trusted service approval metadata is incomplete".to_string())?;
    if approver.is_empty()
        || approver.len() > 128
        || approver.chars().any(|character| character.is_control())
    {
        return Err("Trusted service approval metadata is invalid".to_string());
    }
    let approved_at = record
        .approved_at
        .as_deref()
        .ok_or_else(|| "Trusted service approval metadata is incomplete".to_string())?;
    if approved_at.len() > 64
        || approved_at.chars().any(|character| character.is_control())
        || chrono::DateTime::parse_from_rfc3339(approved_at).is_err()
    {
        return Err("Trusted service approval metadata is invalid".to_string());
    }
    Ok(())
}

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

pub fn validate_oci_launch_spec_against_record(
    spec: &OciLaunchSpec,
    record: &TrustedServiceRuntimeRecord,
) -> Result<(), String> {
    if !EXECUTION_ENABLED || record.state != "approved" || !record.execution_enabled {
        return Err("Trusted service execution is disabled".to_string());
    }
    if record.approved_by.is_none() || validate_oci_activation_gates(&record.gates).is_err() {
        return Err("Trusted service activation gates are incomplete".to_string());
    }
    if validate_oci_approval_metadata(record).is_err() {
        return Err("Trusted service approval metadata is invalid".to_string());
    }
    if spec.addon_id != record.addon_id
        || spec.activation_instance != record.activation_instance
        || spec.image != record.image
        || spec.image_digest != record.image_digest
        || spec.capabilities != record.capabilities
        || spec.storage_namespace != record.storage_namespace
        || spec.api_namespace != record.api_namespace
    {
        return Err("OCI launch spec does not match the approved activation".to_string());
    }
    validate_oci_launch_spec(spec)
}

pub fn validate_oci_launch_spec_against_record_with_registry_policy(
    spec: &OciLaunchSpec,
    record: &TrustedServiceRuntimeRecord,
    registry_policy: &OciRegistryPolicy,
) -> Result<(), String> {
    validate_oci_launch_spec_against_record(spec, record)?;
    validate_oci_image_registry(&spec.image, registry_policy)
}

pub fn build_rootless_argv(
    runtime_name: &str,
    spec: &OciLaunchSpec,
) -> Result<Vec<String>, String> {
    validate_oci_launch_spec(spec)?;
    if !SUPPORTED_ROOTLESS_RUNTIMES.contains(&runtime_name) {
        return Err("unsupported rootless runtime".to_string());
    }
    let mut argv = vec![runtime_name.to_string(), "run".to_string()];
    argv.extend([
        "--rm".to_string(),
        format!("--name={}", container_name(spec)),
        "--network=none".to_string(),
        "--read-only".to_string(),
        "--user=65532:65532".to_string(),
        "--cap-drop=ALL".to_string(),
        "--security-opt=no-new-privileges".to_string(),
        "--security-opt=seccomp=default".to_string(),
        format!("--tmpfs=/tmp:rw,size={}", spec.resources.writable_bytes),
        format!("--pids-limit={}", spec.resources.pids),
        format!("--memory={}", spec.resources.memory_bytes),
        format!(
            "--cpus={}.{:03}",
            spec.resources.cpu_millis / 1_000,
            spec.resources.cpu_millis % 1_000
        ),
        format!("{}@{}", spec.image, spec.image_digest),
    ]);
    argv.extend(spec.command.iter().cloned());
    Ok(argv)
}

fn container_name(spec: &OciLaunchSpec) -> String {
    let mut name = format!("screentra-{}-{}", spec.addon_id, spec.activation_instance)
        .chars()
        .map(|ch| if ch == '.' || ch == '_' { '-' } else { ch })
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(63)
        .collect::<String>();
    if name.is_empty() {
        name = "screentra-addon".to_string();
    }
    name
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciLaunchPolicy {
    pub rootless: bool,
    pub non_root: bool,
    pub network_disabled: bool,
    pub read_only_rootfs: bool,
    pub no_new_privileges: bool,
    pub drop_all_capabilities: bool,
    pub seccomp_default: bool,
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
    pub gateway_socket_path: String,
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
        || spec
            .image
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err("OCI image must be an immutable untagged reference".to_string());
    }
    let digest = spec
        .image_digest
        .strip_prefix("sha256:")
        .ok_or_else(|| "OCI image digest must use lowercase sha256".to_string())?;
    if digest.len() != 64
        || !digest
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return Err("OCI image digest must be lowercase sha256".to_string());
    }
    if spec.storage_namespace != spec.addon_id || spec.api_namespace != spec.addon_id {
        return Err("OCI namespaces must bind to the addon identity".to_string());
    }
    crate::addons::validate_addon_storage_namespace(&spec.storage_namespace)?;
    crate::addons::validate_addon_storage_namespace(&spec.api_namespace)?;
    if spec.gateway_socket_path != "/run/screentra/gateway.sock" {
        return Err("OCI gateway endpoint is not controlled".to_string());
    }
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
                || matches!(
                    part.as_str(),
                    "sh" | "bash" | "dash" | "/bin/sh" | "/bin/bash" | "-c" | "-lc"
                )
        })
    {
        return Err("OCI command must not invoke a shell".to_string());
    }
    if !spec.environment.is_empty()
        || !spec.host_mounts.is_empty()
        || !spec.published_ports.is_empty()
    {
        return Err("OCI launch spec exposes a forbidden host boundary".to_string());
    }
    if !spec.policy.rootless
        || !spec.policy.non_root
        || !spec.policy.network_disabled
        || !spec.policy.read_only_rootfs
        || !spec.policy.no_new_privileges
        || !spec.policy.drop_all_capabilities
        || !spec.policy.seccomp_default
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciRegistryPolicy {
    pub allowed_registries: BTreeSet<String>,
}

pub fn validate_oci_image_registry(image: &str, policy: &OciRegistryPolicy) -> Result<(), String> {
    if policy.allowed_registries.is_empty()
        || policy.allowed_registries.iter().any(|registry| {
            registry.is_empty()
                || registry.len() > 255
                || registry.contains('/')
                || registry.contains(':')
                || registry.contains('@')
                || registry.contains("..")
                || registry.starts_with('.')
                || registry.ends_with('.')
                || registry.starts_with('-')
                || registry.ends_with('-')
                || registry.split('.').any(|label| {
                    label.is_empty()
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric() || character == '-')
                })
                || registry
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
                || registry != &registry.to_ascii_lowercase()
        })
    {
        return Err("OCI registry allowlist is invalid".to_string());
    }
    let (registry, repository) = image
        .split_once('/')
        .ok_or_else(|| "OCI image must use an allowlisted registry".to_string())?;
    if repository.is_empty()
        || repository.starts_with('/')
        || repository.contains("//")
        || repository
            .split('/')
            .any(|part| part == "." || part == "..")
    {
        return Err("OCI image repository path is invalid".to_string());
    }
    if !policy.allowed_registries.contains(registry) {
        return Err("OCI image registry is not allowlisted".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::{
        build_rootless_argv, probe_oci_runtime, validate_oci_activation_gates,
        validate_oci_approval_metadata, validate_oci_image_registry, validate_oci_launch_spec,
        validate_oci_launch_spec_against_record, ActivationGate, OciLaunchPolicy, OciLaunchSpec,
        OciRegistryPolicy, OciResourceLimits, TrustedServiceRuntimeRecord,
        REQUIRED_OCI_ACTIVATION_GATES,
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
            gateway_socket_path: "/run/screentra/gateway.sock".to_string(),
            command: vec!["/service".to_string()],
            environment: Vec::new(),
            host_mounts: Vec::new(),
            published_ports: Vec::new(),
            policy: OciLaunchPolicy {
                rootless: true,
                non_root: true,
                network_disabled: true,
                read_only_rootfs: true,
                no_new_privileges: true,
                drop_all_capabilities: true,
                seccomp_default: true,
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
    fn activation_gate_validation_rejects_duplicates_and_incomplete_passes() {
        let complete = REQUIRED_OCI_ACTIVATION_GATES
            .iter()
            .map(|name| ActivationGate {
                name: (*name).to_string(),
                passed: true,
                reason: "verified".to_string(),
            })
            .collect::<Vec<_>>();
        validate_oci_activation_gates(&complete).expect("complete gates should pass");

        let mut duplicate = complete.clone();
        duplicate.push(ActivationGate {
            name: "image_signature".to_string(),
            passed: false,
            reason: "conflict".to_string(),
        });
        assert!(validate_oci_activation_gates(&duplicate).is_err());

        let mut unknown = complete.clone();
        unknown.push(ActivationGate {
            name: "future_gate".to_string(),
            passed: true,
            reason: "unexpected".to_string(),
        });
        assert!(validate_oci_activation_gates(&unknown).is_err());

        let mut malformed_reason = complete.clone();
        malformed_reason[0].reason = "bad\nreason".to_string();
        assert!(validate_oci_activation_gates(&malformed_reason).is_err());

        let mut empty_reason = complete.clone();
        empty_reason[0].reason.clear();
        assert!(validate_oci_activation_gates(&empty_reason).is_err());

        let mut incomplete = complete;
        incomplete[0].passed = false;
        assert!(validate_oci_activation_gates(&incomplete).is_err());
    }

    #[test]
    fn registry_policy_requires_an_explicit_allowlisted_registry() {
        let policy = OciRegistryPolicy {
            allowed_registries: BTreeSet::from(["registry.example".to_string()]),
        };
        validate_oci_image_registry("registry.example/weather", &policy)
            .expect("allowlisted registry should pass");
        assert!(validate_oci_image_registry("docker.io/weather", &policy).is_err());
        assert!(validate_oci_image_registry("weather", &policy).is_err());
        assert!(validate_oci_image_registry("registry.example/../weather", &policy).is_err());

        let invalid_policy = OciRegistryPolicy {
            allowed_registries: BTreeSet::from(["Registry.Example".to_string()]),
        };
        assert!(validate_oci_image_registry("registry.example/weather", &invalid_policy).is_err());

        let malformed_policy = OciRegistryPolicy {
            allowed_registries: BTreeSet::from(["registry!.example".to_string()]),
        };
        assert!(
            validate_oci_image_registry("registry.example/weather", &malformed_policy).is_err()
        );
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
        environment
            .environment
            .push(("TOKEN".to_string(), "secret".to_string()));
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

        let mut gateway = safe_spec();
        gateway.gateway_socket_path = "/tmp/host.sock".to_string();
        assert!(validate_oci_launch_spec(&gateway).is_err());
    }

    #[test]
    fn builds_fixed_rootless_argv_without_shell_or_host_boundaries() {
        let args = build_rootless_argv("podman", &safe_spec()).expect("argv should validate");
        assert_eq!(args.first().map(String::as_str), Some("podman"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--network=none", "--read-only"]));
        assert!(args.iter().any(|arg| arg == "--cap-drop=ALL"));
        assert!(args.iter().any(|arg| arg == "--user=65532:65532"));
        assert!(args
            .iter()
            .any(|arg| arg == "--security-opt=seccomp=default"));
        assert!(args
            .iter()
            .any(|arg| arg == "--tmpfs=/tmp:rw,size=16777216"));
        assert!(args
            .iter()
            .any(|arg| arg == "--security-opt=no-new-privileges"));
        assert!(args.iter().any(|arg| arg == "registry.example/weather@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert_eq!(args.last().map(String::as_str), Some("/service"));
        assert!(!args
            .iter()
            .any(|arg| arg == "-c" || arg == "--volume" || arg == "-v"));
    }
    #[test]
    fn approved_record_binding_still_respects_disabled_execution_gate() {
        let record = TrustedServiceRuntimeRecord {
            record_version: 1,
            addon_id: "example.weather".to_string(),
            addon_version: "1.0.0".to_string(),
            image: "registry.example/weather".to_string(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            capabilities: vec!["storage".to_string()],
            storage_namespace: "example.weather".to_string(),
            api_namespace: "example.weather".to_string(),
            activation_instance: "activation-1".to_string(),
            state: "approved".to_string(),
            approved_by: Some("admin".to_string()),
            approved_at: Some("2026-10-15T00:00:00Z".to_string()),
            updated_at: "2026-10-15T00:00:00Z".to_string(),
            execution_enabled: false,
            gates: Vec::new(),
            last_error: None,
        };
        validate_oci_approval_metadata(&record).expect("approval metadata should validate");
        let mut invalid = record.clone();
        invalid.approved_by = Some(String::new());
        assert!(validate_oci_approval_metadata(&invalid).is_err());
        assert!(validate_oci_launch_spec_against_record(&safe_spec(), &record).is_err());
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
