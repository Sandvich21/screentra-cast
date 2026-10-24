use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedPrincipal {
    pub username: String,
    pub role: String,
}

use crate::auth::{extract_token, AuthState};

/// Routes that stay open without auth (player + telemetry + auth itself).
/// Anything outside this allowlist requires a valid session or bearer token.
pub const OPEN_ROUTES: &[&str] = &[
    "/api/auth/login",
    "/api/auth/status",
    "/api/setup/status",
    "/api/setup",
    "/api/clients/register",
    "/api/log",

    "/api/system/time",
    "/api/system/liveness",
    "/api/system/readiness",
    "/metrics",
    "/api/interactive/submit",
    // 2026-08-11: /api/ics-proxy is open so the calendar widget iframe
    // (no auth token) can fetch external ICS feeds. SSRF hardening is
    // still enforced by validate_proxy_url (no localhost/private IPs).
    "/api/ics-proxy",
    "/api/weather/forecast",
    "/api/traffic/events",
    "/api/traffic/map.png",
    // Open download routes for anonymous installer access
    "/install/screentra-cast-installers.tar.gz",
    "/install/player-client.sh",
    "/install/player-client.sh.sig",
];

/// Method+path prefixes that are always open even though the prefix overlaps
/// with the gated namespace (e.g. config fetches by enrolled players).
fn is_open_path(path: &str, method: &str) -> bool {
    // The static fallback serves the login page, player pages, stylesheets,
    // scripts, and bundled assets before a session exists. Keep API, client,
    // upload, and install namespaces behind their explicit policies.
    if method == "GET"
        && !path.starts_with("/api/")
        && !path.starts_with("/clients/")
        && !path.starts_with("/uploads/")
        && !path.starts_with("/install/")
    {
        return true;
    }
    if OPEN_ROUTES.contains(&path) {
        let public_method = match path {
            "/api/auth/login" | "/api/setup" | "/api/clients/register" | "/api/log" | "/api/interactive/submit" => method == "POST",
            "/api/auth/status" | "/api/setup/status" | "/api/system" | "/api/system/time" | "/api/system/liveness" | "/api/system/readiness" | "/metrics" | "/api/ics-proxy" | "/api/weather/forecast" | "/api/traffic/events" | "/api/traffic/map.png" | "/install/screentra-cast-installers.tar.gz" | "/install/player-client.sh" | "/install/player-client.sh.sig" => method == "GET",
            _ => false,
        };
        if public_method {
            return true;
        }
    }
    if (path == "/api/auth/google/login-url" || path == "/api/auth/google/callback")
        && method == "GET"
    {
        return true;
    }

    if path == "/api/setup/" && method == "POST" {
        return true;
    }

    // open so the player UI can mark itself enrolled without a session.
    if method == "POST" && path.starts_with("/api/clients/") && path.ends_with("/enroll") {
        return true;
    }
    // OPTIONS pre-flight for that endpoint.
    if method == "OPTIONS" && path.starts_with("/api/clients/") && path.ends_with("/enroll") {
        return true;
    }
    if method == "POST"
        && path.starts_with("/clients/")
        && path.ends_with("/screenshot")
    {
        return true;
    }
    // Client command acknowledgements authenticate in the handler with the
    // enrolled per-client credential rather than an admin session.
    if method == "POST"
        && path.starts_with("/api/clients/")
        && path.ends_with("/command/ack")
    {
        return true;
    }
    if method == "POST"
        && path.starts_with("/api/clients/")
        && path.ends_with("/addons/status")
    {
        return true;
    }
    // Player config fetches (read-only) are open so enrolled players still work
    // without a session. Player writes (assign layout, set command) stay gated.
    if method == "GET" && (path == "/uploads" || path.starts_with("/uploads/")) {
        return true;
    }
    if method == "GET" {
        if path.starts_with("/api/clients/") && path.ends_with("/config") {
            return true;
        }
        // The addon inventory is read by enrolled players; the handler still
        // requires the per-client credential or an admin session.
        if path.starts_with("/api/clients/") && path.ends_with("/addons") {
            return true;
        }
        if path.starts_with("/api/clients/") && path.ends_with("/package") {
            return true;
        }
        // The privileged client updater authenticates this read with the
        // enrolled per-client token in the handler.
        if path.starts_with("/api/clients/") && path.ends_with("/update-check") {
            return true;
        }
        if path.starts_with("/api/clients/") && path.ends_with("/admin-users") {
            return true;
        }
        if path.starts_with("/api/clients/")
            && path.ends_with("/displays")
            && !path.contains("/displays/bulk-assign")
        {
            return true;
        }
        if let Some(rest) = path.strip_prefix("/api/clients/") {
            // /api/clients/{id}/displays/{index}/config — display-scoped config fetches
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() == 4 && parts[1] == "displays" && parts[3] == "config" {
                return true;
            }
        }
        // GET /api/clients/{id}/virtual — fetch virtual client info
        if method == "GET" && path.contains("/virtual") && path.split("/virtual").count() == 2 && path.ends_with("/virtual") {
            return true;
        }
        // PATCH /api/clients/{id}/virtual — update virtual client name
        if method == "PATCH" && path.contains("/virtual") && path.split("/virtual").count() == 2 && path.ends_with("/virtual") {
            return true;
        }
        // Player heartbeat reads (no such endpoint yet; placeholder).

        // Player command poll (read-only — clients pull to see if they have
        // a pending nudge/layout-assignment/etc. Idempotent and safe to
        // expose without auth since no state leaks.
        if path.starts_with("/api/clients/") && path.ends_with("/command") {
            return true;
        }
        // GET status, list templates, list layouts, list schedules etc. are read-only
        // — these remain open so a static page can still render a login redirect.
        if method == "GET" {
            // Player-facing read endpoints. Referenced by player.html (the
            // browser target on physical display devices) where the browser
            // has no auth cookie. Writes (PUT/POST/DELETE) stay gated.
            if path == "/api/displays" {
                return true;
            }
            if path == "/api/display-state" || path.starts_with("/api/display-state/") {
                return true;
            }
            // Layout fetches by enrolled players — player.js hits
            // /api/layouts/<id> for the assigned layout. Read-only, returns
            // widget geometry + template metadata. Writes (POST/PUT/DELETE)
            // stay gated.
            if path.ends_with("/propagation") {
                return false;
            }
            if path.starts_with("/api/layouts/") {
                return true;
            }
            // Widget preset reads — actor pre-fetches /api/widget-presets/<id>
            // for any weather widget that references a preset UUID by its
            // template id, and inlines template_elements into the local
            // assigned-layout cache. Read-only, returns template geometry +
            // assets. Writes (POST/PUT/DELETE) stay gated.
            if path.starts_with("/api/widget-presets/") {
                return true;
            }
            // Read-only file browsing so the UI file picker can render before login.
            // Writes (POST/DELETE) stay gated.

            // /api/system/health — health probe for external monitoring
            // (Uptime Kuma, Prometheus blackbox, etc.). No secrets leak;
            // returns counts + version only.
            if path == "/api/system/health" {
                return true;
            }
            if path == "/api/weather/lookup" {
                return true;
            }
            // News headlines are read by the player's news-template.html
            // iframe and by other internal consumers; gating them with
            // auth would break the iframe (no token available there).
            if path == "/api/news/headlines" {
                return true;
            }
            // Installed font list is read by the player runtime so it
            // can @font-face every woff2 the layouts reference. The
            // data is just a list of {family, weight, url} — not
            // sensitive — and the player is unauthenticated.
            if path == "/api/fonts/installed" {
                return true;
            }
            // Preview-status returns woff2 URLs for Google Font
            // families the picker wants to render. Same story: the
            // font-picker iframe is unauthenticated and the URLs
            // are same-origin /uploads/ paths that anyone can hit.
            if path == "/api/fonts/preview-status" {
                return true;
            }
            if path == "/api/fonts/full-preview-css" {
                // Same story: the runtime fetches this on every
                // page load to inject @font-face for installed
                // fonts. The URLs are same-origin /uploads/ paths
                // that anyone can hit anyway.
                return true;
            }
        }
    }
    false
}

