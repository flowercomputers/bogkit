import unittest
from pathlib import Path

from service import AccountLoader, DeadlineExceeded, RetryConfig, TransientError


class FakeClock:
    def __init__(self):
        self.now = 0.0
        self.sleeps = []

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds


class Provider:
    def __init__(self, outcomes):
        self.outcomes = outcomes
        self.calls = 0

    def fetch(self, request_id):
        outcome = self.outcomes[self.calls]
        self.calls += 1
        if isinstance(outcome, Exception):
            raise outcome
        return outcome


def config(**changes):
    values = {"request_deadline_ms": 400, "retry_backoff_ms": 120, "max_attempts": 3}
    values.update(changes)
    return RetryConfig(**values)


class AccountLoaderTest(unittest.TestCase):
    def test_config_contract_uses_milliseconds(self):
        self.assertEqual(RetryConfig.load(Path("config.json")).retry_backoff_ms, 120)

    def test_database_lookup_is_twenty_milliseconds(self):
        clock = FakeClock()
        self.assertEqual(AccountLoader(Provider(["ok"]), clock, config()).load("a"), "ok")
        self.assertEqual(clock.sleeps, [0.020])

    def test_transient_failure_retries_inside_deadline(self):
        clock = FakeClock()
        loader = AccountLoader(Provider([TransientError(), "ok"]), clock, config())
        self.assertEqual(loader.load("b"), "ok")
        self.assertAlmostEqual(clock.now, 0.160)

    def test_all_attempts_are_available(self):
        provider = Provider([TransientError(), TransientError(), "ok"])
        self.assertEqual(AccountLoader(provider, FakeClock(), config()).load("c"), "ok")
        self.assertEqual(provider.calls, 3)

    def test_failed_delivery_can_be_retried(self):
        provider = Provider([TransientError(), TransientError(), TransientError(), "ok"])
        loader = AccountLoader(provider, FakeClock(), config())
        with self.assertRaises(TransientError):
            loader.load("d")
        self.assertEqual(loader.load("d"), "ok")

    def test_completed_delivery_is_idempotent(self):
        provider = Provider(["ok"])
        loader = AccountLoader(provider, FakeClock(), config())
        self.assertEqual(loader.load("e"), "ok")
        self.assertEqual(loader.load("e"), "ok")
        self.assertEqual(provider.calls, 1)

    def test_deadline_prevents_an_extra_provider_call(self):
        provider = Provider([TransientError(), "must-not-run"])
        loader = AccountLoader(provider, FakeClock(), config(request_deadline_ms=100))
        with self.assertRaises(DeadlineExceeded):
            loader.load("f")
        self.assertEqual(provider.calls, 1)


if __name__ == "__main__":
    unittest.main()
