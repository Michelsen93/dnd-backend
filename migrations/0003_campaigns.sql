CREATE TABLE IF NOT EXISTS campaigns (
    id TEXT PRIMARY KEY,
    owner_user_id TEXT NOT NULL,
    name TEXT NOT NULL,
    invite_code TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (owner_user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_campaigns_owner_user_id ON campaigns(owner_user_id);
CREATE INDEX IF NOT EXISTS idx_campaigns_invite_code ON campaigns(invite_code);

CREATE TABLE IF NOT EXISTS campaign_members (
    id TEXT PRIMARY KEY,
    campaign_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    character_id TEXT NOT NULL,
    character_name TEXT NOT NULL,
    sprite_key TEXT NOT NULL,
    joined_at TEXT NOT NULL,
    FOREIGN KEY (campaign_id) REFERENCES campaigns(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    UNIQUE (campaign_id, character_id)
);
CREATE INDEX IF NOT EXISTS idx_campaign_members_campaign_id ON campaign_members(campaign_id);
CREATE INDEX IF NOT EXISTS idx_campaign_members_user_id ON campaign_members(user_id);
