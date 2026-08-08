#!/usr/bin/env python3
"""Concrete reproducer for the filename-only Python baseline."""

import json
import sys
import zipfile


def check(path: str) -> dict[str, object]:
    errors: list[dict[str, str]] = []
    try:
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
            if "manifest.json" not in names:
                errors.append({"code": "missing_required", "path": "manifest.json"})
            for name in names:
                lowered = name.lower()
                if not lowered.endswith((".json", ".nc", ".gcode")):
                    errors.append({"code": "extension_not_allowed", "path": name})
    except (OSError, zipfile.BadZipFile) as error:
        errors.append({"code": "unreadable_zip", "path": str(error)})
    errors.sort(key=lambda item: (item["code"], item["path"]))
    return {"ready": not errors, "diagnostics": errors}


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: baseline.py BUNDLE.zip")
    print(json.dumps(check(sys.argv[1]), sort_keys=True))
