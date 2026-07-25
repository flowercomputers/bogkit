"""BirdNET species detections over 3-second frames.

BirdNET is conditioned on where and when: passing Tompkins Square Park's
coordinates and the recording's week of year narrows the candidate species
list to what could plausibly be there. The archive is April 2021 — ISO weeks
11–19 — which is a real constraint, so the priors are worth using. Whether
they were applied is recorded on every detection, because a detection made
with a location prior is not the same evidence as one made without.

Embeddings are stored only for bird-positive frames in this first version: an
embedding per 3 seconds across 1,237 hours would dominate the index for little
retrieval benefit until evaluation shows otherwise.

The species id is derived from the scientific name, which is stable across
BirdNET label-file revisions in a way common names are not.

    python bird_frames.py 9561 --asset-dir ../data/assets/9561 --out ../data/prepared/9561

Licence note: BirdNET's model is CC BY-NC-SA. Fine for research and hackathon
use; a commercial deployment needs a licence review or a different bird model.
The interface here is deliberately thin so the model can be swapped.
"""

from __future__ import annotations

import argparse
import json
import re
import warnings
from pathlib import Path

from common import (
    BatchWriter,
    ModelStamp,
    config_hash,
    load_assets,
    progress,
    sha256_bytes,
    sha256_file,
    utc_now,
)

warnings.filterwarnings("ignore")

# Tompkins Square Park, Manhattan.
LATITUDE = 40.7265
LONGITUDE = -73.9815

FRAME_SECONDS = 3.0
# BirdNET's own default; detections below this are noise on urban recordings.
MIN_CONFIDENCE = 0.25


def species_id(scientific_name: str) -> str:
    """Mirror of `store::species_id` in the Rust crate."""
    return re.sub(r"[^a-z0-9]", "_", scientific_name.strip().lower())


