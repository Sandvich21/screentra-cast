# Weather and traffic add-on boundary

## Purpose

Make weather and traffic independently updateable without making the server software release carry provider changes. The first packages are:

- **Weather widget standard (US & Canada)**
- **Traffic standard (US & Canada)**

This document records the current code boundary and the safe extraction order. It does not enable add-on execution, move credentials, or authorize a deployment.

## Current contract surface

The server currently exposes the read-only routes already named by the declarative package manifests:

- `GET /api/weather/lookup` → `weather_location_lookup` (`src/api.rs`)
- `GET /api/weather/forecast` → `weather_forecast` (`src/api.rs`)
- `GET /api/traffic/events` → `traffic_events` (`src/api.rs`)
- `GET /api/traffic/map.png` → `traffic_map_image` (`src/api.rs`)

The initial manifests are metadata-only. They reuse the existing `weather` and `traffic` renderer types and do not load provider code.

## Existing ownership map

### Weather

The weather provider and normalization path are currently inside `src/api.rs`:

- location lookup and NWS geocoding response handling: `weather_location_lookup`
- NWS forecast orchestration: `weather_forecast`
- NWS phrase-to-condition normalization: `weather_code_from_nws`
- daily/hourly/alert normalization helpers: `nws_*`
- five-minute in-process cache and stale-response metadata: `WEATHER_CACHE`, `cached_weather_value`, `cache_weather_value`, `mark_weather_stale`, and `weather_response_with_meta`
- provider HTTP helper: `curl_json`

The client-side consumer is shared rather than add-on-owned today:

- live loading/fallback state: `static/js/player.js` (`ripleyLoadWeatherData`)
- weather normalization and display variables: `static/js/player.js` (`ripleyWeatherForOffset` and related render helpers)
- editor preview: `static/js/editor.js` (`_refreshWeatherEditorPreviews`)
- weather icons and alert markup: `static/js/weather-icons.js`
- standalone weather page behavior: `static/js/weather.js` and `static/weather.html`

### Traffic

Traffic provider selection and normalized event fetching are already concentrated in `src/traffic.rs`:

- `TrafficProvider` is the provider registry and regional selector.
- `TrafficQuery` is the bounded provider query input.
- provider-specific fetchers normalize into the common event object consumed by the widget.
- geographic boundary functions determine provider coverage.

The route adapter remains in `src/api.rs`:

- `traffic_events` validates coordinates/radius, reads server-side provider configuration, selects a provider, calls `TrafficProvider::fetch`, and adds common response metadata.
- `traffic_map_image` is a separate server-side static-map image proxy and should not be treated as provider-event logic.

The client-side consumer is shared today:

- live loading and unavailable-state handling: `static/js/player.js` (`ripleyLoadTrafficData`)
- event filtering and local closure aging: `static/js/traffic-runtime.js`
- editor preview: `static/js/editor.js` (`_refreshTrafficEditorPreviews`)
- event variables, repeat groups, and map markup: `static/js/player.js`

## Safe extraction order

1. **Freeze the normalized response contracts.** Add versioned Rust/JSON fixtures for weather and traffic success, stale/unavailable, configured/unconfigured, and provider coverage responses. The contract must be independent of NOAA, 511, WZDx, Google Maps, or any other upstream format.
2. **Separate pure normalization from transport.** Move weather NWS normalization helpers into a provider-neutral module first. Keep HTTP, caching, credentials, and route handlers in the core until the signed provider boundary exists. Traffic normalization is farther along because `src/traffic.rs` already owns most provider-specific behavior.
3. **Make the shared client renderer consume only the normalized contract.** Keep the existing renderer and editor behavior during the transition; add compatibility tests so a package update cannot silently change layout fields or fallback semantics.
4. **Define provider package metadata against the frozen contract.** A regional package declares its region and normalized response version. It does not receive arbitrary routes, raw database access, host filesystem access, unsigned JavaScript, or unrestricted network proxying.
5. **Implement signed provider-runtime/client-extension support separately.** Before provider code leaves the server, the platform needs signed artifact verification, compatibility checks, capability isolation, loading, update, rollback, disable/recovery, and independent approval. `client_extension` is currently reserved and is not an executable runtime.
6. **Migrate one provider at a time.** Keep the core implementation as the fallback until the signed package is verified, compatible, enabled, and producing the same normalized contract. Roll back to the core path on signature, compatibility, readiness, timeout, or provider errors.
7. **Only then remove duplicated core provider code.** Removal is a later release after runtime evidence and client-visible verification, not part of declarative package installation.

## Regional expansion

South American, European, and other regional packages should implement the same normalized weather or traffic contract while declaring a distinct bounded `region_scope`. Region selection must be explicit and fail closed: an unsupported location must return the standard unavailable response rather than selecting a provider based on a broad or guessed geographic match.

## Current safety state

- Standard packages are declarative manifests only.
- `EXECUTION_ENABLED` remains disabled.
- The database boundary remains disabled.
- No provider code is dynamically loaded.
- No credentials are issued or transported by the package contract.
- No production listener, image pull, container start, deployment, rollout, or push is part of this work.
