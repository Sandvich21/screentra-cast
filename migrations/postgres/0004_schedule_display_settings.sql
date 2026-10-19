-- Add schedule fields introduced after the original runtime-column migration.
-- This is separate from 0002 because deployed databases may already have
-- recorded 0002 as applied.
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS brightness integer;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS contrast integer;
