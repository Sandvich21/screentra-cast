# Magic Mirror Add-on — October 30, 2026 Delivery Plan

## Goal

Deliver a bounded Magic Mirror add-on for a designated test client first, with local speech-to-text, a Camera widget, and lightweight Halloween filters. Promote only after the client-visible behavior is verified.

The first target is client `11.1.1.56`; the server remains `.25`. No fleet rollout is included until the test client passes.

## Implementation status (2026-10-18)

The metadata-only trusted add-on lane now reports bounded client capabilities and service health, carries inert client-service status/rollback and push-to-talk transcription requests through the existing closed command path, and includes a dashboard surface for authenticated queued status and rollback-assessment requests. It also includes a built-in Camera widget with opt-in browser-local capture plus fixed local filters (`none`, `grayscale`, `sepia`, `halloween`). A bounded local-only AI IPC codec now defines future `health`/`transcribe` frames without opening a listener or running inference. A test-only in-memory local AI protocol fixture now decodes complete requests through that typed codec and returns deterministic bounded health, transcription, and error frames; its focused protocol tests are fixture evidence only, not service execution or runtime-readiness evidence. A contract-only Magic Mirror local STT client-extension manifest fixture now covers signed metadata shape, microphone/local-inference/client-service capabilities, digest-pinned bounded service metadata, and declarative rejection. A read-only local model identity/integrity helper now validates bounded model IDs, exact lowercase SHA-256 digests, approved-root containment, regular non-symlink files, and bounded hashing; it does not stage, download, execute, or activate a model. A pure client-service lifecycle contract now validates bounded identity/version metadata and the closed state graph, including explicit rollback-ready metadata; its tests are contract evidence only and do not provide runtime or process-execution evidence. The local AI health response may also carry a paired, bounded model ID and exact lowercase SHA-256 digest; this is protocol identity metadata only and does not prove a model is staged, loaded, or running. A pure health-observation binding helper now checks decoded or serialized observations against the expected service/version/activation tuple and optional model identity, returning metadata-only evidence rather than runtime health. The actual client supervisor, speech-to-text service, model staging, camera hardware verification, and filter inference remain deferred and execution-disabled.

### Capability policy/admission evidence

The pure client capability-admission helper is contract evidence for a future
capability broker, not permission activation. It validates bounded client and
add-on identities, the closed unique capability set, dependency rules, and an
explicit administrator policy whose approved set is empty by default. A
metadata-only admission result can admit only a requested subset of that
policy and always reports `execution_enabled: false`. Admission does not claim
camera or microphone permission, local inference, a running client service, or
any other hardware or process activation; those require a separate reviewed
runtime/permission gate.

### Supervisor-plan evidence boundary

The pure `screentra-client-service-supervisor-plan.py` contract composes the lifecycle and health identity validators for the Magic Mirror STT metadata. Its activation plans are metadata-only evidence: `execution_enabled` is false and `process_started`, `socket_bound`, and `model_loaded` are false. Health/status may bind a bounded observation; start/stop/update/rollback return structured `execution_disabled` results without changing lifecycle state. A plan is not evidence of a running service, bound socket, loaded model, installed package, or hardware readiness.

### Local-model staging-plan evidence boundary

The pure `screentra-local-model-staging-plan.py` contract binds the service and
activation identities to an exact lowercase model digest, an approved absolute
root, and a bounded relative path. Its `verify` intent composes the existing
read-only integrity helper only for a caller-supplied local path; `stage`,
`download`, and `load` return `execution_disabled` without filesystem or
network work. Every result reports execution, download, staging, loading,
process, and network flags as false. A verification or stage plan is metadata
evidence only—it does not mean that a model was staged, loaded, or executed.

### STT client preflight-evidence boundary

