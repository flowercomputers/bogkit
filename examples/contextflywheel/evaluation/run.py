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


def claude_usage(stdout: str) -> dict:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError:
        return {}
    usage = payload.get("usage") or {}
    return {
        "input_tokens": usage.get("input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "cache_read_input_tokens": usage.get("cache_read_input_tokens"),
        "cache_creation_input_tokens": usage.get("cache_creation_input_tokens"),
    }


def ledger_metrics(records: list[dict]) -> dict:
    tool_calls = [r for r in records if r["kind"] == "tool_call"]
    normalized = [r["text"].split(" result:", 1)[0] for r in tool_calls]
    duplicate_actions = len(normalized) - len(set(normalized))
    beliefs = [r for r in records if r["kind"] == "belief_revision"]
    correction_seconds = None
    if len(beliefs) >= 2:
        from datetime import datetime
        first = datetime.fromisoformat(beliefs[0]["timestamp"].replace("Z", "+00:00"))
        second = datetime.fromisoformat(beliefs[1]["timestamp"].replace("Z", "+00:00"))
        correction_seconds = round((second - first).total_seconds(), 3)
    return {
        "tool_calls": len(tool_calls),
        "tool_failures": sum(r["kind"] == "tool_failure" for r in records),
        "belief_revisions": len(beliefs),
        "decision_redirects": sum(r["kind"] == "decision_redirect" for r in records),
        "context_snapshots": sum(r["kind"] == "context_snapshot" for r in records),
        "repeated_tool_actions": duplicate_actions,
        "time_to_belief_correction_seconds": correction_seconds,
    }


def run_once(source: Path, command: list[str], mode: str, run: int) -> dict:
    root = Path(tempfile.mkdtemp(prefix=f"contextflywheel-{mode}-{run}-"))
    fixture = root / "fixture"
    data = root / "mission"
    shutil.copytree(source, fixture)
    argv = [part.format(fixture=fixture, data=data, mode=mode) for part in command]
    started = time.monotonic()
    process = subprocess.run(argv, cwd=fixture, text=True, capture_output=True, timeout=900)
    runtime = time.monotonic() - started
    test = subprocess.run(["python3", "-m", "unittest", "discover", "-v"], cwd=fixture, text=True, capture_output=True)
    records = []
    ledger = data / "ledger.jsonl"
    if ledger.exists():
        records = [json.loads(line) for line in ledger.read_text().splitlines() if line]
    result = {
        "mode": mode,
        "run": run,
        "success": test.returncode == 0,
        "agent_exit": process.returncode,
        "runtime_seconds": round(runtime, 3),
        "ledger_records": len(records),
        "stdout_chars": len(process.stdout),
        "stderr_chars": len(process.stderr),
    }
    result.update(ledger_metrics(records))
    result.update(claude_usage(process.stdout))
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent-command", nargs="+", required=True)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--output", type=Path, default=Path("evaluation-results.json"))
    parser.add_argument("--fixture", type=Path)
    args = parser.parse_args()
    executable = Path(args.agent_command[0])
    if executable.parent != Path("."):
        args.agent_command[0] = str(executable.resolve())
    source = args.fixture or Path(__file__).parents[1] / "fixture"
    results = [run_once(source, args.agent_command, mode, run) for mode in ("raw", "deterministic", "hybrid") for run in range(1, args.runs + 1)]
    args.output.write_text(json.dumps(results, indent=2) + "\n")
    for mode in ("raw", "deterministic", "hybrid"):
        rows = [r for r in results if r["mode"] == mode]
        print(f"{mode:13} success={sum(r['success'] for r in rows)}/{len(rows)} avg_runtime={sum(r['runtime_seconds'] for r in rows)/len(rows):.2f}s")


if __name__ == "__main__":
    main()
