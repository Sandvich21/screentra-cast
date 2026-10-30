ALTER TABLE client_displays ADD COLUMN IF NOT EXISTS display_key text;
ALTER TABLE client_displays ADD COLUMN IF NOT EXISTS gpu text;
ALTER TABLE client_displays ADD COLUMN IF NOT EXISTS gpu_driver text;
ALTER TABLE client_displays ADD COLUMN IF NOT EXISTS drm_device text;

CREATE TABLE IF NOT EXISTS client_display_blacklist (
    client_id text NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    display_key text NOT NULL,
    PRIMARY KEY (client_id, display_key)
);
CREATE INDEX IF NOT EXISTS idx_client_display_blacklist_client
    ON client_display_blacklist(client_id);