fn is_unsafe_method(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

fn has_bearer_authorization(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("Bearer "))
        .unwrap_or(false)
}

fn required_capability(path: &str, method: &str) -> &'static str {
    if path == "/api/auth/step-up" || path == "/api/auth/logout" || path.starts_with("/api/account") {
        return "viewer";
    }
    if path.starts_with("/api/users") {
        return "manage_users";
    }
    if path.starts_with("/api/backups") {
        return "manage_backups";
    }
    if path == "/api/audit" {
        return "view_audit";
    }
    if path == "/api/version/update/export"
        || path == "/api/version/update/import"
        || path == "/api/version/update/apply"
        || path == "/api/version/update/github/settings"
        || path == "/api/version/export"
    {
        return "server_update";
    }
    if path.starts_with("/api/version/update/")
        || path == "/api/version/restart"
        || path.starts_with("/api/version/import")
        || path.starts_with("/api/version/player-client-bundle/")
        || path.starts_with("/api/version/server-bundle/")
    {
        return "server_admin";
    }
    if path.starts_with("/api/interactive/submissions") {
        return "manage_submissions";
    }
    if path.starts_with("/api/layouts/") && path.ends_with("/publish") && method == "POST" {
        return "publish_layout";
    }
    if path.starts_with("/api/layouts")
        || path.starts_with("/api/layout-folders")
        || path.starts_with("/api/templates")
        || path.starts_with("/api/scenes")
        || path.starts_with("/api/layout-sets")
    {
        return if is_unsafe_method(method) {
            "manage_layouts"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/widget-presets") {
        return if path.ends_with("/export") && method == "GET" {
            "manage_presets"
        } else if is_unsafe_method(method) {
            "manage_presets"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/display-overrides") {
        return if is_unsafe_method(method) {
            "manage_layouts"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/files") {
        return if is_unsafe_method(method) {
            "manage_files"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/fonts") {
        return if is_unsafe_method(method) {
            "manage_fonts"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/schedules") {
        return if is_unsafe_method(method) {
            "manage_schedules"
        } else {
            "viewer"
        };
    }
    if path == "/api/display-state" || path.starts_with("/api/display-state/") {
        return if is_unsafe_method(method) {
            "control_displays"
        } else {
            "viewer"
        };
    }
    if path.starts_with("/api/clients") {
        if !is_unsafe_method(method) {
            return "viewer";
        }
        if path == "/api/clients/virtual" && method == "POST" {
            return "manage_clients";
        }
        if path.ends_with("/adopt") && method == "POST" {
            return "manage_clients";
        }
        if path.ends_with("/history") && method == "DELETE" {
            return "manage_clients";
        }
        let client_parts: Vec<&str> = path
            .strip_prefix("/api/clients/")
            .unwrap_or("")
            .split('/')
            .collect();
        if client_parts.len() == 1 && method == "DELETE" {
            return "manage_clients";
        }
        if path.ends_with("/virtual") && method == "DELETE" {
            return "manage_clients";
        }
        if path.ends_with("/assignment")
            || path.ends_with("/displays/bulk-assign")
            || path.ends_with("/displays/layout")
            || path == "/api/clients/assign-bulk"
        {
            return "assign_clients";
        }
        return "control_clients";
    }
    if path.starts_with("/api/addons/trusted-service") {
        return "manage_addons";
    }
    if path == "/api/addons" || path.starts_with("/api/addons/") {
        return if is_unsafe_method(method) {
            "manage_addons"
        } else {
            "viewer"
        };
    }
    if path == "/api/refresh" || path == "/api/clear-cache" || path == "/api/news/refresh" {
        return if is_unsafe_method(method) {
            "control_system"
        } else {
            "viewer"
        };
    }
    if path == "/api/system/time" && is_unsafe_method(method) {
        return "control_system";
    }
    if is_unsafe_method(method) {
        // Fail closed for any new state-changing endpoint until it receives
        // an explicit capability assignment.
        return "server_admin";
    }
    "viewer"
}

fn role_has_capability(role: &str, capability: &str) -> bool {
    if role == "admin" {
        return true;
    }
    match role {
        "layout-manager" => matches!(
            capability,
            "viewer"
                | "manage_layouts"
                | "publish_layout"
                | "manage_files"
                | "manage_fonts"
                | "manage_presets"
        ),
        "operator" => matches!(
            capability,
            "viewer"
                | "control_clients"
                | "assign_clients"
                | "control_displays"
                | "manage_schedules"
                | "control_system"
        ),
        "viewer" => capability == "viewer",
        _ => false,
    }
}
fn required_step_up_scope(path: &str, method: &str) -> Option<String> {
    if path.starts_with("/api/addons/execution/approvals") && is_unsafe_method(method) {
        return Some("addon-execution-approval".to_string());
    }
    if path == "/api/version/export"
        || path == "/api/version/update/export"
        || path == "/api/version/update/import"
        || path == "/api/version/update/apply"
        || path == "/api/version/update/github/settings" {
        return Some("server-update".to_string());
    }
    // Client commands and display assignments are already protected by the
    // authenticated dashboard session, role capabilities, client enrollment,
    // and the server-side command allowlist. They intentionally do not use a
    // second step-up prompt.
    None
}

pub async fn require_auth(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    if is_open_path(&path, method.as_str()) {
        return Ok(next.run(req).await);
    }
    let headers = req.headers();
    let token = extract_token(headers)
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    let payload = auth
        .token_payload(&token)
        .await
        .ok_or((StatusCode::UNAUTHORIZED, "auth required".to_string()))?;
    if is_unsafe_method(method.as_str())
        && !has_bearer_authorization(headers)
        && headers.contains_key(axum::http::header::COOKIE)
    {
        crate::auth::csrf_check_state_change(headers)?;
    }
    let actual_role = payload
        .get("role")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let username = payload
        .get("sub")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let capability = required_capability(&path, method.as_str());
    if !role_has_capability(actual_role, capability) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{capability} capability required"),
        ));
    }
    if let Some(scope) = required_step_up_scope(&path, method.as_str()) {
        let valid = crate::auth::extract_step_up_token(headers)
            .map(|step_up| auth.verify_step_up(&step_up, &username, &scope))
            .unwrap_or(false);
        if !valid {
            return Err((
                StatusCode::FORBIDDEN,
                "step-up authentication required".to_string(),
            ));
        }
    }
    let mut req = req;
    req.extensions_mut().insert(AuthenticatedPrincipal {
        username: username.clone(),
        role: actual_role.to_string(),
    });
    let mut response = next.run(req).await;
    if let Some(scope) = required_step_up_scope(&path, method.as_str()) {
        let cookie = auth.step_up_cookie_value(&username, &scope);
        if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
            response
                .headers_mut()
                .append(axum::http::header::SET_COOKIE, value);
        }
    }
    Ok(response)
}

