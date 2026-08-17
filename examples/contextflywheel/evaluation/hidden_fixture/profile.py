from service import AccountLoader, RetryConfig, TransientError
from test_service import FakeClock, Provider

clock = FakeClock()
loader = AccountLoader(Provider([TransientError(), "ok"]), clock, RetryConfig(400, 120, 3))
try:
    loader.load("profile")
except Exception as error:
    print(f"result={type(error).__name__}")
print("database_ms=20")
print(f"simulated_elapsed_seconds={clock.now:.3f}")
print(f"sleep_calls={clock.sleeps}")
