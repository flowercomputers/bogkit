# Persistent Bog notebook starter

A small loopback-only application backed by real Bog records and semantic/BM25 search. Python 3.9+, no third-party Python dependencies. This is a starting point for a local single-user app, not a publicly hosted multi-user service. It displays up to 100 notes and three search neighbors; it does not implement pagination or live synchronization.

From the repository root, first connect. Create a private directory outside public assets:

```sh
mkdir -p ~/.config/bog-cloud
python3 scripts/cloud/bog_app_access.py --connect --auth-file ~/.config/bog-cloud/agent.json
```

Show the approval link, keep the command running, and approve on Bog. No chat acknowledgment is needed. The command verifies access before reporting Connected. Credentials are never printed. If interrupted before credential collection, run it again; an unused approval expires. After collection, the saved authorization can be reused explicitly.

Create a project and install its scoped app access. Choose a stable, unique idempotency key and keep it for retries:

```sh
python3 scripts/cloud/bog_project.py --auth-file ~/.config/bog-cloud/agent.json create \
  --name notebook --idempotency-key my-notebook-setup-1 \
  --output ~/.config/bog-cloud/notebook.json
```

The result reports the Bog ID and actual resources after checking app access. Shared workspaces require `--workspace-id ID` before `create`; omission uses Personal. Do not display either private file. If installation fails after creation, the Bog may already exist: retain its ID/key, inspect it, and prepare fresh app access instead of creating a new Bog.

Using the returned Bog ID, add both search resources. These commands preserve existing resources, enforce revision checks, and wait for activation:

```sh
python3 scripts/cloud/bog_project.py --auth-file ~/.config/bog-cloud/agent.json add-search \
  --bog-id BOG_ID --name semantic_search --kind semantic --fields /title /body
python3 scripts/cloud/bog_project.py --auth-file ~/.config/bog-cloud/agent.json add-search \
  --bog-id BOG_ID --name text_search --kind bm25 --fields /title /body
python3 starters/cloud-notebook/server.py --config ~/.config/bog-cloud/notebook.json
```

Open `http://127.0.0.1:4317`. Add a note titled “A cabin for deep work” with body “An isolated shelter among trees, away from interruptions.” Search “quiet woodland retreat”: Meaning should find the note, while Words has no shared tokens. Results depend on other records; semantic ranking is not confidence. Nothing is seeded automatically.

Restart with the same server command; records persist in Bog. The server serves only the bundled page and explicit API routes, never the credential directory. Browser requests contain no Bog credential. It rejects unexpected Host and Origin values and binds only to loopback. Keep the app single-user/local; a public deployment needs its own user authentication and authorization.

For expiry or revocation, reconnect management access with `--connect`, prepare a fresh app handoff, and run `bog_app_access.py --handoff ID --auth-file ... --output ... --replace`. Restart the notebook afterward. Never replace the app credential with a management credential. Revoke unused app credentials through the console.

## Client API

`scripts/cloud/bog_client.py` provides `Client.from_config`, `get`, `put`, `remove`, `batch`, `query`, `search`, `resources`, and `diagnostics`. Management clients created with `from_auth` additionally support `create` and `add_search`. `add_search` accepts optional filter/projection stages. Errors carry HTTP status without printing server bodies; a 409 requires refreshing the definition and reconsidering the change, not blind retries. A timeout may leave a build running; inspect the returned job ID before retrying.

`diagnostics()` returns the usage envelope. Configurable Bogs include exposed search resources with fields and eligible record counts, definition revision, and worker boot ID. Counts are computed on demand from the source under the worker lock; they are not a separate index-integrity audit. Sequence numbers are comparable only within one boot. Search responses themselves confirm query completion and carry a sequence/cursor.