#[cfg(test)]
mod authorization_tests {
    use super::*;

    #[test]
    fn static_gets_are_open_but_static_mutations_are_not() {
        assert!(is_open_path("/", "GET"));
        assert!(is_open_path("/index.html", "GET"));
        assert!(is_open_path("/player.html", "GET"));
        assert!(is_open_path("/css/main.css", "GET"));
        assert!(is_open_path("/js/app.js", "GET"));
        assert!(!is_open_path("/index.html", "POST"));
        assert!(!is_open_path("/js/app.js", "DELETE"));
        assert!(!is_open_path("/api/layouts", "GET"));
        assert!(is_open_path("/install/player-client.sh", "GET"));
        assert!(is_open_path("/install/player-client.sh.sig", "GET"));
        assert!(!is_open_path("/install/unknown.sh", "GET"));
    }

    #[test]
    fn capabilities_are_scoped_by_operation() {
        assert!(role_has_capability("viewer", "viewer"));
        assert!(!role_has_capability("viewer", "control_clients"));
        assert!(role_has_capability("operator", "control_clients"));
        assert!(role_has_capability("operator", "assign_clients"));
        assert!(!role_has_capability("operator", "manage_layouts"));
        assert!(role_has_capability("layout-manager", "manage_layouts"));
        assert!(role_has_capability("layout-manager", "publish_layout"));
        assert!(!role_has_capability("layout-manager", "control_clients"));
        assert!(role_has_capability("admin", "manage_users"));
        assert!(role_has_capability("admin", "server_update"));
    }

