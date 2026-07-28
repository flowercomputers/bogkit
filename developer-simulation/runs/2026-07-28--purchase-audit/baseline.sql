-- Reference shape for the recommended PostgreSQL baseline.
-- This file was reviewed but not executed because the prototype has no
-- PostgreSQL service.

CREATE TABLE purchase_request (
    request_id bigint PRIMARY KEY,
    amount_cents bigint NOT NULL CHECK (amount_cents >= 0),
    status text NOT NULL CHECK (status IN ('draft', 'approved', 'rejected', 'cancelled')),
    policy_version text NOT NULL
);

CREATE TABLE purchase_request_audit (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_id bigint NOT NULL REFERENCES purchase_request(request_id),
    action text NOT NULL CHECK (action IN ('create', 'approve', 'reject', 'cancel')),
    previous_status text,
    new_status text NOT NULL,
    amount_cents bigint NOT NULL,
    actor text NOT NULL,
    policy_version text NOT NULL,
    occurred_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);

CREATE INDEX purchase_request_audit_timeline
    ON purchase_request_audit (request_id, sequence);

-- Replace purchase_app with the actual non-owner application role.
REVOKE UPDATE, DELETE, TRUNCATE ON purchase_request_audit FROM purchase_app;
GRANT SELECT, INSERT ON purchase_request_audit TO purchase_app;

-- Each service mutation must update purchase_request and insert exactly one
-- purchase_request_audit row inside the same SQL transaction. Production
-- enforcement should use one reviewed mutation function or an equivalent
-- single repository path, plus integration tests against PostgreSQL.
