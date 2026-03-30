CREATE TABLE local_user (
    user_id INTEGER PRIMARY KEY,
    password_hash TEXT NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    FOREIGN KEY (user_id) REFERENCES user(id)
);