    #[test]
    fn state_export_requires_server_update_step_up() {
        assert_eq!(required_capability("/api/version/export", "GET"), "server_update");
        assert_eq!(required_step_up_scope("/api/version/export", "GET").as_deref(), Some("server-update"));
        assert_eq!(required_step_up_scope("/api/version/update/export", "GET").as_deref(), Some("server-update"));
    }

    #[test]
    fn client_command_poll_is_open_only_for_get() {
        assert!(is_open_path("/api/clients/example/command", "GET"));
        assert!(is_open_path("/api/clients/example/addons", "GET"));
        assert!(!is_open_path("/api/clients/example/command", "DELETE"));
        assert!(!is_open_path("/api/clients/example/displays/0/command", "DELETE"));
        assert!(is_open_path("/api/clients/example/command/ack", "POST"));
        assert!(!is_open_path("/api/clients/example/command/ack", "GET"));
        assert!(is_open_path("/clients/example/screenshot", "POST"));
        assert!(!is_open_path("/clients/example/screenshot", "GET"));
    }

    #[test]
    fn traffic_events_is_open_only_for_get() {
        assert!(is_open_path("/api/traffic/events", "GET"));
        assert!(!is_open_path("/api/traffic/events", "POST"));
        assert!(!is_open_path("/api/traffic/events", "PUT"));
        assert!(!is_open_path("/api/traffic/events", "PATCH"));
        assert!(!is_open_path("/api/traffic/events", "DELETE"));
    }