The pure `screentra-stt-client-preflight-evidence.py` evaluator accepts only a bounded, already-collected/redacted evidence mapping. It validates the non-secret host label, supported architecture, bounded resource counts, audio/service booleans, paired model identity, local-runtime boolean, and optional camera boolean. It returns deterministic blockers and warnings as metadata-only evidence with execution, runtime-readiness, hardware-access, and model-staging flags forced false. This is evidence evaluation only—not a live preflight, readiness claim, command probe, hardware probe, server contact, or state change.

### STT runtime-admission evidence boundary

The pure `screentra-stt-runtime-admission.py` module composes the preflight, model, supervisor, resource-policy, resource-enforcement-observation, local-IPC, model-load-observation, and bounded audio-permission metadata outputs with only immutable validated result types for approval, resource, enforcement, IPC, installation, model-load, and audio-permission evidence. It binds the full service/version/activation/host/model identity tuple and exact seven-gate inventory, binds observed resource-enforcement metadata to the resource policy's exact service/activation/policy-version identity, binds IPC service/activation/socket/protocol/operation metadata across the supervisor and IPC-policy results, and binds model-load service/activation/model/digest metadata to the validated model plan and installation evidence while binding audio-permission service/activation/host/backend/source metadata and rejecting raw, arbitrary, tampered, or identity-drifting evidence. A complete all-passed record may return `admission: approved_for_future_review`, but `runtime_activation_allowed`, `execution_enabled`, `hardware_permission_activated`, `microphone_accessed`, `audio_captured`, `listener_bound`, `process_started`, `socket_bound`, `model_loaded`, `inference_started`, `resource_enforced`, `cgroup_bound`, `ipc_enforced`, and lifecycle-state changes remain false. This is a fail-closed metadata boundary, not runtime readiness or activation; it is not wired to listeners, startup, routes, installers, hardware, audio, model/image loading, inference, or remote hosts.

### STT bounded runtime/model installation evidence

The pure `screentra-stt-runtime-installation-evidence.py` contract is a separate installation-evidence slice. It binds `magic-mirror-stt`, activation `activation-20261018-01`, service version `0.1.0`, the allowlisted `whisper.cpp` runtime at version `1.9.5` and source commit `d1be6fde11ac6e0407606b4e42fe72d34add8037`, plus the installed runtime digest `d8b3d4f22d96f2a5c26cc34966927c69ab88406b52cdc7cee7c94e75bee16d97` and `whisper-tiny.en` model digest `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`. `policy_valid` and `artifacts_verified` require both artifact checks and the explicit non-starting check, while every execution, activation, process, model-load, and service-start flag remains false. Installation evidence proves only bounded artifact identity and a non-starting verification record; it is not runtime readiness and does not prove a listener, capture path, inference, model loading, service health, resource enforcement, or lifecycle activation. Runtime admission consumes only this exact immutable result type and rejects raw, arbitrary, tampered, or mismatched evidence.

### STT model integrity, staged artifact, load observation, and activation boundary

Model integrity is the read-only identity/digest verification in the local-model staging plan and preflight evidence; it proves only that a caller-supplied model identity and digest were checked. A staged artifact is the separate runtime-installation evidence record, which binds the installed runtime/model identities and a non-starting check; it does not prove that the model was loaded. The pure `screentra-stt-model-load-evidence.py` contract is a third, bounded observation slice. It accepts only service/activation/model/digest identity plus `model_file_observed`, `load_observation_non_mutating`, and `load_identity_observed`; `load_observation_valid` is true only when all three real booleans are true. Its immutable result always forces execution, runtime activation, model-loaded, inference-started, and process-started flags false and carries no readiness claim. Runtime admission consumes only this exact type and binds its complete identity to the model plan and installation evidence.

Actual model activation and inference remain a fourth, separately authorized runtime boundary. They require independent evidence for the activated model, process/service lifecycle, local IPC, resource enforcement, and audio/capture path; none is inferred from model integrity, a staged artifact, or a load observation. The current plan and admission result therefore keep execution, activation, model-loaded, inference-started, process, socket, audio, and lifecycle flags false.

