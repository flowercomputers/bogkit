#!/usr/bin/env python3
"""Non-LLM smoke adapter used only to verify the evaluation harness."""

import sys
from pathlib import Path


_, mode, fixture, data = sys.argv
path = Path(fixture) / "service.py"
path.write_text(path.read_text().replace("RETRY_BACKOFF_SECONDS = 0.50", "RETRY_BACKOFF_SECONDS = 0.05"))
print(f"smoke adapter completed mode={mode} data={data}")