    #[test]
    fn traffic_map_is_open_only_for_get() {
        assert!(is_open_path("/api/traffic/map.png", "GET"));
        assert!(!is_open_path("/api/traffic/map.png", "POST"));
        assert!(!is_open_path("/api/traffic/map.png", "PUT"));
        assert!(!is_open_path("/api/traffic/map.png", "DELETE"));
    }

    #[test]
    fn capability_policy_is_explicit_and_fails_closed() {
        assert_eq!(required_capability("/api/new-state-changing-route", "POST"), "server_admin");
        assert_eq!(required_capability("/api/layouts/one", "PUT"), "manage_layouts");
        assert_eq!(required_capability("/api/layouts/one/publish", "POST"), "publish_layout");
        assert_eq!(required_capability("/api/clients/one/nudge", "POST"), "control_clients");
        assert_eq!(required_capability("/api/clients/one/assignment", "PUT"), "assign_clients");
        assert_eq!(required_capability("/api/backups", "GET"), "manage_backups");
        assert_eq!(required_capability("/api/version/update/import", "POST"), "server_update");
        assert_eq!(required_capability("/api/version/export", "GET"), "server_update");
        assert_eq!(required_capability("/api/audit", "GET"), "view_audit");
        assert_eq!(required_capability("/api/layouts/one", "GET"), "viewer");
        assert_eq!(required_capability("/api/clients/one", "DELETE"), "manage_clients");
        assert_eq!(required_capability("/api/clients/virtual", "POST"), "manage_clients");
        assert_eq!(required_capability("/api/display-overrides/x", "DELETE"), "manage_layouts");
        assert!(!role_has_capability("operator", "manage_clients"));
        assert!(!role_has_capability("layout-manager", "manage_clients"));
        assert!(!is_open_path("/api/system/time", "POST"));
        assert!(is_open_path("/api/system/time", "GET"));
        assert!(!is_open_path("/api/clients/virtual", "POST"));
    }

