#!/usr/bin/env python3
"""End-to-end acceptance checks without third-party Python packages."""

import hashlib
import json
import pathlib
import random
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
BARCODES = ROOT / "fixtures" / "barcodes.tsv"


def run(binary: pathlib.Path, data: bytes, output: pathlib.Path, *, max_open: int = 24):
    return run_with_map(binary, BARCODES, data, output, max_open=max_open)


def run_with_map(
    binary: pathlib.Path,
    barcode_map: pathlib.Path,
    data: bytes,
    output: pathlib.Path,
    *,
    max_open: int = 24,
):
    return subprocess.run(
        [
            binary,
            "--barcodes",
            barcode_map,
            "--out",
            output,
            "--max-open",
            str(max_open),
        ],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def checksums(directory: pathlib.Path) -> dict[str, str]:
    return {
        path.name: hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(directory.iterdir())
        if path.is_file()
    }


def pair(name: str, r1: str, r2: str = "ACGT") -> bytes:
    return (
        f"@{name}/1\n{r1}\n+\n{'I' * len(r1)}\n"
        f"@{name}/2\n{r2}\n+\n{'I' * len(r2)}\n"
    ).encode("ascii")


def main() -> int:
    binary = pathlib.Path(sys.argv[1]).resolve()
    clean = (ROOT / "fixtures" / "clean.fastq").read_bytes()
    mixed = (ROOT / "fixtures" / "mixed.fastq").read_bytes()

    with tempfile.TemporaryDirectory(prefix="fastq-spill-test-") as temp_text:
        temp = pathlib.Path(temp_text)
        baseline_out = temp / "baseline"
        baseline = subprocess.run(
            [sys.executable, ROOT / "baseline.py", "--barcodes", BARCODES, "--out", baseline_out],
            input=clean,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        assert baseline.returncode == 0, baseline.stderr

        rust_out = temp / "rust-clean"
        result = run(binary, clean, rust_out)
        assert result.returncode == 0, result.stderr
        for filename in ["alpha.fastq", "beta.fastq", "gamma.fastq", "delta.fastq", "unmatched.fastq"]:
            assert (rust_out / filename).read_bytes() == (baseline_out / filename).read_bytes(), filename

        mixed_out = temp / "mixed"
        result = run(binary, mixed, mixed_out)
        assert result.returncode == 0, result.stderr
        manifest = json.loads((mixed_out / "manifest.json").read_text())
        assert manifest["total_pairs"] == 4
        assert manifest["exact_pairs"] == 1
        assert manifest["corrected_pairs"] == 1
        assert manifest["ambiguous_pairs"] == 1
        assert manifest["unmatched_pairs"] == 1
        assert sum(item["pairs"] for item in manifest["samples"]) == 2
        assert b"@corrected/1" in (mixed_out / "gamma.fastq").read_bytes()
        assert b"@tie/1" in (mixed_out / "ambiguous.fastq").read_bytes()

        rng = random.Random(20260805)
        mutated = list("CCCCCCCCCC")
        position = rng.randrange(10)
        mutated[position] = rng.choice([base for base in "ACGT" if base != mutated[position]])
        seeded_out = temp / "seeded-mutation"
        seeded_data = pair("seeded", "".join(mutated) + "ACGT")
        result = run(binary, seeded_data, seeded_out)
        assert result.returncode == 0, result.stderr
        seeded_manifest = json.loads((seeded_out / "manifest.json").read_text())
        assert seeded_manifest["corrected_pairs"] == 1
        assert (seeded_out / "gamma.fastq").read_bytes() == seeded_data

        malformed = {
            "truncated": (
                (ROOT / "fixtures" / "truncated.fastq").read_bytes(),
                b"line 8:",
            ),
            "length": (
                (ROOT / "fixtures" / "unequal.fastq").read_bytes(),
                b"line 4:",
            ),
            "pair-id": (
                (ROOT / "fixtures" / "mismatched.fastq").read_bytes(),
                b"line 5:",
            ),
        }
        for name, (data, line_marker) in malformed.items():
            output = temp / f"bad-{name}"
            result = run(binary, data, output)
            assert result.returncode != 0, name
            assert line_marker in result.stderr, (name, result.stderr)
            assert not (output / "manifest.json").exists(), name
            for secret in (b"left", b"right", b"AAAAAAAAAA", b"IIII", b"alpha"):
                assert secret not in result.stderr, (name, secret, result.stderr)

        identifier_cases = {
            "empty-id": b"@ /1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@ /2\nACGT\n+\nIIII\n",
            "control-id": b"@hidden\0/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@hidden\0/2\nACGT\n+\nIIII\n",
            "conflicting-role": (
                b"@dual/1 2:N:0:1\nAAAAAAAAAA\n+\nIIIIIIIIII\n"
                b"@dual/2 2:N:0:1\nACGT\n+\nIIII\n"
            ),
        }
        for name, data in identifier_cases.items():
            output = temp / f"bad-{name}"
            result = run(binary, data, output)
            assert result.returncode != 0, name
            assert b"unsupported FASTQ identifier" in result.stderr, (name, result.stderr)
            assert not (output / "manifest.json").exists(), name
            for secret in (b"hidden", b"dual", b"AAAAAAAAAA", b"IIII", b"alpha"):
                assert secret not in result.stderr, (name, secret, result.stderr)

        collision_maps = {
            "case": "AAAAAAAAAA\tAlpha\nCCCCCCCCCC\talpha\n",
            "ambiguous": "AAAAAAAAAA\tAmbiguous\n",
            "unmatched": "AAAAAAAAAA\tUnmatched\n",
        }
        for name, contents in collision_maps.items():
            barcode_map = temp / f"collision-{name}.tsv"
            barcode_map.write_text(contents)
            output = temp / f"collision-{name}"
            result = run_with_map(binary, barcode_map, b"", output)
            assert result.returncode != 0, name
            assert not output.exists(), name
            assert b"Alpha" not in result.stderr and b"Ambiguous" not in result.stderr, result.stderr

        first = temp / "repeat-a"
        second = temp / "repeat-b"
        assert run(binary, mixed, first, max_open=2).returncode == 0
        assert run(binary, mixed, second, max_open=2).returncode == 0
        assert checksums(first) == checksums(second)

        many_map = temp / "many.tsv"
        generator = ROOT / "fixtures" / "generate.py"
        subprocess.run(
            [sys.executable, generator, "--samples", "30", "--pairs", "0", "--barcodes", many_map],
            check=True,
        )
        many_input = b"".join(pair(f"p{index}", _barcode(index)) for index in range(30))
        many_out = temp / "many"
        result = subprocess.run(
            [binary, "--barcodes", many_map, "--out", many_out, "--max-open", "3"],
            input=many_input,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        assert json.loads((many_out / "manifest.json").read_text())["max_open_writers"] == 3

    print("integration checks passed")
    return 0


def _barcode(index: int) -> str:
    bases = "ACGT"
    chars = ["A"] * 10
    for position in range(9, -1, -1):
        chars[position] = bases[index & 3]
        index >>= 2
    return "".join(chars)


if __name__ == "__main__":
    raise SystemExit(main())
