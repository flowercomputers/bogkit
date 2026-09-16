CREATE TABLE IF NOT EXISTS bogs (
 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
 template TEXT NOT NULL CHECK(template='records-v1'),
 desired_state TEXT NOT NULL CHECK(desired_state IN ('running','stopped')),
 observed_state TEXT NOT NULL CHECK(observed_state IN ('creating','ready','stopped','failed','restoring','maintenance')),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0),
 failure_code TEXT, created_at INTEGER NOT NULL,
 startup_nonce TEXT
);
CREATE TABLE IF NOT EXISTS create_requests (
 request_key TEXT PRIMARY KEY, body_hash BLOB NOT NULL,
 bog_id TEXT NOT NULL REFERENCES bogs(id)
);
CREATE TABLE IF NOT EXISTS tokens (
 id TEXT PRIMARY KEY, bog_id TEXT NOT NULL REFERENCES bogs(id),
 secret_hash BLOB NOT NULL UNIQUE,
 scope TEXT NOT NULL CHECK(scope IN ('read','write')),
 revoked_at INTEGER, created_at INTEGER NOT NULL
);
PRAGMA user_version=1;