### STT bounded audio-permission evidence boundary

The pure `screentra-stt-audio-permission-evidence.py` contract accepts only a
closed, bounded record bound to the service, activation, and host identities,
with `audio_backend: pipewire`, a `group_membership` or `user_service`
permission source, and four real boolean observations. `policy_valid` can be
true only when `permission_observed`, `capture_device_observed`,
`user_service_observed`, and `non_capture_check` are all true. The frozen result
always forces execution, runtime activation, hardware-permission activation,
microphone access, audio capture, and listener binding false and carries no
readiness claim. It rejects paths, URLs, commands, credentials, raw audio or
audio bytes, device-control fields, hardware-capture claims, and oversized or
identity-drifting records.

Audio-permission evidence is not permission activation: observing a permission
source or a capture device does not grant or activate microphone permission.
Permission activation remains a separate deferred gate and must have its own
explicit authorization and runtime evidence. Capture is a third, independent
boundary: no microphone or camera is accessed, no audio is captured, and no
listener or service is started by this evidence contract or by its runtime-
admission integration.

### STT future-activation approval-record boundary

The pure `screentra-stt-approval-record.py` validator accepts one bounded,
identity-bound approval record for the future client-local STT lane. Its closed
required gate inventory is `capability_admission`, `model_integrity`,
`audio_permission`, `local_runtime`, `resource_policy`, `ipc_policy`, and
`rollback_policy`; every gate must be unique, present, boolean, and carry a
bounded non-empty reason. The frozen result records approval intent/evidence
only and always reports `execution_enabled: false` and
`runtime_activation_allowed: false`. It does not claim process, model, audio,
or IPC/socket readiness and is not wired to startup, activation, hardware, or
remote hosts.

### STT resource-policy evidence boundary

The pure `screentra-stt-resource-policy-evidence.py` validator accepts only the closed service/activation identity, four bounded client-service resource values, and an explicit policy version. Its frozen metadata result uses the manifest bounds (`cpu_millis` 1..4000, `memory_bytes` 64 MiB..2 GiB, `pids` 1..256, and `writable_bytes` 0..512 MiB), while forcing `execution_enabled`, `runtime_activation_allowed`, `process_started`, and `resource_enforced` false. The pure `screentra-stt-resource-enforcement-evidence.py` validator is a separate observed-enforcement boundary, not a resource-policy declaration. It accepts only the closed service/activation/policy-version identity and five real boolean observations (`cpu_enforced`, `memory_enforced`, `pids_enforced`, `writable_enforced`, and `observation_non_mutating`). `enforcement_valid` and `policy_valid` are true only when all five observations are true, but the immutable result always forces `execution_enabled`, `runtime_activation_allowed`, `process_started`, `resource_enforced`, and `cgroup_bound` false and carries no readiness claim. It rejects arbitrary or duplicate fields, paths, URLs, commands, credentials, raw audio/model bytes, container/process claims, device-control fields, and oversized records. This is bounded metadata evidence about an observation; it does not apply or independently prove resource enforcement, process/container state, cgroup binding, or runtime readiness, and it is not wired to services, listeners, hardware, audio, model loading, or deployment.

### STT local-IPC policy evidence boundary

The pure `screentra-stt-ipc-policy-evidence.py` validator binds a closed local socket label, protocol version, codec-compatible frame/request/response limits, and only the `health`/`transcribe` operations to the service and activation identities. Its frozen result requires `local_only: true`, reports `policy_valid: true`, and forces `execution_enabled`, `runtime_activation_allowed`, `socket_bound`, and `ipc_enforced` false. IPC policy evidence is metadata-only: it is not a bound listener, transport enforcement, or transport readiness, and it performs no process, audio, credential, or network work.

### STT bounded IPC transport-readiness observation boundary

