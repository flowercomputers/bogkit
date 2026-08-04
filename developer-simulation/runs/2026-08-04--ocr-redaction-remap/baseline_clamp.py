#!/usr/bin/env python3
"""Content-free reproducer for stale-offset clamping.

The assertions model the existing policy; stdout intentionally contains no OCR text.
"""

import json


old_text = "prefix extra account-12345 suffix"
revised_text = "prefix account 12345 suffix"
start = old_text.index("account-12345")
end = start + len("account-12345")

clamped_start = min(start, len(revised_text))
clamped_end = min(end, len(revised_text))
covered = revised_text[clamped_start:clamped_end]

assert "account 12345" not in covered, "fixture must reproduce partial exposure"
assert revised_text.index("account 12345") < clamped_start, "fixture must expose a sensitive prefix"

print(json.dumps({"baseline": "stale_offset_clamp", "partial_exposure_reproduced": True}))
