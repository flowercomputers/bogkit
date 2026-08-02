#!/usr/bin/env python3
"""Frozen comparison baseline: the existing in-memory direct-delete collector."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def object_hash(number: int) -> str:
    return f"{number:064x}"


def seed(root: Path, references: int, unique: int, inventory: int, manifests: int) -> None:
    if unique > inventory or references < unique:
        raise ValueError("require references >= unique and inventory >= unique")
    manifest_dir = root / "manifests"
    blob_dir = root / "blobs"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    blob_dir.mkdir(parents=True, exist_ok=True)
    handles = [
        (manifest_dir / f"snapshot-{index:08}.jsonl").open("w", encoding="utf-8")
        for index in range(manifests)
    ]
    try:
        for index in range(references):
            value = index if index < unique else index % unique
            handles[index % manifests].write(json.dumps({"hash": object_hash(value)}) + "\n")
    finally:
        for handle in handles:
            handle.close()
    for index in range(inventory):
        (blob_dir / object_hash(index)).touch()


def collect(root: Path) -> tuple[int, int]:
    # The observed production shape: every reference lives as a Python string
    # in one set, and collection deletes immediately after the scan.
    referenced: set[str] = set()
    manifests = sorted((root / "manifests").glob("*.jsonl"))
    for manifest in manifests:
        with manifest.open(encoding="utf-8") as source:
            for record_number, line in enumerate(source, 1):
                try:
                    record = json.loads(line)
                    value = record["hash"]
                except (json.JSONDecodeError, KeyError, TypeError) as error:
                    raise RuntimeError(
                        f"malformed manifest {manifest} record {record_number}: {error}"
                    ) from error
                if not isinstance(value, str) or len(value) != 64:
                    raise RuntimeError(
                        f"malformed manifest {manifest} record {record_number}: invalid hash"
                    )
                referenced.add(value)

    removed = 0
    for blob in (root / "blobs").iterdir():
        if blob.is_file() and blob.name not in referenced:
            blob.unlink()
            removed += 1
    return len(referenced), removed


def main() -> None:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    seed_parser = commands.add_parser("seed")
    seed_parser.add_argument("root", type=Path)
    seed_parser.add_argument("--references", type=int, default=1_000)
    seed_parser.add_argument("--unique", type=int, default=250)
    seed_parser.add_argument("--inventory", type=int, default=300)
    seed_parser.add_argument("--manifests", type=int, default=10)
    collect_parser = commands.add_parser("collect")
    collect_parser.add_argument("root", type=Path)
    args = parser.parse_args()

    if args.command == "seed":
        seed(args.root, args.references, args.unique, args.inventory, args.manifests)
        print(f"seeded {args.root}")
    else:
        referenced, removed = collect(args.root)
        print(f"loaded {referenced} unique references; directly removed {removed} blobs")


if __name__ == "__main__":
    main()
