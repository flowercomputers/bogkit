CREATE TABLE agent_tokens(id TEXT PRIMARY KEY,account_id TEXT NOT NULL REFERENCES accounts(id),name TEXT NOT NULL,secret_hash BLOB NOT NULL UNIQUE,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,revoked_at INTEGER);
CREATE INDEX agent_tokens_account ON agent_tokens(account_id);
PRAGMA user_version=3;
