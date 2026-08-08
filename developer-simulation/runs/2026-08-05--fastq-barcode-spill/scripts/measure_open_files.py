#!/usr/bin/env python3
"""Observe the release process with lsof during a throttled streamed run."""

import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1]
    default_binary = root.parents[1] / "target" / "release" / "fastq-barcode-spill"
    binary = pathlib.Path(os.environ.get("FASTQ_BINARY", default_binary)).resolve()
    generator = root / "fixtures" / "generate.py"
    lsof_command = os.environ.get("LSOF_COMMAND", "lsof")
    pair_count = int(os.environ.get("FASTQ_MEASURE_PAIRS", "200000"))

    with tempfile.TemporaryDirectory(prefix="fastq-open-files-", dir="/private/tmp") as temporary:
        temp = pathlib.Path(temporary)
        barcode_map = temp / "barcodes.tsv"
        output = temp / "output"
        subprocess.run(
            [
                sys.executable,
                generator,
                "--samples",
                "384",
                "--pairs",
                "0",
                "--well-spaced",
                "--barcodes",
                barcode_map,
            ],
            check=True,
        )
        producer = subprocess.Popen(
            [
                sys.executable,
                generator,
                "--samples",
                "384",
                "--pairs",
                str(pair_count),
                "--pause-ms",
                "20",
                "--barcodes",
                barcode_map,
                "--well-spaced",
                "--mixed",
                "--emit-only",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert producer.stdout is not None
        consumer = subprocess.Popen(
            [binary, "--barcodes", barcode_map, "--out", output, "--max-open", "24"],
            stdin=producer.stdout,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        producer.stdout.close()

        observed_max = 0
        polls = 0
        successful_polls = 0
        prefix = f"n{output}/"
        while consumer.poll() is None:
            observation = subprocess.run(
                [lsof_command, "-a", "-p", str(consumer.pid), "-Fn"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                check=False,
            )
            if observation.returncode == 0:
                successful_polls += 1
            open_fastq = sum(
                1
                for line in observation.stdout.splitlines()
                if line.startswith(prefix) and line.endswith(".fastq")
            )
            observed_max = max(observed_max, open_fastq)
            polls += 1
            time.sleep(0.01)

        consumer_stdout, consumer_stderr = consumer.communicate()
        producer_stderr = producer.communicate()[1]
        if producer.returncode != 0 or consumer.returncode != 0:
            print("measurement workload failed", file=sys.stderr)
            if producer_stderr:
                print(producer_stderr.decode("utf-8", "replace"), file=sys.stderr)
            if consumer_stderr:
                print(consumer_stderr.decode("utf-8", "replace"), file=sys.stderr)
            return 1

        manifest = json.loads((output / "manifest.json").read_text())
        print(
            f"observed_max_open_fastq_files={observed_max} "
            f"manifest_max_open_writers={manifest['max_open_writers']} polls={polls} "
            f"successful_polls={successful_polls}"
        )
        if successful_polls == 0 or observed_max == 0:
            print("no valid positive lsof observation", file=sys.stderr)
            return 1
        if observed_max > 24 or manifest["max_open_writers"] > 24:
            return 1
        if f"processed {pair_count} read pairs".encode() not in consumer_stdout:
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
