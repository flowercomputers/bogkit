-- This reserved workspace has no members. Claimable credentials remain scoped to one Bog.
INSERT OR IGNORE INTO workspaces(id,name,personal_account_id,suspended_at,created_at,deleted_at)
VALUES('00000000-0000-0000-0000-000000000002','Temporary unclaimed Bogs',NULL,NULL,0,NULL);
CREATE TABLE IF NOT EXISTS claimable_bogs (
 bog_id TEXT PRIMARY KEY REFERENCES bogs(id),
 desired_name TEXT NOT NULL,
 request_key TEXT NOT NULL UNIQUE,
 request_hash BLOB NOT NULL,
 recovery_hash BLOB NOT NULL,
 creator_token_id TEXT,
 source_hash BLOB NOT NULL,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('active','claimed','expired')),
 claim_attempts INTEGER NOT NULL DEFAULT 0,
 claimed_workspace_id TEXT REFERENCES workspaces(id),
 claimed_at INTEGER
);
CREATE TABLE IF NOT EXISTS claimable_claims (
 id TEXT PRIMARY KEY,
 bog_id TEXT NOT NULL REFERENCES bogs(id),
 code_hash BLOB NOT NULL UNIQUE,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 consumed_at INTEGER,
 workspace_id TEXT REFERENCES workspaces(id),
 CHECK(expires_at > created_at)
);
CREATE INDEX IF NOT EXISTS claimable_claims_bog ON claimable_claims(bog_id, created_at DESC);
CREATE INDEX IF NOT EXISTS claimable_active_expiry ON claimable_bogs(state, expires_at);
CREATE INDEX IF NOT EXISTS claimable_created ON claimable_bogs(created_at);
