#!/usr/bin/env python3
"""Small reference implementation of the stated existing exact-match behavior."""

import argparse
import pathlib
import sys


def load_map(path: pathlib.Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="ascii").splitlines():
        if line and not line.startswith("#"):
            barcode, sample = line.split("\t")
            result[barcode.upper()] = sample
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--barcodes", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()

    mapping = load_map(args.barcodes)
    args.out.mkdir(parents=True)
    # This intentionally models the original limitation: every destination is open.
    handles = {}
    try:
        for sample in dict.fromkeys(mapping.values()):
            handles[sample] = (args.out / f"{sample}.fastq").open("wb")
        handles["__unmatched__"] = (args.out / "unmatched.fastq").open("wb")
    except OSError:
        for handle in handles.values():
            handle.close()
        print("error: could not open all output files", file=sys.stderr)
        return 1
    try:
        stream = sys.stdin.buffer
        while True:
            lines = [stream.readline() for _ in range(8)]
            if not lines[0]:
                break
            if any(not line for line in lines):
                raise ValueError("truncated FASTQ pair")
            barcode = lines[1].rstrip(b"\r\n")[:10].decode("ascii").upper()
            destination = mapping.get(barcode, "__unmatched__")
            handles[destination].writelines(lines)
    finally:
        for handle in handles.values():
            handle.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
