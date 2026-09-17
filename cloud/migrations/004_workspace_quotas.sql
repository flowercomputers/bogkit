ALTER TABLE accounts ADD COLUMN uncapped_bogs INTEGER NOT NULL DEFAULT 0 CHECK(uncapped_bogs IN (0,1));
ALTER TABLE accounts ADD COLUMN platform_operator INTEGER NOT NULL DEFAULT 0 CHECK(platform_operator IN (0,1));
ALTER TABLE workspaces ADD COLUMN uncapped_bogs INTEGER NOT NULL DEFAULT 0 CHECK(uncapped_bogs IN (0,1));
CREATE TABLE workspace_create_requests(account_id TEXT NOT NULL REFERENCES accounts(id),request_key TEXT NOT NULL,request_name TEXT NOT NULL,workspace_id TEXT NOT NULL REFERENCES workspaces(id),PRIMARY KEY(account_id,request_key));
PRAGMA user_version=4;
