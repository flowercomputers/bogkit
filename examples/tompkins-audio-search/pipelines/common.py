"""Shared plumbing for the inference workers.

Three concerns live here, all of them about not lying to the store:

**Decoding.** Every model gets audio decoded the same way, from the compacted
assets, at the sample rate it expects. The archive is stereo AAC at 48 kHz;
CLAP and BirdNET want mono, so the downmix is an explicit step rather than an
assumption buried in a loader.

**Provenance.** Every record carries a `ModelStamp`: model name and version,
checkpoint hash, pipeline-config hash, and a hash of the decoded audio the
model actually saw. The input hash matters because a re-remux can change the
bytes without the checkpoint moving, and we need to notice.

**Resumability.** Output is written to a `.tmp` file, checksummed, then
atomically renamed to `.ready`. A crash therefore loses at most one asset, and
a half-written batch can never be mistaken for a complete one.

Nothing here touches the Bog store. Workers stage batches; the Rust side owns
the write lock and commits them.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from dataclasses import dataclass, asdict
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

# 90 kHz MPEG-TS ticks, matching src/timeline.rs
TICKS_PER_SECOND = 90_000


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def config_hash(config: dict) -> str:
    """Hash of the pipeline configuration, so a threshold change is visible."""
    return sha256_bytes(json.dumps(config, sort_keys=True).encode())


@dataclass
class ModelStamp:
    model_name: str
    model_version: str
    checkpoint_hash: str
    config_hash: str
    input_hash: str
    created_at_utc: str

    def to_dict(self) -> dict:
        return asdict(self)


# ---------------------------------------------------------------------------
# audio
# ---------------------------------------------------------------------------


def decode_asset(path: Path, sample_rate: int, mono: bool = True) -> np.ndarray:
    """Decode a compacted asset to float32 PCM via ffmpeg.

    ffmpeg rather than a Python decoder because it is the same binary that
    produced the asset, so a container quirk cannot show up as a silent
    difference between what we indexed and what the browser plays.

    Mono means *one channel*, not `(L+R)/2`. Large stretches of this archive
    are recorded with the channels partly phase-inverted — measured L/R
    correlation runs to -0.95 — and summing those cancels the content instead
    of averaging it. On one 20-second span the downmix sat 9.3 dB below the
    left channel alone, and the 500-2000 Hz band where speech lives fell from
    6.6% of the energy to 0.01%. Every model was being fed the cancellation
    residue. Taking a single channel costs almost nothing when the channels
    agree, and preserves the signal when they do not.
    """
    cmd = [
        "ffmpeg", "-hide_banner", "-loglevel", "error",
        "-i", str(path),
        "-f", "f32le", "-acodec", "pcm_f32le",
    ]
    cmd += ["-af", "pan=mono|c0=c0"] if mono else ["-ac", "2"]
    cmd += ["-ar", str(sample_rate), "-"]
    proc = subprocess.run(cmd, capture_output=True, check=True)
    audio = np.frombuffer(proc.stdout, dtype=np.float32)
    if not mono:
        audio = audio.reshape(-1, 2)
    return audio


def rms_dbfs(x: np.ndarray) -> float:
    """Level in dBFS, floored so silence does not become -inf in JSON."""
    if x.size == 0:
        return -120.0
    r = float(np.sqrt(np.mean(np.square(x, dtype=np.float64))))
    return max(-120.0, 20.0 * np.log10(r)) if r > 0 else -120.0


# ---------------------------------------------------------------------------
# assets
# ---------------------------------------------------------------------------


@dataclass
class Asset:
    asset_id: str
    file: Path
    first_media_sequence: int
    last_media_sequence: int
    stream_start_ticks: int
    stream_end_ticks: int

    @property
    def stream_start_ms(self) -> int:
        return self.stream_start_ticks // 90

    def stream_ms(self, offset_seconds: float) -> int:
        """Asset-local seconds -> stream-relative milliseconds.

        The single most important conversion in the pipeline: an off-by-one
        here silently mislabels every timestamp in the asset.
        """
        return self.stream_start_ms + int(round(offset_seconds * 1000.0))


def load_assets(stream_id: int, asset_dir: Path) -> list[Asset]:
    """Read the asset table, preferring the timeline-resolved one.

    `tools/compact.mjs` records each asset's position relative to *the fetch*.
    That equals its position in the stream only when the fetch began at segment
    0; fetching a window out of the middle of a stream leaves every asset
    reporting a start of 0, and every timestamp derived from it is wrong by
    however far into the stream the window really sits.

    `data/timeline/assets-{id}.json`, written by the Rust `timeline` command,
    carries the positions resolved against the decoded timeline. When it is
    present it wins.
    """
    resolved = _load_resolved(stream_id, asset_dir)
    if resolved:
        return resolved
    return _load_fetch_relative(stream_id, asset_dir)


def _load_resolved(stream_id: int, asset_dir: Path) -> list[Asset]:
    # asset_dir is .../data/assets/{id}, so the timeline sits two levels up
    path = asset_dir.parent.parent / "timeline" / f"assets-{stream_id}.json"
    if not path.exists():
        return []
    doc = json.loads(path.read_text())
    out: list[Asset] = []
    for a in doc.get("assets", []):
        file = asset_dir / f"{a['assetId']}.m4a"
        if not file.exists():
            continue          # resolved but not fetched; nothing to analyse
        out.append(
            Asset(
                asset_id=a["assetId"],
                file=file,
                first_media_sequence=a["firstMediaSequence"],
                last_media_sequence=a["lastMediaSequence"],
                stream_start_ticks=a["streamStartMs"] * 90,
                stream_end_ticks=a["streamEndMs"] * 90,
            )
        )
    out.sort(key=lambda a: a.stream_start_ticks)
    return out


def _load_fetch_relative(stream_id: int, asset_dir: Path) -> list[Asset]:
    """Fallback: the table compaction wrote, positions relative to the fetch."""
    candidates = sorted(asset_dir.glob("assets-*.json"))
    if not candidates:
        raise SystemExit(f"no assets-*.json in {asset_dir}; run tools/compact.mjs first")
    assets: list[Asset] = []
    for path in candidates:
        doc = json.loads(path.read_text())
        if doc.get("streamId") != stream_id:
            continue
        for a in doc["assets"]:
            # compact.mjs records the path relative to its own working
            # directory; the workers run from elsewhere, so re-anchor on the
            # asset directory we were actually given
            recorded = Path(a["file"])
            file = recorded if recorded.is_absolute() else asset_dir / recorded.name
            if not file.exists() and recorded.exists():
                file = recorded
            assets.append(
                Asset(
                    asset_id=a["assetId"],
                    file=file,
                    first_media_sequence=a["firstMediaSequence"],
                    last_media_sequence=a["lastMediaSequence"],
                    stream_start_ticks=a["streamStartTicks"],
                    stream_end_ticks=a["streamEndTicks"],
                )
            )
    # Reject colliding definitions rather than picking one. Two asset tables
    # claiming the same id with different segment ranges means a stale run is
    # still on disk, and quietly choosing either would index the wrong audio
    # under the right-looking timestamps.
    by_id: dict[str, Asset] = {}
    for a in assets:
        prior = by_id.get(a.asset_id)
        if prior is None:
            by_id[a.asset_id] = a
            continue
        if (prior.first_media_sequence, prior.last_media_sequence) != (
            a.first_media_sequence,
            a.last_media_sequence,
        ):
            raise SystemExit(
                f"asset id {a.asset_id} is defined twice with different segment ranges "
                f"({prior.first_media_sequence}..{prior.last_media_sequence} vs "
                f"{a.first_media_sequence}..{a.last_media_sequence}). remove the stale "
                f"assets-*.json in {asset_dir} before continuing."
            )
    assets = sorted(by_id.values(), key=lambda a: a.stream_start_ticks)
    return assets


# ---------------------------------------------------------------------------
# staged output
# ---------------------------------------------------------------------------


class BatchWriter:
    """Write one asset's records as a staged, checksummed, atomic batch.

    Layout under `out_dir`:

        {track}/{asset_id}.jsonl.tmp     being written
        {track}/{asset_id}.jsonl         complete
        {track}/{asset_id}.ready        sha256 of the .jsonl

    The Rust committer only reads batches that have a matching `.ready`, so a
    truncated file is never committed.
    """

    def __init__(self, out_dir: Path, track: str, asset_id: str):
        self.dir = out_dir / track
        self.dir.mkdir(parents=True, exist_ok=True)
        self.final = self.dir / f"{asset_id}.jsonl"
        self.tmp = self.dir / f"{asset_id}.jsonl.tmp"
        self.ready = self.dir / f"{asset_id}.ready"
        self.count = 0

    def is_done(self) -> bool:
        """True when a complete, checksum-matching batch already exists."""
        if not (self.final.exists() and self.ready.exists()):
            return False
        return self.ready.read_text().strip() == sha256_file(self.final)

    def __enter__(self):
        self._f = open(self.tmp, "w")
        return self

    def write(self, record: dict) -> None:
        self._f.write(json.dumps(record, separators=(",", ":")) + "\n")
        self.count += 1

    def __exit__(self, exc_type, exc, tb):
        self._f.close()
        if exc_type is not None:
            self.tmp.unlink(missing_ok=True)
            return False
        os.replace(self.tmp, self.final)
        self.ready.write_text(sha256_file(self.final) + "\n")
        return False


def pick_device() -> str:
    """MPS when available, else CPU. Reported so runs are comparable."""
    import torch

    if torch.backends.mps.is_available():
        return "mps"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


def progress(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)
