"""Second pass: turn raw prompt cosines into calibrated zero-shot tags.

`clap_windows.py` stores an unthresholded cosine against every prompt and
leaves `zero_shot_tags` empty, because deciding a label needs statistics the
first window cannot have. Measured over the pilot, CLAP's cosine scale is
strongly label-dependent — "bicycle" has a median of 0.263 while "laughter"
sits at -0.064 — so a single global floor fires some labels constantly and
others never.

The fix is to score each label against *its own* distribution over this
corpus: a tag is emitted when the window is an outlier for that label
(z >= Z_MIN) and the raw cosine clears a floor. "Nothing applies here" is then
representable, which a per-window softmax could never express.

Calibration is written to `tag_calibration.json` and hashed into every
rewritten record's config hash, so a threshold change is visible as a model
change and the affected records can be reprocessed.

    python tag_calibrate.py ../data/prepared/9561 --z 3.0 --floor 0.25
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

from common import config_hash, progress, sha256_file

MAX_TAGS_PER_WINDOW = 5


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("prepared_dir", type=Path, help="e.g. ../data/prepared/9561")
    ap.add_argument("--prompts", type=Path, default=Path(__file__).parent / "prompt_bank.json")
    ap.add_argument("--z", type=float, default=3.0, help="std devs above a label's own mean")
    ap.add_argument("--floor", type=float, default=0.25, help="absolute cosine floor")
    args = ap.parse_args()

    bank = json.loads(args.prompts.read_text())
    labels = [p["label"] for p in bank["prompts"]]
    acoustic_dir = args.prepared_dir / "acoustic"
    batches = sorted(acoustic_dir.glob("*.jsonl"))
    if not batches:
        raise SystemExit(f"no acoustic batches under {acoustic_dir}")

    # pass 1: accumulate per-label mean and variance over every window
    progress(f"calibrate: reading {len(batches)} batches")
    n = 0
    total = np.zeros(len(labels), dtype=np.float64)
    total_sq = np.zeros(len(labels), dtype=np.float64)
    for path in batches:
        for line in path.open():
            cos = json.loads(line).get("prompt_cosines")
            if not cos or len(cos) != len(labels):
                continue
            c = np.asarray(cos, dtype=np.float64)
            total += c
            total_sq += c * c
            n += 1
    if n == 0:
        raise SystemExit("no windows carried prompt_cosines; rerun clap_windows.py")

    mean = total / n
    var = np.maximum(total_sq / n - mean * mean, 1e-12)
    std = np.sqrt(var)
    progress(f"calibrate: {n} windows")

    calibration = {
        "windows": n,
        "z_min": args.z,
        "cosine_floor": args.floor,
        "max_tags_per_window": MAX_TAGS_PER_WINDOW,
        "prompt_bank_blake3": sha256_file(args.prompts),
        "labels": {
            label: {"mean": round(float(mean[i]), 5), "std": round(float(std[i]), 5)}
            for i, label in enumerate(labels)
        },
    }
    calib_path = args.prepared_dir / "tag_calibration.json"
    calib_path.write_text(json.dumps(calibration, indent=2, sort_keys=True) + "\n")
    calib_hash = config_hash(calibration)
    progress(f"calibrate: wrote {calib_path}")

    print(f"\n{'label':24} {'mean':>8} {'std':>8} {'threshold':>10}")
    order = np.argsort(-(mean + args.z * std))
    for i in order:
        thr = max(args.floor, mean[i] + args.z * std[i])
        print(f"{labels[i]:24} {mean[i]:8.3f} {std[i]:8.3f} {thr:10.3f}")

    # pass 2: rewrite each batch with tags filled in, then re-checksum
    speech_idx = labels.index("speech") if "speech" in labels else None
    tagged_windows = 0
    tag_counts: dict[str, int] = {}

    for path in batches:
        records = [json.loads(line) for line in path.open()]
        for r in records:
            cos = r.get("prompt_cosines")
            if not cos or len(cos) != len(labels):
                continue
            c = np.asarray(cos, dtype=np.float64)
            z = (c - mean) / std
            keep = np.where((z >= args.z) & (c >= args.floor))[0]
            keep = keep[np.argsort(-z[keep])][:MAX_TAGS_PER_WINDOW]
            tags = [
                {
                    "label": labels[i],
                    # the stored score is the label-relative z, which is what
                    # the threshold was actually applied to
                    "score": round(float(z[i]), 4),
                }
                for i in keep
            ]
            r["zero_shot_tags"] = tags
            if tags:
                tagged_windows += 1
            for t in tags:
                tag_counts[t["label"]] = tag_counts.get(t["label"], 0) + 1
            if speech_idx is not None:
                # a label-relative speech prior, mapped into 0..1 for display;
                # the authoritative decision remains the VAD's
                r["speech_probability"] = round(
                    float(1.0 / (1.0 + np.exp(-z[speech_idx]))), 5
                )
            # the calibration is part of how this record was produced
            r["model"]["config_hash"] = config_hash(
                {"clap": r["model"]["config_hash"], "tags": calib_hash}
            )

        tmp = path.with_suffix(".jsonl.tmp")
        with tmp.open("w") as f:
            for r in records:
                f.write(json.dumps(r, separators=(",", ":")) + "\n")
        tmp.replace(path)
        # rebuilt from the stem rather than with_suffix, so an asset id
        # containing a dot could not silently retarget the marker
        (path.parent / f"{path.stem}.ready").write_text(sha256_file(path) + "\n")

    progress(
        f"\ncalibrate: {tagged_windows}/{n} windows carry at least one tag "
        f"({100 * tagged_windows / n:.1f}%) — the rest are represented by their "
        f"embedding alone, which is the point"
    )
    for label, count in sorted(tag_counts.items(), key=lambda kv: -kv[1])[:25]:
        progress(f"   {label:24} {count:6} ({100 * count / n:.2f}% of windows)")


if __name__ == "__main__":
    main()
