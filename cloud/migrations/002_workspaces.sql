CREATE TABLE accounts(id TEXT PRIMARY KEY,issuer TEXT NOT NULL,subject TEXT NOT NULL,suspended_at INTEGER,created_at INTEGER NOT NULL,UNIQUE(issuer,subject));
CREATE TABLE workspaces(id TEXT PRIMARY KEY,name TEXT NOT NULL,personal_account_id TEXT UNIQUE REFERENCES accounts(id),suspended_at INTEGER,created_at INTEGER NOT NULL,deleted_at INTEGER);
INSERT INTO workspaces VALUES('00000000-0000-0000-0000-000000000001','Unclaimed legacy',NULL,NULL,0,NULL);
CREATE TABLE memberships(workspace_id TEXT NOT NULL REFERENCES workspaces(id),account_id TEXT NOT NULL REFERENCES accounts(id),role TEXT NOT NULL CHECK(role IN ('owner','member')),PRIMARY KEY(workspace_id,account_id));
CREATE TABLE bogs_v2 (
 id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),name TEXT NOT NULL,
 template TEXT NOT NULL CHECK(template='records-v1'),template_version TEXT NOT NULL CHECK(template_version='records-v1'),
 desired_state TEXT NOT NULL CHECK(desired_state IN ('running','stopped')),
 observed_state TEXT NOT NULL CHECK(observed_state IN ('creating','ready','stopped','failed','restoring','maintenance')),
 generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0),failure_code TEXT,created_at INTEGER NOT NULL,startup_nonce TEXT,deleted_at INTEGER,cleanup_completed_at INTEGER,UNIQUE(workspace_id,name));
INSERT INTO bogs_v2 SELECT id,'00000000-0000-0000-0000-000000000001',name,template,template_version,desired_state,observed_state,generation,failure_code,created_at,startup_nonce,NULL,NULL FROM bogs;
CREATE TABLE create_requests_v2(workspace_id TEXT NOT NULL REFERENCES workspaces(id),request_key TEXT NOT NULL,body_hash BLOB NOT NULL,bog_id TEXT NOT NULL REFERENCES bogs(id),PRIMARY KEY(workspace_id,request_key));
INSERT INTO create_requests_v2 SELECT '00000000-0000-0000-0000-000000000001',request_key,body_hash,bog_id FROM create_requests;
DROP TABLE create_requests;
DROP TABLE bogs;
ALTER TABLE bogs_v2 RENAME TO bogs;
ALTER TABLE create_requests_v2 RENAME TO create_requests;
ALTER TABLE tokens ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE tokens ADD COLUMN account_id TEXT REFERENCES accounts(id);
ALTER TABLE tokens ADD COLUMN expires_at INTEGER;
UPDATE tokens SET workspace_id='00000000-0000-0000-0000-000000000001';
CREATE TABLE invitations(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL REFERENCES workspaces(id),issuer_account_id TEXT NOT NULL REFERENCES accounts(id),secret_hash BLOB NOT NULL UNIQUE,role TEXT NOT NULL CHECK(role IN ('owner','member')),expires_at INTEGER NOT NULL,accepted_at INTEGER,revoked_at INTEGER,created_at INTEGER NOT NULL);
CREATE TABLE audit_events(id INTEGER PRIMARY KEY,workspace_id TEXT,account_id TEXT,action TEXT NOT NULL,resource_id TEXT,created_at INTEGER NOT NULL);
PRAGMA user_version=2;
