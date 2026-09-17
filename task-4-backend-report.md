
## Review round 1

Addressed task-4-6 review findings 1-3 with dependency owners:
- Cursorless wait service path now calls bounded ChangeWaiter.initialize and validates caller timeout. REST and MCP both reject timeout26 with no cursor; targeted shared cursor test passed.
- Response cursor generation now comes from immutable WorkerLease.generation captured with the concrete worker by lifecycle agent, never registry metadata after response. Lifecycle agent owns crash/restart regression test.
- OpenAPI adds schema read path and explicit workspace_id on usage/deletion. Contract test verifies all three. Gateway test now joins a second verified account to a shared workspace and tests explicit schema/usage/owner deletion while its personal default stays empty. Gateway2 and contractunit1 passed.
- Public change-wait description discloses initial state acquisition's separate 25-second ceiling, including timeout0.
