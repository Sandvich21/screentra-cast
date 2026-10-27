ALTER TABLE client_heartbeats
    ADD COLUMN IF NOT EXISTS cpu_temp double precision;
