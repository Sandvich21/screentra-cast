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

The pure `screentra-stt-runtime-admission.py` module composes the three existing metadata outputs with only the immutable `STTApprovalRecord` result from `screentra-stt-approval-record.py`. It binds the full service/version/activation/host/model identity tuple and the exact seven-gate inventory, while rejecting raw or arbitrary approval mappings and identity drift. A complete all-passed record may return `admission: approved_for_future_review`, but `runtime_activation_allowed`, `execution_enabled`, `process_started`, `socket_bound`, `model_loaded`, and lifecycle-state changes remain false. This is a fail-closed metadata boundary, not runtime readiness or activation.

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

The pure `screentra-stt-resource-policy-evidence.py` validator accepts only the closed service/activation identity, four bounded client-service resource values, and an explicit policy version. Its frozen metadata result uses the manifest bounds (`cpu_millis` 1..4000, `memory_bytes` 64 MiB..2 GiB, `pids` 1..256, and `writable_bytes` 0..512 MiB), while forcing `execution_enabled`, `runtime_activation_allowed`, `process_started`, and `resource_enforced` false. Resource-policy evidence describes reviewed bounds; it is not resource enforcement and does not claim a process or container is running. The contract is not wired to heartbeat, installers, startup, deployment, hardware/audio, model/image loading, or listeners.

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
