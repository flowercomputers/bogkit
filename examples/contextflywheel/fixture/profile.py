import time

import service


started = time.monotonic()
service.database_lookup()
database_ms = (time.monotonic() - started) * 1_000

print(f"database_ms={database_ms:.1f}")
print(f"retry_backoff_ms={service.RETRY_BACKOFF_SECONDS * 1_000:.1f}")
print(f"deadline_ms={service.REQUEST_DEADLINE_SECONDS * 1_000:.1f}")
