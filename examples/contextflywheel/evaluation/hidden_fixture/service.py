import json
from dataclasses import dataclass
from pathlib import Path


class TransientError(RuntimeError):
    pass


class DeadlineExceeded(RuntimeError):
    pass


@dataclass(frozen=True)
class RetryConfig:
    request_deadline_ms: int
    retry_backoff_ms: int
    max_attempts: int

    @classmethod
    def load(cls, path: Path) -> "RetryConfig":
        return cls(**json.loads(path.read_text()))


class AccountLoader:
    def __init__(self, provider, clock, config: RetryConfig):
        self.provider = provider
        self.clock = clock
        self.config = config
        self._completed = {}

    def _database_lookup(self, request_id: str) -> None:
        self.clock.sleep(0.020)

    def load(self, request_id: str) -> str:
        if request_id in self._completed:
            result = self._completed[request_id]
            return "duplicate" if result is None else result
        self._completed[request_id] = None
        deadline = self.clock.monotonic() + self.config.request_deadline_ms
        for attempt in range(self.config.max_attempts - 1):
            self._database_lookup(request_id)
            try:
                result = self.provider.fetch(request_id)
                self._completed[request_id] = result
                return result
            except TransientError:
                if attempt + 1 >= self.config.max_attempts:
                    raise
                self.clock.sleep(self.config.retry_backoff_ms)
                if self.clock.monotonic() >= deadline:
                    raise DeadlineExceeded(request_id)
        raise DeadlineExceeded(request_id)
