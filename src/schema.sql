CREATE TABLE IF NOT EXISTS nonces (
    nonce TEXT PRIMARY KEY NOT NULL,
    difficulty INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at INTEGER
);

CREATE INDEX IF NOT EXISTS nonces_expires_at ON nonces (expires_at);

CREATE TABLE IF NOT EXISTS submissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    nonce TEXT NOT NULL,
    data TEXT NOT NULL,
    submitted_at INTEGER NOT NULL
);
