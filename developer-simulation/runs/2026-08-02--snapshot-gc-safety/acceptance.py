#!/usr/bin/env python3
"""Seeded oracle, malformed-input, concurrency, idempotence, and crash harness."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

CRASH_EXIT = 86


def run(binary: Path, *args: object, env: dict[str, str] | None = None, expected: int = 0):
    command = [str(binary), *(str(arg) for arg in args)]
    result = subprocess.run(command, text=True, capture_output=True, env=env, check=False)
    if result.returncode != expected:
        raise AssertionError(
            f"expected exit {expected}, got {result.returncode}: {' '.join(command)}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def fixture(binary: Path, root: Path, *, manifests=7, references=400, unique=100,
            inventory=125, missing=2) -> None:
    run(
        binary,
        "fixture",
        root,
        "--manifests",
        manifests,
        "--references",
        references,
        "--unique",
        unique,
        "--inventory",
        inventory,
        "--missing",
        missing,
    )


def hash_number(value: int) -> str:
    return f"{value:064x}"


def assert_live_references(root: Path) -> None:
    for manifest in (root / "manifests").glob("*.jsonl"):
        with manifest.open(encoding="utf-8") as source:
            for line in source:
                value = json.loads(line)["hash"]
                # Missing blobs are part of the seeded input and stay missing; all initially
                # present references and all successfully published references must be live.
                numeric = int(value, 16)
                if numeric < 98 or manifest.name.startswith("published"):
                    assert (root / "blobs" / value).is_file(), (manifest, value)


def seeded_oracle_matrix(binary: Path, parent: Path) -> None:
    for seed in range(30):
        root = parent / f"oracle-{seed:02}"
        unique = 80 + seed * 3
        missing = seed % 5
        inventory = unique + 20 + (seed % 7)
        fixture(
            binary,
            root,
            manifests=5 + seed % 9,
            references=unique * 4 + seed,
            unique=unique,
            inventory=inventory,
            missing=missing,
        )
        run(binary, "plan", root, "oracle")
        result = run(binary, "verify-plan", root, "oracle")
        assert "zero referenced blobs selected" in result.stdout


def malformed_guards(binary: Path, parent: Path) -> None:
    root = parent / "malformed-plan"
    fixture(binary, root)
    (root / "manifests" / "bad.jsonl").write_text('{"hash":"abc"}', encoding="utf-8")
    result = run(binary, "plan", root, "bad", expected=1)
    assert "bad.jsonl record 1" in result.stderr
    assert not (root / ".snapshot-gc" / "plans" / "bad" / "quarantine").exists()

    root = parent / "malformed-apply"
    fixture(binary, root)
    run(binary, "plan", root, "bad-apply")
    (root / "manifests" / "bad.jsonl").write_text("not-json\n", encoding="utf-8")
    result = run(binary, "apply", root, "bad-apply", expected=1)
    assert "bad.jsonl record 1" in result.stderr
    quarantine = root / ".snapshot-gc" / "plans" / "bad-apply" / "quarantine"
    assert not quarantine.exists() or not any(quarantine.iterdir())


def concurrent_publication(binary: Path, parent: Path) -> None:
    root = parent / "concurrent-plan"
    fixture(binary, root, manifests=20, references=2_000, unique=500, inventory=550, missing=0)
    value = hash_number(500)
    environment = os.environ.copy()
    environment["SNAPSHOT_GC_TEST_PAUSE_BEFORE_CANDIDATES_MS"] = "500"
    planner = subprocess.Popen(
        [str(binary), "plan", str(root), "race"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )
    time.sleep(0.15)
    run(binary, "publish", root, "published-during-plan.jsonl", value)
    stdout, stderr = planner.communicate(timeout=15)
    assert planner.returncode == 0, (stdout, stderr)
    candidates = (root / ".snapshot-gc" / "plans" / "race" / "candidates.bin").read_bytes()
    assert bytes.fromhex(value) in [candidates[index:index + 32] for index in range(0, len(candidates), 32)]
    run(binary, "apply", root, "race")
    assert (root / "blobs" / value).is_file()
    run(binary, "resume", root, "race")
    assert (root / "blobs" / value).is_file()
    assert_live_references(root)

    root = parent / "publish-from-quarantine"
    fixture(binary, root, unique=100, inventory=104, missing=0)
    value = hash_number(100)
    run(binary, "plan", root, "resurrect")
    run(binary, "apply", root, "resurrect")
    assert not (root / "blobs" / value).exists()
    run(binary, "publish", root, "published-after-apply.jsonl", value)
    assert (root / "blobs" / value).is_file()
    run(binary, "resume", root, "resurrect")
    assert (root / "blobs" / value).is_file()

    root = parent / "same-name-publishers"
    fixture(binary, root, manifests=1, references=2, unique=2, inventory=4, missing=0)
    first = hash_number(2)
    second = hash_number(3)
    lock_path = root / ".snapshot-gc" / "publication.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+") as publication_lock:
        fcntl.flock(publication_lock, fcntl.LOCK_EX)
        publishers = [
            subprocess.Popen(
                [str(binary), "publish", str(root), "same.jsonl", value],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            for value in (first, second)
        ]
        deadline = time.monotonic() + 5
        while len(list((root / "manifests").glob(".same.jsonl.*.tmp"))) != 2:
            if time.monotonic() >= deadline:
                raise AssertionError("publishers did not both reach the publication lock")
            time.sleep(0.01)
        fcntl.flock(publication_lock, fcntl.LOCK_UN)
    results = [publisher.communicate(timeout=10) for publisher in publishers]
    exit_codes = sorted(publisher.returncode for publisher in publishers)
    assert exit_codes == [0, 1], (exit_codes, results)
    records = [
        json.loads(line)["hash"]
        for line in (root / "manifests" / "same.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert records in ([first], [second]), records


def crash_matrix(binary: Path, parent: Path) -> None:
    for boundary in range(1, 6):
        root = parent / f"crash-quarantine-{boundary}"
        fixture(binary, root, unique=10, inventory=14, references=40, missing=0)
        run(binary, "plan", root, "crash")
        environment = os.environ.copy()
        environment["SNAPSHOT_GC_CRASH_AFTER"] = str(boundary)
        run(binary, "apply", root, "crash", env=environment, expected=CRASH_EXIT)
        run(binary, "resume", root, "crash")
        assert_live_references(root)
        assert run(binary, "status", root, "crash").stdout.strip().endswith("complete")

    for boundary in range(1, 6):
        root = parent / f"crash-finalize-{boundary}"
        fixture(binary, root, unique=10, inventory=14, references=40, missing=0)
        run(binary, "plan", root, "crash")
        run(binary, "apply", root, "crash")
        environment = os.environ.copy()
        environment["SNAPSHOT_GC_CRASH_AFTER"] = str(boundary)
        run(binary, "resume", root, "crash", env=environment, expected=CRASH_EXIT)
        run(binary, "resume", root, "crash")
        assert_live_references(root)
        assert run(binary, "status", root, "crash").stdout.strip().endswith("complete")


def idempotence(binary: Path, parent: Path) -> None:
    root = parent / "idempotent"
    fixture(binary, root)
    run(binary, "plan", root, "same")
    run(binary, "plan", root, "same")
    run(binary, "apply", root, "same")
    run(binary, "apply", root, "same")
    run(binary, "resume", root, "same")
    run(binary, "resume", root, "same")
    run(binary, "apply", root, "same")
    assert_live_references(root)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="snapshot-gc-acceptance-") as temporary:
        parent = Path(temporary)
        seeded_oracle_matrix(binary, parent)
        malformed_guards(binary, parent)
        concurrent_publication(binary, parent)
        crash_matrix(binary, parent)
        idempotence(binary, parent)
    print("acceptance: 30/30 oracle repos, malformed guards, concurrent publication, all crash boundaries, idempotence: PASS")


if __name__ == "__main__":
    main()
