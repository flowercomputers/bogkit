"""Deliberately broken timeout fixture for the ContextFlywheel demo."""

import time

DATABASE_SECONDS = 0.02
RETRY_BACKOFF_SECONDS = 0.50  # The agent should diagnose and reduce this.
REQUEST_DEADLINE_SECONDS = 0.20


def database_lookup() -> None:
    time.sleep(DATABASE_SECONDS)


def load_account(attempts: int = 2) -> dict[str, str]:
    started = time.monotonic()
    for attempt in range(attempts):
        database_lookup()
        if attempt == 0:
            time.sleep(RETRY_BACKOFF_SECONDS)
        if time.monotonic() - started > REQUEST_DEADLINE_SECONDS:
            raise TimeoutError("account request exceeded 200ms deadline")
    return {"status": "ok"}
