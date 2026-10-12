-- Screentra Cast PostgreSQL server schema.
-- This migration is intentionally independent of the client-side SQLite schema.

CREATE TABLE IF NOT EXISTS schema_migrations (
    version text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS layout_folders (
    id text PRIMARY KEY,
    name text NOT NULL,
    parent_id text REFERENCES layout_folders(id) ON DELETE CASCADE,
    created_at text NOT NULL,
    updated_at text NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_layout_folders_parent ON layout_folders(parent_id);

CREATE TABLE IF NOT EXISTS layouts (
    id text PRIMARY KEY,
    name text NOT NULL,
    interactive boolean NOT NULL DEFAULT false,
    page_type text NOT NULL DEFAULT 'single',
    timing_mode text NOT NULL DEFAULT 'global',
    duration integer NOT NULL DEFAULT 15,
    shuffle boolean NOT NULL DEFAULT false,
    canvas_orientation text NOT NULL DEFAULT 'landscape',
    folder_id text REFERENCES layout_folders(id) ON DELETE SET NULL,
    items text NOT NULL DEFAULT '[]',
    created_at text NOT NULL,
    updated_at text NOT NULL,
    snap_grid text
);

CREATE TABLE IF NOT EXISTS widgets (
    id text PRIMARY KEY,
    layout_id text NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
    widget_type text NOT NULL,
    x double precision NOT NULL,
    y double precision NOT NULL,
    w double precision NOT NULL,
    h double precision NOT NULL,
    z integer NOT NULL DEFAULT 0,
    props text NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS display_state (
    display text PRIMARY KEY,
    layout_id text
);

CREATE TABLE IF NOT EXISTS display_overrides (
    display text PRIMARY KEY,
    width integer NOT NULL,
    height integer NOT NULL
);

CREATE TABLE IF NOT EXISTS templates (
    id text PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL DEFAULT 'general',
    is_builtin boolean NOT NULL DEFAULT false,
    layout_json text NOT NULL,
    created_at text NOT NULL,
    layout_set_id text
);

CREATE TABLE IF NOT EXISTS layout_revisions (
    layout_id text NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
    revision bigint NOT NULL,
    snapshot text NOT NULL,
    created_at text NOT NULL,
    published_by text,
    PRIMARY KEY (layout_id, revision)
);
CREATE INDEX IF NOT EXISTS idx_layout_revisions_layout ON layout_revisions(layout_id, revision DESC);

CREATE TABLE IF NOT EXISTS refresh_state (
    id integer PRIMARY KEY DEFAULT 1,
    epoch bigint NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS backups (
    id text PRIMARY KEY,
    name text NOT NULL,
    created_at text NOT NULL,
    file_path text NOT NULL,
    size_bytes bigint NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    token text PRIMARY KEY,
    username text NOT NULL,
    expires_at bigint NOT NULL,
    revoked boolean NOT NULL DEFAULT false
);

CREATE TABLE IF NOT EXISTS layout_sets (
    id text PRIMARY KEY,
    name text NOT NULL,
    shuffle boolean NOT NULL DEFAULT false,
    dwell_secs integer NOT NULL DEFAULT 30,
    created_at text NOT NULL,
    updated_at text NOT NULL
);

CREATE TABLE IF NOT EXISTS schedule_runs (
    id bigserial PRIMARY KEY,
    schedule_id text NOT NULL,
    action text NOT NULL,
    target text NOT NULL,
    success boolean NOT NULL,
    message text,
    occurred_at text NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_schedule_runs_schedule_time
    ON schedule_runs(schedule_id, occurred_at);

CREATE TABLE IF NOT EXISTS audit_events (
    id bigserial PRIMARY KEY,
    actor text,
    role text,
    event_type text NOT NULL,
    target text,
    outcome text NOT NULL,
    detail text,
    occurred_at text NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_events_occurred_at
    ON audit_events(occurred_at);

CREATE TABLE IF NOT EXISTS schedules (
    id text PRIMARY KEY,
    display text NOT NULL,
    layout_id text REFERENCES layouts(id) ON DELETE SET NULL,
    day_of_week integer NOT NULL,
    start_time text NOT NULL,
    end_time text NOT NULL,
    created_at text NOT NULL,
    layout_set_id text,
    action text NOT NULL DEFAULT 'switch_layout',
    target_client text,
    power_state text,
    updated_at text,
    display_name text,
    control_method text NOT NULL DEFAULT 'auto',
    display_index integer NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_schedules_display ON schedules(display);
CREATE INDEX IF NOT EXISTS idx_schedules_layout_set_id ON schedules(layout_set_id);

CREATE TABLE IF NOT EXISTS clients (
    id text PRIMARY KEY,
    name text NOT NULL,
    address text NOT NULL,
    role text NOT NULL DEFAULT 'player',
    version text,
    uptime_seconds bigint,
    cpu_usage double precision,
    memory_used bigint,
    memory_total bigint,
    disk_used bigint,
    disk_total bigint,
    active_layout_id text,
    assigned_layout_id text,
    last_seen text NOT NULL,
    enrolled boolean NOT NULL DEFAULT false,
    last_command_result text,
    last_command_at text,
    max_displays integer,
    installer_detected_max_displays integer,
    touch_binding text,
    theme text,
    is_virtual boolean NOT NULL DEFAULT false,
    url text,
    client_auth_token_hash text
);
CREATE INDEX IF NOT EXISTS idx_clients_last_seen ON clients(last_seen);
ALTER TABLE clients ADD COLUMN IF NOT EXISTS client_auth_token_hash text;

CREATE TABLE IF NOT EXISTS client_heartbeats (
    id bigserial PRIMARY KEY,
    client_id text NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    recorded_at text NOT NULL,
    cpu_usage double precision,
    memory_used bigint,
    memory_total bigint,
    disk_used bigint,
    disk_total bigint,
    uptime_seconds bigint
);
CREATE INDEX IF NOT EXISTS idx_client_heartbeats_client_time
    ON client_heartbeats(client_id, recorded_at);

CREATE TABLE IF NOT EXISTS layout_set_entries (
    id text PRIMARY KEY,
    layout_set_id text NOT NULL REFERENCES layout_sets(id) ON DELETE CASCADE,
    layout_id text NOT NULL REFERENCES layouts(id) ON DELETE CASCADE,
    order_index integer NOT NULL DEFAULT 0,
    interval_secs integer NOT NULL DEFAULT 30
);

CREATE TABLE IF NOT EXISTS client_displays (
    client_id text NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    display_index integer NOT NULL,
    connector text,
    label text,
    width integer,
    height integer,
    refresh_rate double precision,
    enabled boolean NOT NULL DEFAULT true,
    assigned_layout_id text,
    active_layout_id text,
    last_seen text NOT NULL,
    last_error text,
    orientation text NOT NULL DEFAULT 'normal',
    control_method text NOT NULL DEFAULT 'auto',
    PRIMARY KEY (client_id, display_index)
);
CREATE INDEX IF NOT EXISTS idx_client_displays_client ON client_displays(client_id);

CREATE TABLE IF NOT EXISTS client_commands (
    client_id text PRIMARY KEY REFERENCES clients(id) ON DELETE CASCADE,
    action text NOT NULL,
    payload text,
    requested_at text NOT NULL,
    requested_by text
);
CREATE TABLE IF NOT EXISTS client_command_cooldowns (
    client_id text NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    action text NOT NULL,
    last_requested_at text NOT NULL,
    burst_started_at text,
    burst_count integer NOT NULL DEFAULT 0,
    cooldown_until text,
    PRIMARY KEY (client_id, action)
);
CREATE INDEX IF NOT EXISTS idx_client_command_cooldowns_last_requested
    ON client_command_cooldowns(last_requested_at);

CREATE TABLE IF NOT EXISTS users (
    id text PRIMARY KEY,
    username text NOT NULL UNIQUE,
    display_name text NOT NULL,
    role text NOT NULL DEFAULT 'layout-manager',
    password_hash text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_at text NOT NULL,
    updated_at text NOT NULL,
    last_login_at text,
    google_sub text,
    google_email text
);
CREATE INDEX IF NOT EXISTS idx_users_username ON users(username);
CREATE INDEX IF NOT EXISTS idx_users_role ON users(role);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_google_sub
    ON users(google_sub) WHERE google_sub IS NOT NULL;

CREATE TABLE IF NOT EXISTS scenes (
    id text PRIMARY KEY,
    name text NOT NULL,
    description text,
    created_at text NOT NULL,
    updated_at text NOT NULL
);

CREATE TABLE IF NOT EXISTS scene_display_layouts (
    scene_id text NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    display_index integer NOT NULL,
    layout_id text REFERENCES layouts(id) ON DELETE SET NULL,
    PRIMARY KEY (scene_id, display_index)
);
CREATE INDEX IF NOT EXISTS idx_scene_display_layouts_scene
    ON scene_display_layouts(scene_id);

CREATE TABLE IF NOT EXISTS interactive_submissions (
    id bigserial PRIMARY KEY,
    client_id text NOT NULL,
    interactive_layout_id text,
    interactive_layout_name text,
    widget_id text NOT NULL,
    widget_type text NOT NULL,
    widget_label text,
    payload_json text NOT NULL,
    client_address text,
    collected_at text NOT NULL,
    received_at text NOT NULL,
    google_sync_status text NOT NULL DEFAULT 'pending',
    google_sync_message text,
    google_synced_at text,
    google_sheet_id text
);
CREATE INDEX IF NOT EXISTS idx_interactive_submissions_client
    ON interactive_submissions(client_id);
CREATE INDEX IF NOT EXISTS idx_interactive_submissions_layout
    ON interactive_submissions(interactive_layout_id);
CREATE INDEX IF NOT EXISTS idx_interactive_submissions_status
    ON interactive_submissions(google_sync_status);
CREATE INDEX IF NOT EXISTS idx_interactive_submissions_collected
    ON interactive_submissions(collected_at);

CREATE TABLE IF NOT EXISTS widget_presets (
    id text PRIMARY KEY,
    name text NOT NULL,
    widget_type text NOT NULL,
    props_json text NOT NULL,
    created_at text NOT NULL,
    updated_at text NOT NULL,
    is_builtin boolean NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS idx_widget_presets_type ON widget_presets(widget_type);

CREATE TABLE IF NOT EXISTS widget_preset_assets (
    id text PRIMARY KEY,
    preset_id text NOT NULL REFERENCES widget_presets(id) ON DELETE CASCADE,
    kind text NOT NULL,
    key text NOT NULL,
    url text NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_widget_preset_assets_preset
    ON widget_preset_assets(preset_id);

CREATE TABLE IF NOT EXISTS news_feeds (
    url text PRIMARY KEY,
    title text NOT NULL DEFAULT '',
    headlines_json text NOT NULL DEFAULT '[]',
    last_fetched_at text NOT NULL DEFAULT '',
    last_error text,
    etag text,
    last_modified text
);
CREATE INDEX IF NOT EXISTS idx_news_feeds_last_fetched
    ON news_feeds(last_fetched_at);

INSERT INTO refresh_state (id, epoch)
VALUES (1, 0)
ON CONFLICT (id) DO NOTHING;

INSERT INTO schema_migrations (version)
VALUES ('0001_initial')
ON CONFLICT (version) DO NOTHING;
