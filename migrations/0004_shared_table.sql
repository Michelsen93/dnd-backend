-- Campaign-scoped shared table: encounters, prep content, live state, sessions and the event feed.

CREATE TABLE IF NOT EXISTS encounters (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_encounters_campaign_id ON encounters(campaign_id);

CREATE TABLE IF NOT EXISTS campaign_entities (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    revealed INTEGER NOT NULL DEFAULT 0,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_campaign_entities_campaign_id ON campaign_entities(campaign_id);

CREATE TABLE IF NOT EXISTS table_states (
    campaign_id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS game_sessions (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    number INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    payload TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_game_sessions_campaign_id ON game_sessions(campaign_id);

CREATE TABLE IF NOT EXISTS campaign_events (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    session_id TEXT,
    actor_user_id TEXT,
    kind TEXT NOT NULL,
    visibility TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_campaign_events_campaign_created ON campaign_events(campaign_id, created_at);

-- Move the old per-user encounter blobs into per-campaign rows (only for campaigns the user owns).
INSERT OR IGNORE INTO encounters (id, campaign_id, payload, created_at, updated_at)
SELECT json_extract(e.value, '$.id'),
       json_extract(e.value, '$.campaignId'),
       e.value,
       COALESCE(json_extract(e.value, '$.createdAt'), s.updated_at),
       s.updated_at
FROM encounter_states s, json_each(s.payload, '$.encounters') e
WHERE json_extract(e.value, '$.id') IS NOT NULL
  AND json_extract(e.value, '$.campaignId') IN (SELECT id FROM campaigns WHERE owner_user_id = s.user_id);

DROP TABLE IF EXISTS encounter_states;
DROP TABLE IF EXISTS combat_entries;
DROP TABLE IF EXISTS combat_sessions;
