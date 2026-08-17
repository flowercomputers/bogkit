#!/usr/bin/env python3
"""Repeatable three-mode agent evaluation.

The agent command is an argv template. Supported placeholders are {fixture},
{data}, and {mode}. The command must edit only the copied fixture.
"""

import argparse
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


def run_once(source: Path, command: list[str], mode: str, run: int) -> dict:
    root = Path(tempfile.mkdtemp(prefix=f"contextflywheel-{mode}-{run}-"))
    fixture = root / "fixture"
    data = root / "mission"
    shutil.copytree(source, fixture)
    argv = [part.format(fixture=fixture, data=data, mode=mode) for part in command]
    started = time.monotonic()
    process = subprocess.run(argv, cwd=fixture, text=True, capture_output=True, timeout=900)
    runtime = time.monotonic() - started
    test = subprocess.run(["python3", "-m", "unittest", "test_service.py"], cwd=fixture, text=True, capture_output=True)
    records = []
    ledger = data / "ledger.jsonl"
    if ledger.exists():
        records = [json.loads(line) for line in ledger.read_text().splitlines() if line]
    return {
        "mode": mode,
        "run": run,
        "success": test.returncode == 0,
        "agent_exit": process.returncode,
        "runtime_seconds": round(runtime, 3),
        "tool_calls": sum(r["kind"] == "tool_call" for r in records),
        "tool_failures": sum(r["kind"] == "tool_failure" for r in records),
        "belief_revisions": sum(r["kind"] == "belief_revision" for r in records),
        "ledger_records": len(records),
        "stdout_chars": len(process.stdout),
        "stderr_chars": len(process.stderr),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent-command", nargs="+", required=True)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--output", type=Path, default=Path("evaluation-results.json"))
    args = parser.parse_args()
    executable = Path(args.agent_command[0])
    if executable.parent != Path("."):
        args.agent_command[0] = str(executable.resolve())
    source = Path(__file__).parents[1] / "fixture"
    results = [run_once(source, args.agent_command, mode, run) for mode in ("raw", "deterministic", "hybrid") for run in range(1, args.runs + 1)]
    args.output.write_text(json.dumps(results, indent=2) + "\n")
    for mode in ("raw", "deterministic", "hybrid"):
        rows = [r for r in results if r["mode"] == mode]
        print(f"{mode:13} success={sum(r['success'] for r in rows)}/{len(rows)} avg_runtime={sum(r['runtime_seconds'] for r in rows)/len(rows):.2f}s")


if __name__ == "__main__":
    main()
