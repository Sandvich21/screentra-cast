# Screentra Cast v0.1.5 — Declarative Add-on Manager Stabilization

## Included

- Declarative add-on install, listing, update, enable/disable, and removal.
- Atomic manifest updates that preserve the existing lifecycle state.
- Automatic first-start installation of the standard US/Canada weather and traffic manifests.
- Standard widget registrations:
  - `weather-standard`
  - `traffic-standard`
- Removing a standard package is persistent; the startup seed marker prevents it from being silently reinstalled.
- Existing layouts continue to use the shared `weather` and `traffic` host renderers.

## Add-on manager behavior

The Add-ons panel accepts declarative manifest JSON. If the manifest ID is already installed, the operation updates it; otherwise it installs it. Updates require the URL/path ID and manifest ID to match and do not change enabled/disabled state. Existing add-ons can be enabled, disabled, or removed from the same panel.

The standard packages are metadata-only. They do not execute uploaded server code, load unsigned client JavaScript, expose database access, issue credentials, or activate trusted-service containers.

## Ownership boundary

Weather and traffic are add-on-owned widget definitions. Their manifests own the
widget IDs, labels, schemas, defaults, region contracts, and `host_renderer`
selection. The server retains only generic trusted host adapters named by that
field; it does not treat `weather` or `traffic` as base palette entries. Legacy
manifests using `renderer_type` remain readable, but newly serialized manifests
use `host_renderer` to make the boundary explicit.


The widget definitions, aliases, region scope, and data contracts are independently updateable. Weather and traffic provider/rendering implementation remains a shared core host service for this release. Signed provider runtime loading, regional executable providers, rollback, and deeper hardening are post-release work.

## Verification

- 385 tests: 379 passed, 0 failed, 6 ignored.
- `cargo check --locked` passed.
- `cargo build --release --locked` passed.
- JavaScript syntax checks passed.
- `git diff --check` passed.
- Dedicated display verification on test client `11.1.1.30` remains pending because the host was unreachable during the release-preparation preflight.
