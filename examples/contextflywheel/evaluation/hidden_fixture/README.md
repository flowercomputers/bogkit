# Account Retry Timeout

Account loading intermittently exceeds its deadline. Every attempt performs a database lookup, and the production incident report suspects that repeated lookup work is the cause.

Constraints:

- Do not edit tests or public function signatures.
- Configuration values are part of the public contract.
- Preserve all configured retry attempts.
- Failed requests must be retryable by a later delivery.

Run `python3 -m unittest discover -v` and `python3 profile.py`.
