-- Persist the bounded action to run when a schedule's active window ends.
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS end_action text NOT NULL DEFAULT 'none';
CREATE TABLE IF NOT EXISTS schedule_runtime_state (
    schedule_id text PRIMARY KEY,
    active boolean NOT NULL DEFAULT false,
    updated_at text NOT NULL
);
