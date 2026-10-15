CREATE TABLE IF NOT EXISTS addon_storage (
    addon_id      TEXT NOT NULL,
    storage_key   TEXT NOT NULL,
    value_json    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (addon_id, storage_key)
);

CREATE INDEX IF NOT EXISTS idx_addon_storage_updated_at
    ON addon_storage(updated_at);
