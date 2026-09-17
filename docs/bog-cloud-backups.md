# Whole-host recovery backups

`scripts/cloud/offhost_backup.py` captures the entire registry and instance data together, encrypts with age, and uploads to a pre-existing private S3-compatible bucket. It preserves Bog IDs, workspace membership, invitations and app credential hashes. It does not call the single-Bog restore API (which creates new IDs). Browser sessions and other files outside `registry.sqlite` and `instances/` are excluded; everyone signs in again after recovery.

This is release tooling, **not an active daily backup deployment**. No bucket, credentials, scheduler, or production restore has been configured by this change. The checked-in systemd examples are inactive and are not directly usable inside Fly's existing container. An operator must provide an external scheduler with verified host stop/start hooks, storage destination and failure alerting before claiming daily protection.

## Prerequisites and secrets

Install Python 3.11+, a maintained age CLI and AWS CLI v2 using your platform's trusted package distribution. Commands are resolved through a trusted operator-controlled PATH. No dependency is downloaded at runtime. This script targets Unix hosts, where Python `flock` and Rust `std::fs::File::try_lock` use the same advisory lock mechanism. Do not use it on NFS or Windows. Do not run unrelated writers during a snapshot or restore.

Supply AWS credentials through the standard AWS environment/provider chain. Supply `BOG_BACKUP_BUCKET`, a dedicated per-host `BOG_BACKUP_PREFIX` ending in `/`, `BOG_BACKUP_AGE_RECIPIENT` (public age1 recipient), and optionally `BOG_BACKUP_ENDPOINT` (HTTPS, without embedded credentials). Restrict IAM to put/get/list/delete only within that prefix and deny public access at the bucket. Uploads explicitly request the private ACL; stores that prohibit ACLs fail closed and require an operator-reviewed adapter. No resources are provisioned.

Recovery additionally requires `BOG_BACKUP_AGE_IDENTITY`, the age secret identity text, from a protected secret manager. It is passed to age through stdin, never arguments. Keep the identity off the backed-up host, with a separately tested recovery copy. Do not enable shell tracing, AWS debug output, or dump the environment. Child command output and exception details are suppressed. Backups contain sensitive registry information even though browser sessions are excluded.

## Snapshot and retention

Stop and reap the manager **and every worker** before running:

```
python3 scripts/cloud/offhost_backup.py backup /data/bog-cloud
```

The tool holds `manager.lock`, each `instances/*/worker.lock`, and each Fjall `instances/*/data/lock` through the whole snapshot. Any active lock holder aborts the backup. A stopped manager alone is insufficient: workers can survive manager failure. Never bypass a lock failure by deleting lock files. The SQLite backup API includes committed WAL contents. The capture allowlist excludes sockets, lock files and separate authentication-session databases. Symlinks and unexpected special files fail closed. Only trusted closed service directories are supported (no concurrent filesystem changes by other users).

Plaintext intermediate data stays in a private temporary directory and is removed on normal exit; a killed process or host crash can leave it behind, so use encrypted local storage and clean stale private temporary directories. The encrypted archive is fully written and fsynced before upload. S3 PUT publishes the object atomically. Each successful upload is followed by retention pruning of matching archive names older than seven days within this host's prefix. Failed uploads do not trigger pruning. A cleanup failure marks the run failed even if upload succeeded. Bucket lifecycle rules can independently enforce retention after scheduler outages.

Restart the service even if snapshot/upload fails. Connect scheduler failure and missed-run signals to the operator's existing monitoring. The sample unit's stop/start hooks are deliberately prerequisites rather than guessed process-killing commands.

## Restore and automated drill

Download the selected archive with the AWS CLI or run a disposable isolated drill directly:

```
python3 scripts/cloud/offhost_backup.py drill host-a/20260917T040000Z-00000000-0000-0000-0000-000000000000.tar.age
python3 scripts/cloud/offhost_backup.py restore /private/recovery.tar.age /data/restored-bog-cloud
```

The destination must not exist; its parent must already exist and be canonical (no symlink components). Decryption occurs in a private temporary sibling. The tool rejects links, traversal, duplicate/unexpected archive entries, verifies every file checksum and size, runs SQLite integrity checks and checks the registry schema version against the manifest before publishing with a same-filesystem rename. Run restore in an operator-exclusive parent directory; no other process may create the destination concurrently. Encryption authentication protects the manifest against external tampering; checksums also detect accidental internal inconsistency.

The `drill` command downloads from the configured host prefix, performs the same full restore checks in a disposable directory, then removes it. Schedule it separately on a recovery host holding the identity and alert on any nonzero exit. It validates byte preservation and registry integrity, not application compatibility. Before release and after schema changes, boot the restored tree with the matching Bog binary on an isolated address, verify the same IDs, permissions, app credentials and records with two accounts, and check revoked access remains revoked. Reauthenticate browser sessions. Never point the recovery drill at the production listener or production data directory.

Local tests: `python3 -m unittest discover -s scripts/cloud/tests -v`. These use a fake encryption command/object store to exercise orchestration and failures; they do not establish real age cryptography, real S3 behavior or an active schedule. Those are deployment gates.