The pure `screentra-stt-ipc-readiness-evidence.py` validator is a separate observation contract, not an IPC policy declaration or listener activation. It accepts only the closed service/activation/socket/protocol/operation identity plus five real booleans: `listener_observed`, `peer_identity_observed`, `hello_observed`, `transport_non_mutating`, and `local_only`. `readiness_valid` can be true only when all five observations are true, but the immutable result always forces `execution_enabled`, `runtime_activation_allowed`, `socket_bound`, `ipc_enforced`, `listener_started`, and `peer_authenticated` false and carries no transport-ready or runtime-ready claim. It rejects arbitrary or duplicate fields, paths, URLs, commands, credentials, raw audio, ports/listener controls, peer-credential claims, socket paths, and oversized records. Runtime admission consumes only this exact immutable type and binds its complete identity to the validated IPC policy's exact service, activation, socket, protocol, and operation tuple.

This observation is metadata evidence about a bounded transport review only; it does not prove that a listener was started, a socket is currently bound, peer credentials were authenticated by the runtime, IPC is enforced, or a service is ready. Actual listener activation remains a separate explicitly authorized runtime boundary and is not part of this slice.

### STT rollback-policy evidence boundary

The pure `screentra-stt-rollback-policy-evidence.py` validator accepts one closed,
immutable service/version/activation-bound rollback policy with exactly one retained
artifact and five explicit rollback controls. It rejects unknown or duplicate fields,
secrets, paths, URLs, commands, mounts, ports, audio/model data, and oversized
records. `policy_valid` is true only when every rollback control is true; every result
still forces `execution_enabled`, `runtime_activation_allowed`, `rollback_executed`,
`process_started`, and `state_changed` false. This is metadata-only policy evidence,
not a claim that rollback occurred or that the runtime is ready, and it is not wired
to startup, listeners, routes, installers, hardware, audio, models, or deployment.

## Product scope

### Client-side AI service

- Run as a client-local, resource-limited service/container.
- Keep audio processing local by default; do not upload microphone frames.
- Start with push-to-talk transcription rather than always-on recording.
- Use a small CPU-capable model (`whisper.cpp` tiny/base or Vosk, selected after a benchmark).
- Expose only a bounded local IPC contract to the trusted player agent.
- Report service health, model version, capability state, and bounded error text.
- Support explicit start, stop, health, and update/rollback state.

### Camera widget

- Add a trusted built-in `camera` renderer rather than executable add-on JavaScript.
- Capture a local USB/UVC camera on the client; add RTSP/IP cameras only as a separate reviewed mode.
- Keep frames local to the client by default.
- Provide explicit camera on/off state and a visible privacy indicator.
- Add bounded device, resolution, frame-rate, and filter properties.
- Keep the widget compatible with interactive layouts without exposing arbitrary DOM or client process access.

### Filters

Initial filters are lightweight local effects:

- normal
- grayscale/color tint
- zombie tint/eyes/overlay
- vampire/teeth overlay
- ghost/skeleton overlay where the face-landmark path supports it

Start with face detection/landmarks plus deterministic overlays. Do not make a generative image model a deadline dependency.

## Trust and capability boundary

- Declarative manifests may describe the widget and its defaults but cannot execute code.
- The Magic Mirror client package must be signed, versioned, digest-checked, and explicitly targeted to a client capability profile.
- Camera and microphone are separate capabilities and default to disabled until an administrator enables them for a client.
- Capability policy approval/admission is evidence of an allowed declaration only; it does not activate camera or microphone permission, local inference, or a client service runtime.
- The server sends data-only configuration and named commands; it never sends shell text or arbitrary container arguments.
- The client agent validates every command again and returns structured results.
- The AI service receives only the local audio stream through a bounded IPC path; it receives no server credentials, database paths, or unrestricted host filesystem.
- The camera path receives only the selected capture device and bounded settings; it cannot open arbitrary devices.
- Network egress is disabled by default for local inference and filters.
- `EXECUTION_ENABLED` and the existing trusted server-service `database_boundary` remain disabled. This project uses a separately reviewed client-local service lane; it must not weaken the server trusted-service gates.

