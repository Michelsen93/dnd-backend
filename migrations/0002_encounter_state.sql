CREATE TABLE IF NOT EXISTS encounter_states (
    user_id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_encounter_states_updated_at ON encounter_states(updated_at DESC);
