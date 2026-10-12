# Screentra Cast operational observability

The server exposes these unauthenticated probe endpoints:

- `GET /api/system/liveness` — process is accepting requests; no database query.
- `GET /api/system/readiness` — database query succeeds; returns HTTP 503 otherwise.
- `GET /api/system/health` — database health, stale-client count, server time, and version.
- `GET /metrics` — Prometheus text format.

Every response includes `X-Request-ID`. Clients may provide a safe request ID using
that header; otherwise the server generates a UUID. The server logs one structured
key/value completion line per request with method, path, status, duration, and ID.
`RUST_LOG` controls verbosity and defaults to `info`.

The Prometheus example assumes the server is reachable at `127.0.0.1:3000`.
Copy `prometheus.yml` and `screentra-cast-alerts.yml` into a Prometheus installation
and adjust the target for a remote scrape or HTTPS listener. The metrics endpoint
contains counters and gauges only; it does not expose credentials, cookies, request
bodies, query strings, or database connection details.
