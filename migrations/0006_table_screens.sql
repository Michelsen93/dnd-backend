-- A TV or shared screen paired to a campaign: it shows what the whole party can see, read-only.
-- The DM creates a short pairing code; the screen exchanges it for a long random token
-- (stored hashed, like user sessions).
CREATE TABLE IF NOT EXISTS screen_codes (
    code TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS table_screens (
    id_hash TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_table_screens_campaign ON table_screens(campaign_id);