def iso_week_of(date_str: str | None) -> int | None:
    """ISO week from a `YYYY-MM-DD` recording-date estimate."""
    if not date_str:
        return None
    from datetime import date

    try:
        y, m, d = (int(p) for p in date_str[:10].split("-"))
        return date(y, m, d).isocalendar().week
    except Exception:
        return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("stream_id", type=int)
    ap.add_argument("--asset-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument(
        "--recording-date",
        default="2021-04-27",
        help="estimated recording date, for BirdNET's week prior. this corpus "
        "has only S3-derived approximations, so it is an estimate by nature.",
    )
    ap.add_argument("--min-confidence", type=float, default=MIN_CONFIDENCE)
    ap.add_argument("--no-priors", action="store_true", help="disable location/week priors")
    ap.add_argument("--limit-assets", type=int, default=0)
    # off by default: broken on this birdnetlib/TensorFlow build, and only
    # needed for bird audio-similarity search
    ap.add_argument("--embeddings", action="store_true", default=False)
    args = ap.parse_args()

    from birdnetlib import Recording
    from birdnetlib.analyzer import Analyzer

    week = iso_week_of(args.recording_date)
    use_priors = not args.no_priors
    progress(
        f"birdnet: priors={'on' if use_priors else 'off'} "
        f"lat={LATITUDE} lon={LONGITUDE} week={week} min_conf={args.min_confidence}"
    )

    analyzer = Analyzer()
    model_path = Path(getattr(analyzer, "model_path", "") or "")
    checkpoint_hash = (
        sha256_file(model_path)
        if model_path.exists() and model_path.is_file()
        else sha256_bytes(str(getattr(analyzer, "version", "birdnet")).encode())
    )

    cfg = {
        "frame_seconds": FRAME_SECONDS,
        "min_confidence": args.min_confidence,
        "latitude": LATITUDE if use_priors else None,
        "longitude": LONGITUDE if use_priors else None,
        "week": week if use_priors else None,
        "recording_date": args.recording_date,
        "embeddings": bool(args.embeddings),
    }
    cfg_hash = config_hash(cfg)

    assets = load_assets(args.stream_id, args.asset_dir)
    if args.limit_assets:
        assets = assets[: args.limit_assets]
    progress(f"birdnet: {len(assets)} assets to process")

    total = 0
    embed_warned = False
    species_seen: dict[str, int] = {}

    for i, asset in enumerate(assets):
        writer = BatchWriter(args.out, "bird", asset.asset_id)
        if writer.is_done():
            progress(f"birdnet: {asset.asset_id} already done, skipping")
            continue

        stamp = ModelStamp(
            model_name="birdnet-analyzer",
            model_version=str(getattr(analyzer, "version", "unknown")),
            checkpoint_hash=checkpoint_hash,
            config_hash=cfg_hash,
            # BirdNET reads the asset directly, so the asset file *is* the input
            input_hash=sha256_file(asset.file),
            created_at_utc=utc_now(),
        ).to_dict()

        kwargs: dict = {"min_conf": args.min_confidence}
        if use_priors:
            kwargs.update(lat=LATITUDE, lon=LONGITUDE, week_48=week)

        recording = Recording(analyzer, str(asset.file), **kwargs)
        recording.analyze()
        # `extract_embeddings` is a *method*, not a flag — assigning a bool to
        # it silently replaces the method and yields no vectors at all. But
        # calling it after `analyze()` fails on this birdnetlib/TensorFlow build
        # ("Tensor data is null"), so it is opt-in and never fatal: species
        # detection is the point, and audio-similarity search over bird frames
        # is explicitly optional in the first version. Losing an optional
        # feature must not cost the detections, or the speech pass behind it.
        if args.embeddings:
            try:
                recording.extract_embeddings()
            except Exception as e:
                if not embed_warned:
                    progress(f"birdnet: embeddings unavailable ({type(e).__name__}: {e}); "
                             f"continuing with detections only")
                    embed_warned = True

        # embeddings come back as their own list of 3-second frames; index
        # them by start time so a detection can find its own frame
        embed_by_start: dict[int, list[float]] = {}
        for e in getattr(recording, "embeddings", None) or []:
            start = int(round(float(e.get("start_time", -1)) * 1000))
            vec = e.get("embeddings") or e.get("embedding")
            if vec is not None:
                embed_by_start[start] = [round(float(v), 5) for v in vec]

        with writer as w:
            for d in recording.detections:
                start_s = float(d["start_time"])
                end_s = float(d["end_time"])
                sci = d["scientific_name"]
                sid = species_id(sci)
                key = int(round(start_s * 1000))
                w.write(
                    {
                        "kind": "bird",
                        "stream_id": args.stream_id,
                        "start_ms": asset.stream_ms(start_s),
                        "end_ms": asset.stream_ms(end_s),
                        "species_id": sid,
                        "scientific_name": sci,
                        "common_name": d["common_name"],
                        "confidence": round(float(d["confidence"]), 5),
                        "birdnet_embedding": embed_by_start.get(key),
                        "location_prior_used": use_priors,
                        "week_prior_used": use_priors and week is not None,
                        "asset_id": asset.asset_id,
                        "asset_offset_ms": key,
                        "model": stamp,
                    }
                )
                species_seen[d["common_name"]] = species_seen.get(d["common_name"], 0) + 1
            total += w.count

        progress(
            f"birdnet: [{i + 1}/{len(assets)}] {asset.asset_id} -> {writer.count} detections "
            f"({total} total, {len(species_seen)} species)"
        )

    progress(f"birdnet: done, {total} detections, {len(species_seen)} species")
    for name, n in sorted(species_seen.items(), key=lambda kv: -kv[1])[:25]:
        progress(f"   {name:38} {n}")

    summary = args.out / "bird-summary.json"
    summary.write_text(
        json.dumps(
            {"total": total, "species": species_seen, "config": cfg}, indent=2, sort_keys=True
        )
        + "\n"
    )
    progress(f"birdnet: wrote {summary}")


if __name__ == "__main__":
    main()
