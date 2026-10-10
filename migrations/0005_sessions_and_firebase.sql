-- Server-side sessions (expiring, revocable) and Firebase Authentication identities.

ALTER TABLE users ADD COLUMN firebase_uid TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_firebase_uid ON users(firebase_uid) WHERE firebase_uid IS NOT NULL;

CREATE TABLE IF NOT EXISTS sessions (
    -- SHA-256 of the random token in the cookie; the token itself is never stored.
    id_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
