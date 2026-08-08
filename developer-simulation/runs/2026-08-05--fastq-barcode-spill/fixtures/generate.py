#!/usr/bin/env python3
"""Deterministic interleaved FASTQ and barcode-map generator."""

import argparse
import pathlib
import sys
import time

BASES = "ACGT"


def barcode_for(index: int) -> str:
    chars = ["A"] * 10
    for position in range(9, -1, -1):
        chars[position] = BASES[index & 3]
        index >>= 2
    return "".join(chars)


def barcode_set(samples: int, well_spaced: bool) -> list[str]:
    if not well_spaced:
        return [barcode_for(index) for index in range(samples)]

    selected = ["AAAAAAAAAA", "CCAAAAAAAA"][:samples]
    if len(selected) == samples:
        return selected
    for candidate_index in range(4**10):
        candidate = barcode_for(candidate_index)
        if all(sum(a != b for a, b in zip(candidate, chosen)) >= 3 for chosen in selected):
            selected.append(candidate)
            if len(selected) == samples:
                return selected
    raise ValueError("could not construct requested well-spaced barcode set")


def write_map(path: pathlib.Path, barcodes: list[str]) -> None:
    with path.open("w", encoding="ascii", newline="\n") as output:
        for index, barcode in enumerate(barcodes):
            output.write(f"{barcode}\ts{index:03d}\n")


def mutate_once(barcode: str, pair_index: int) -> str:
    position = pair_index % len(barcode)
    replacement = BASES[(BASES.index(barcode[position]) + 1) % len(BASES)]
    return barcode[:position] + replacement + barcode[position + 1 :]


def emit(pairs: int, barcodes: list[str], pause_ms: float, mixed: bool) -> None:
    output = sys.stdout
    r2_sequence = "TGCATGCATGCATGCATGCATGCATGCATGCATGCATGCA"
    r2_quality = "I" * len(r2_sequence)
    chunk: list[str] = []
    for pair_index in range(pairs):
        if not mixed or pair_index % 4 == 0:
            barcode = barcodes[pair_index % len(barcodes)]
        elif pair_index % 4 == 1:
            sample_index = 2 + ((pair_index // 4) % (len(barcodes) - 2))
            barcode = mutate_once(barcodes[sample_index], pair_index)
        elif pair_index % 4 == 2:
            barcode = "CAAAAAAAAA"  # one base from each of the first two whitelist entries
        else:
            barcode = "NNNNNNNNNN"  # more than one mismatch from every whitelist entry
        r1_sequence = barcode + "ACGTACGTACGTACGTACGTACGTACGTAC"
        r1_quality = "I" * len(r1_sequence)
        chunk.append(
            f"@read{pair_index:09d}/1\n{r1_sequence}\n+\n{r1_quality}\n"
            f"@read{pair_index:09d}/2\n{r2_sequence}\n+\n{r2_quality}\n"
        )
        if len(chunk) == 4096:
            output.write("".join(chunk))
            output.flush()
            chunk.clear()
            if pause_ms:
                time.sleep(pause_ms / 1000)
    if chunk:
        output.write("".join(chunk))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--samples", type=int, default=12)
    parser.add_argument("--pairs", type=int, default=0)
    parser.add_argument("--barcodes", type=pathlib.Path, required=True)
    parser.add_argument("--pause-ms", type=float, default=0)
    parser.add_argument("--emit-only", action="store_true")
    parser.add_argument("--well-spaced", action="store_true")
    parser.add_argument("--mixed", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.samples <= 4**10:
        parser.error("samples must be between 1 and 1,048,576")
    if args.pairs < 0:
        parser.error("pairs cannot be negative")
    if args.mixed and (not args.well_spaced or args.samples < 3):
        parser.error("--mixed requires --well-spaced and at least 3 samples")
    barcodes = barcode_set(args.samples, args.well_spaced)
    if not args.emit_only:
        write_map(args.barcodes, barcodes)
    emit(args.pairs, barcodes, args.pause_ms, args.mixed)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
