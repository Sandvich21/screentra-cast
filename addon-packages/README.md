# Standard weather and traffic add-ons

This directory contains the first declarative package contracts for independently versioned standard widgets:

- `weather-widget-standard-us-ca` — **Weather widget standard (US & Canada)**
- `traffic-standard-us-ca` — **Traffic standard (US & Canada)**

## Current phase

These manifests are intentionally `kind=declarative`. They register an add-on-owned widget alias while reusing the existing, trusted `weather` or `traffic` renderer. The `data_contract` is read-only metadata that limits the host API surface to the existing forecast, lookup, event, and map GET routes.

No uploaded server code, client JavaScript, database access, credentials, arbitrary proxying, or dashboard-page injection is enabled by these packages. The trusted-service execution gate and database boundary remain disabled.

The provider implementations still live in the core server today:

- weather fetching/normalization: `src/api.rs`
- traffic provider selection/normalization: `src/traffic.rs` and `src/api.rs`
- shared client behavior: `static/js/weather.js`, `static/js/weather-icons.js`, `static/js/traffic-runtime.js`, and `static/js/player.js`

That is deliberate. The next extraction slice must introduce a signed provider-runtime/client-extension package with a stable response schema, compatibility checks, independent update/rollback, and a real client loader before those implementations can leave the core safely.

## Regional extensions

A future regional package should use its own lower-case ID and the same versioned `data_contract`, for example `weather-standard-eu` or `weather-standard-south-america`. It must provide the same normalized weather response shape to the shared renderer or ship a separately signed client renderer. It must not broaden permissions or bypass the host API boundary.
