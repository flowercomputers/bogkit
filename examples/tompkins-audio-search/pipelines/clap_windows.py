"""CLAP acoustic windows: the general-purpose sound representation.

Windows are 10 s with a 5 s hop — model-appropriate rather than
storage-appropriate. Indexing each 2-second HLS segment instead would give the
model too little acoustic context and would couple model identity to how the
archive happens to be chunked.

The embedding is the primary artifact. Zero-shot tags from the controlled
prompt bank are a convenience layer on top, recorded only when they clear a
threshold, so a moment is never unsearchable just because no prompt fired.

    python clap_windows.py 9561 --asset-dir ../data/assets/9561 --out ../data/prepared/9561

Usage note: the audio tower expects 48 kHz mono, and the archive is stereo, so
the downmix happens in `common.decode_asset` where it is visible.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import torch

from common import (
    Asset,
    BatchWriter,
    ModelStamp,
    config_hash,
    decode_asset,
    load_assets,
    pick_device,
    progress,
    rms_dbfs,
    sha256_bytes,
    utc_now,
)

CHECKPOINT = "laion/clap-htsat-unfused"
SAMPLE_RATE = 48_000
WINDOW_SECONDS = 10.0
HOP_SECONDS = 5.0
EMBED_DIM = 512


def l2_normalize(x: np.ndarray) -> np.ndarray:
    n = np.linalg.norm(x, axis=-1, keepdims=True)
    return x / np.maximum(n, 1e-12)


def joint_embedding(out) -> torch.Tensor:
    """Pull the 512-dim joint-space embedding out of a CLAP feature call.

    transformers 5.x returns `BaseModelOutputWithPooling` from
    `get_{text,audio}_features`, whose `pooler_output` is the projected joint
    embedding; earlier versions returned that tensor directly. Handling both
    keeps this worker from being pinned to one transformers minor version.
    """
    if isinstance(out, torch.Tensor):
        return out
    pooled = getattr(out, "pooler_output", None)
    if pooled is not None:
        return pooled
    for name in ("text_embeds", "audio_embeds"):
        v = getattr(out, name, None)
        if v is not None:
            return v
    raise TypeError(f"cannot find a joint embedding in {type(out).__name__}")


class Clap:
    def __init__(self, device: str):
        from transformers import AutoProcessor, ClapModel

        self.device = device
        self.processor = AutoProcessor.from_pretrained(CHECKPOINT)
        self.model = ClapModel.from_pretrained(CHECKPOINT).to(device).eval()
        # a stable identity for the weights actually loaded
        self.checkpoint_hash = sha256_bytes(
            json.dumps(
                {
                    "checkpoint": CHECKPOINT,
                    "config": self.model.config.to_dict(),
                },
                sort_keys=True,
                default=str,
            ).encode()
        )

    @torch.no_grad()
    def embed_audio(self, windows: list[np.ndarray]) -> np.ndarray:
        inputs = self.processor(
            audio=windows, sampling_rate=SAMPLE_RATE, return_tensors="pt", padding=True
        )
        inputs = {k: v.to(self.device) for k, v in inputs.items()}
        feats = joint_embedding(self.model.get_audio_features(**inputs))
        return l2_normalize(feats.float().cpu().numpy())

    @torch.no_grad()
    def embed_text(self, texts: list[str]) -> np.ndarray:
        inputs = self.processor(text=texts, return_tensors="pt", padding=True)
        inputs = {k: v.to(self.device) for k, v in inputs.items()}
        feats = joint_embedding(self.model.get_text_features(**inputs))
        return l2_normalize(feats.float().cpu().numpy())

    @property
    def logit_scale(self) -> float:
        return float(self.model.logit_scale_a.exp().detach().cpu())


def window_bounds(n_samples: int) -> list[tuple[int, int]]:
    """Window start/end sample pairs.

    The tail is kept only if it holds at least half a window: a 1-second
    fragment embedded as if it were 10 seconds is a misleading vector.
    """
    win = int(WINDOW_SECONDS * SAMPLE_RATE)
    hop = int(HOP_SECONDS * SAMPLE_RATE)
    out = []
    start = 0
    while start < n_samples:
        end = min(start + win, n_samples)
        if end - start >= win // 2:
            out.append((start, end))
        if end >= n_samples:
            break
        start += hop
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("stream_id", type=int)
    ap.add_argument("--asset-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--prompts", type=Path, default=Path(__file__).parent / "prompt_bank.json")
    ap.add_argument("--batch", type=int, default=12)
    ap.add_argument("--limit-assets", type=int, default=0, help="0 = all")
    args = ap.parse_args()

    bank = json.loads(args.prompts.read_text())
    labels = [p["label"] for p in bank["prompts"]]
    prompt_texts = [p["text"] for p in bank["prompts"]]
    min_prob = bank["min_probability"]
    min_cos = bank["min_cosine"]

    cfg = {
        "checkpoint": CHECKPOINT,
        "sample_rate": SAMPLE_RATE,
        "window_seconds": WINDOW_SECONDS,
        "hop_seconds": HOP_SECONDS,
        "mono": True,
        "prompt_bank": bank["prompts"],
        "min_probability": min_prob,
        "min_cosine": min_cos,
    }
    cfg_hash = config_hash(cfg)

    device = pick_device()
    progress(f"clap: loading {CHECKPOINT} on {device}")
    clap = Clap(device)
    text_embeds = clap.embed_text(prompt_texts)  # (P, 512)
    scale = clap.logit_scale
    progress(f"clap: prompt bank of {len(labels)} prompts embedded, logit_scale={scale:.2f}")

    assets = load_assets(args.stream_id, args.asset_dir)
    if args.limit_assets:
        assets = assets[: args.limit_assets]
    progress(f"clap: {len(assets)} assets to process")

    total_windows = 0
    for i, asset in enumerate(assets):
        writer = BatchWriter(args.out, "acoustic", asset.asset_id)
        if writer.is_done():
            progress(f"clap: {asset.asset_id} already done, skipping")
            continue

        audio = decode_asset(asset.file, SAMPLE_RATE, mono=True)
        # the input hash covers exactly the samples the model sees
        input_hash = sha256_bytes(audio.tobytes())
        stamp = ModelStamp(
            model_name="laion-clap-htsat-unfused",
            model_version="transformers-ClapModel",
            checkpoint_hash=clap.checkpoint_hash,
            config_hash=cfg_hash,
            input_hash=input_hash,
            created_at_utc=utc_now(),
        ).to_dict()

        bounds = window_bounds(len(audio))
        with writer as w:
            for b0 in range(0, len(bounds), args.batch):
                chunk = bounds[b0 : b0 + args.batch]
                windows = [audio[s:e] for s, e in chunk]
                embeds = clap.embed_audio(windows)  # (B, 512)

                # Raw cosine against every prompt, stored unthresholded.
                #
                # Tagging deliberately does not happen here. CLAP's cosine
                # scale is strongly label-dependent — measured over this
                # corpus, "bicycle" has a median of 0.263 while "laughter"
                # sits at -0.064 — so no single floor is fair across labels,
                # and a softmax over the bank is worse still because it
                # normalises per window and therefore cannot represent "none
                # of these apply". Deciding a label needs statistics over the
                # corpus, which the first window cannot have. `tag_calibrate.py`
                # is the second pass that fills these in.
                cos = embeds @ text_embeds.T  # (B, P)

                for row, (s, e) in enumerate(chunk):
                    start_ms = asset.stream_ms(s / SAMPLE_RATE)
                    end_ms = asset.stream_ms(e / SAMPLE_RATE)
                    w.write(
                        {
                            "kind": "acoustic",
                            "stream_id": args.stream_id,
                            "start_ms": start_ms,
                            "end_ms": end_ms,
                            "clap_embedding": [round(float(v), 6) for v in embeds[row]],
                            "prompt_cosines": [round(float(v), 5) for v in cos[row]],
                            "zero_shot_tags": [],
                            "rms_dbfs": round(rms_dbfs(audio[s:e]), 2),
                            # filled by the calibration pass, from the speech
                            # prompt; the authoritative decision is the VAD's
                            "speech_probability": 0.0,
                            "asset_id": asset.asset_id,
                            "asset_offset_ms": int(round(s / SAMPLE_RATE * 1000)),
                            "model": stamp,
                        }
                    )
            total_windows += w.count
        progress(
            f"clap: [{i + 1}/{len(assets)}] {asset.asset_id} -> {writer.count} windows "
            f"({total_windows} total)"
        )

    progress(f"clap: done, {total_windows} windows across {len(assets)} assets")


if __name__ == "__main__":
    main()
