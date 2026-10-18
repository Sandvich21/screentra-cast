# Magic Mirror Add-on — October 30, 2026 Delivery Plan

## Goal

Deliver a bounded Magic Mirror add-on for a designated test client first, with local speech-to-text, a Camera widget, and lightweight Halloween filters. Promote only after the client-visible behavior is verified.

The first target is client `11.1.1.56`; the server remains `.25`. No fleet rollout is included until the test client passes.

## Implementation status (2026-10-18)

The metadata-only trusted add-on lane now reports bounded client capabilities and service health, carries inert client-service status/rollback requests through the existing closed command path, and includes a built-in Camera widget with opt-in browser-local capture plus fixed local filters (`none`, `grayscale`, `sepia`, `halloween`). The actual client supervisor, speech-to-text service, model staging, camera hardware verification, and filter inference remain deferred and execution-disabled.

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
- Add model staging and integrity checks.
- Add client actor commands for service lifecycle and transcription requests.
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