    #[test]
    fn addon_storage_routes_require_authenticated_addon_management_capability() {
        assert_eq!(
            required_capability("/api/addons/example.storage/storage/settings", "GET"),
            "viewer"
        );
        assert_eq!(
            required_capability("/api/addons/example.storage/storage/settings", "PUT"),
            "manage_addons"
        );
        assert_eq!(
            required_capability("/api/addons/example.storage/storage/settings", "DELETE"),
            "manage_addons"
        );
        assert_eq!(
            required_capability("/api/addons/trusted-service/staged", "GET"),
            "manage_addons"
        );
        assert_eq!(
            required_capability("/api/addons/trusted-service/staged/example", "DELETE"),
            "manage_addons"
        );
        assert_eq!(
            required_capability("/api/addons/weather-widget-standard-us-ca", "GET"),
            "viewer"
        );
        assert_eq!(
            required_capability("/api/addons/weather-widget-standard-us-ca", "PUT"),
            "manage_addons"
        );
        assert_eq!(
            required_capability("/api/addons/weather-widget-standard-us-ca", "DELETE"),
            "manage_addons"
        );
        assert_eq!(required_capability("/api/addons/install", "POST"), "manage_addons");
        assert_eq!(required_capability("/api/addons", "GET"), "viewer");
        assert_eq!(required_capability("/api/addons", "POST"), "manage_addons");
        assert!(!role_has_capability("viewer", "manage_addons"));
        assert!(!role_has_capability("operator", "manage_addons"));
    }

    #[test]
    fn step_up_scopes_cover_high_impact_operations() {
        assert_eq!(required_step_up_scope("/api/version/update/import", "POST"), Some("server-update".to_string()));
        assert_eq!(required_step_up_scope("/api/version/update/apply", "POST"), Some("server-update".to_string()));
        assert_eq!(required_step_up_scope("/api/version/update/github/settings", "POST"), Some("server-update".to_string()));
        assert_eq!(required_capability("/api/version/update/apply", "POST"), "server_update");
        assert_eq!(required_capability("/api/version/update/github", "GET"), "server_admin");
        assert_eq!(required_step_up_scope("/api/clients/one/displays/0/layout", "POST"), None);
        assert_eq!(required_step_up_scope("/api/layouts/one/publish", "POST"), None);
        assert_eq!(required_step_up_scope("/api/addons/execution/approvals", "GET"), None);
        assert_eq!(
            required_step_up_scope("/api/addons/execution/approvals/example/approve", "POST"),
            Some("addon-execution-approval".to_string())
        );
        assert_eq!(
            required_step_up_scope(
                "/api/addons/execution/approvals/example/targets/client/approve",
                "POST",
            ),
            Some("addon-execution-approval".to_string())
        );
        assert_eq!(required_step_up_scope("/api/display-state", "POST"), None);
        assert_eq!(required_step_up_scope("/api/layouts/one", "PUT"), None);
        assert_eq!(required_step_up_scope("/api/clients/register", "POST"), None);
        assert_eq!(required_step_up_scope("/api/clients/one/assignment", "PUT"), None);
    }
}

pub fn spawn_gc(auth: AuthState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            auth.gc_revocations();
        }
    });
}

// Placeholder for future fine-grained roles. Not used yet.
#[allow(dead_code)]
pub struct RoleSet {
    pub revoked: Arc<RwLock<HashSet<String>>>,
}
