ALTER TABLE client_displays ADD COLUMN IF NOT EXISTS output_scale double precision NOT NULL DEFAULT 1.0;