## Milestones

### M1 — Contract and fixtures

- Freeze client capability, service health, IPC, camera configuration, filter, and widget property contracts.
- Add manifest fixtures and negative validation tests.
- Add a local fake AI service fixture for protocol tests only; it must not be mistaken for runtime evidence.

### M2 — Client-local AI service

- Add the service source/package and bounded local IPC.
- Add model identity/integrity contract and focused read-only verification tests; model staging and runtime use remain deferred.
- Add client actor commands for service lifecycle and transcription requests.
- Define and test the pure client-service lifecycle state-transition contract; this is metadata contract evidence only, not runtime or process-execution evidence.
- Add health/capability reporting through the existing heartbeat/status path.
- Add installer artifacts without installing them on a client until the package passes focused tests.

### M3 — Camera renderer

- Add the trusted player-side camera renderer and configuration validation.
- Add local capture and privacy-state handling.
- Add editor palette registration and preview behavior.
- Add interactive-layout compatibility tests.

### M4 — Filters

- Add the lightweight filter pipeline and deterministic fixture frames.
- Add filter selection and bounded parameters.
- Verify CPU/memory/frame-rate behavior on `.56` with a connected camera.

### M5 — Test-client integration

- Perform a read-only preflight on `.56`.
- Latest read-only probe: Debian 13 amd64, 4 CPUs, approximately 7.2 GiB RAM, approximately 101 GiB free on `/`; Docker, Podman, and FFmpeg are absent; no `/dev/video*` camera exists; ALSA capture devices `/dev/snd/pcmC1D0c` and `/dev/snd/pcmC1D2c` are present; PipeWire and WirePlumber system services are inactive.
- Authorized read-only preflight on 2026-10-18 found `.56` online as `third-floor` (Debian 13 amd64, 4 CPUs, approximately 7.2 GiB RAM, approximately 101.5 GiB free on `/`). A Logitech HD Pro Webcam C920 is present as `/dev/video0` and `/dev/video1`, with PipeWire/WirePlumber user services active and both camera video and camera audio nodes visible. The `board` service account belongs to `audio`, `video`, `input`, and `render`; local renderer/admin health returned `ok`; Docker, Podman, FFmpeg, and a local model were absent. This is read-only host evidence, not camera capture, model readiness, or runtime activation; blockers remain the missing local runtime and model.
- Install only explicitly approved prerequisites and preserve rollback copies.
- Deploy to `.56` only after artifact and service checks pass.
- Verify service health, audio transcription, camera display, privacy state, filter behavior, and layout stability.

### M6 — Release gate

- Run focused tests, serial full suite, locked check/build, JavaScript/Python syntax checks, and `git diff --check`.
- Perform a physical/visual confirmation on `.56`.
- Record unsupported or unverified features honestly.
- Do not promote to other clients, enable arbitrary extension execution, or push/tag/release without explicit authorization.

## Acceptance criteria for October 30

- The add-on installs and can be enabled/disabled without executing manifest code.
- `.56` can run the client-local AI service and report health through the existing client path.
- Push-to-talk audio produces transcription locally using the selected small model, or a clear hardware/model blocker is reported.
- The Camera widget can display a connected local camera on an interactive layout.
- Camera access is visibly indicated and can be disabled.
- At least one lightweight Halloween filter works on the test client; unsupported filters fail visibly rather than silently falling back.
- Transient server/API failures do not clear the active layout or service configuration.
- No credentials, raw database access, arbitrary shell commands, unrestricted filesystem access, or arbitrary client JavaScript are introduced.
- Evidence distinguishes source tests, protocol tests, client service execution, and physical visual confirmation.

## Explicitly deferred

- Generative “make me look like a zombie” image transformation.
- Continuous always-on microphone listening.
- Unrestricted third-party camera URLs or arbitrary RTSP proxying.
- Fleet rollout and automatic installation on all clients.
- Enabling the existing trusted server-service OCI execution gate.
