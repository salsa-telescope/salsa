CREATE TABLE guest_session (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id          INTEGER NOT NULL,
    telescope_id     TEXT NOT NULL,
    started_at       INTEGER NOT NULL,
    ended_at         INTEGER,
    last_activity_at INTEGER NOT NULL,
    end_reason       TEXT,
    country          TEXT,
    FOREIGN KEY (user_id) REFERENCES user(id)
);
CREATE INDEX idx_guest_session_active  ON guest_session(telescope_id, ended_at);
CREATE INDEX idx_guest_session_started ON guest_session(started_at);
