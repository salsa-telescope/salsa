PRAGMA foreign_keys = OFF;

-- Create identity table: one row per (user, provider) pair
CREATE TABLE user_identity (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    external_id TEXT NOT NULL,
    UNIQUE(provider, external_id)
);

-- Migrate existing identities from user table
INSERT INTO user_identity (user_id, provider, external_id)
SELECT id, provider, external_id FROM user
WHERE provider IS NOT NULL AND external_id IS NOT NULL;

-- Recreate user table without provider/external_id columns
CREATE TABLE new_user (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL
);
INSERT INTO new_user SELECT id, username FROM user;
DROP TABLE user;
ALTER TABLE new_user RENAME TO user;

-- Add link_user_id to pending_oauth2 so we can track link-vs-login intent
ALTER TABLE pending_oauth2 ADD COLUMN link_user_id INTEGER REFERENCES user(id);

PRAGMA foreign_keys = ON;
