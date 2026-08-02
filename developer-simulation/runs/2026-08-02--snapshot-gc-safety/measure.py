#!/usr/bin/env python3
"""Run one command and report wall time, peak RSS, and optional peak scratch bytes."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import threading
import time


def logical_size(root: Path) -> int:
    total = 0
    if not root.exists():
        return total
    for directory, _subdirectories, files in os.walk(root):
        for name in files:
            try:
                total += (Path(directory) / name).stat().st_size
            except FileNotFoundError:
                pass
    return total


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scratch", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        parser.error("a command is required after --")

    peak_scratch = 0
    stop = threading.Event()

    def monitor() -> None:
        nonlocal peak_scratch
        if args.scratch is None:
            return
        while not stop.wait(0.01):
            peak_scratch = max(peak_scratch, logical_size(args.scratch))

    watcher = threading.Thread(target=monitor, daemon=True)
    watcher.start()
    started = time.monotonic()
    result = subprocess.run(command, check=False)
    wall_seconds = time.monotonic() - started
    stop.set()
    watcher.join()
    if args.scratch is not None:
        peak_scratch = max(peak_scratch, logical_size(args.scratch))

    peak_rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    if sys.platform.startswith("linux"):
        peak_rss *= 1024
    print(
        json.dumps(
            {
                "exit": result.returncode,
                "wall_seconds": round(wall_seconds, 3),
                "peak_rss_bytes": peak_rss,
                "peak_scratch_bytes": peak_scratch,
            },
            sort_keys=True,
        )
    )
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
