"""Measure what CLAP zero-shot scores actually look like on this corpus.

The first pilot run exposed a calibration problem worth recording. Scoring the
prompt bank with a softmax forces every window's probabilities to sum to 1, so
a stretch of featureless quiet street noise still produces confident-looking
tags — "bicycle" fired on 42% of windows and *no* window came out untagged.
A threshold on a quantity that is normalised per window cannot express "none of
these prompts apply".

Raw cosine similarity does not have that problem: it is an absolute measure of
how close the audio sits to a prompt, so "nothing matches" is representable.
This script reports the cosine distribution per label over already-computed
window embeddings, which is what a defensible threshold has to be derived from.

    python calibrate_tags.py ../data/prepared/9561/acoustic --top 15
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("acoustic_dir", type=Path)
    ap.add_argument("--prompts", type=Path, default=Path(__file__).parent / "prompt_bank.json")
    ap.add_argument("--top", type=int, default=15)
    ap.add_argument("--max-windows", type=int, default=20_000)
    args = ap.parse_args()

    embeds: list[list[float]] = []
    for path in sorted(args.acoustic_dir.glob("*.jsonl")):
        for line in path.open():
            embeds.append(json.loads(line)["clap_embedding"])
            if len(embeds) >= args.max_windows:
                break
        if len(embeds) >= args.max_windows:
            break
    if not embeds:
        raise SystemExit(f"no window embeddings under {args.acoustic_dir}")
    audio = np.asarray(embeds, dtype=np.float32)
    print(f"{len(audio)} window embeddings, dim {audio.shape[1]}")

    from transformers import AutoProcessor, ClapModel
    import torch

    bank = json.loads(args.prompts.read_text())
    labels = [p["label"] for p in bank["prompts"]]
    texts = [p["text"] for p in bank["prompts"]]

    model = ClapModel.from_pretrained("laion/clap-htsat-unfused").eval()
    proc = AutoProcessor.from_pretrained("laion/clap-htsat-unfused")
    with torch.no_grad():
        out = model.get_text_features(
            **proc(text=texts, return_tensors="pt", padding=True)
        )
        text = getattr(out, "pooler_output", out).float().numpy()
    text /= np.maximum(np.linalg.norm(text, axis=1, keepdims=True), 1e-12)

    cos = audio @ text.T  # (W, P)

    print(f"\noverall cosine: min {cos.min():.3f}  mean {cos.mean():.3f}  max {cos.max():.3f}")
    print(
        "\nper-label cosine distribution "
        "(a label whose p99 barely exceeds its median never discriminates):"
    )
    print(f"{'label':24} {'median':>8} {'p90':>8} {'p99':>8} {'max':>8} {'>0.30':>8}")
    rows = []
    for i, label in enumerate(labels):
        c = cos[:, i]
        rows.append(
            (
                label,
                float(np.median(c)),
                float(np.percentile(c, 90)),
                float(np.percentile(c, 99)),
                float(c.max()),
                int((c > 0.30).sum()),
            )
        )
    for label, med, p90, p99, mx, over in sorted(rows, key=lambda r: -r[4]):
        print(f"{label:24} {med:8.3f} {p90:8.3f} {p99:8.3f} {mx:8.3f} {over:8}")

    # How many windows would carry at least one tag at various cosine floors?
    # A floor that tags everything is as useless as one that tags nothing.
    print("\ncoverage by cosine floor:")
    print(f"{'floor':>8} {'windows tagged':>16} {'share':>8} {'mean tags/window':>18}")
    for floor in [0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45]:
        hit = cos > floor
        tagged = int(hit.any(axis=1).sum())
        print(
            f"{floor:8.2f} {tagged:16} {100 * tagged / len(cos):7.1f}% "
            f"{hit.sum(axis=1).mean():18.2f}"
        )

    # The softmax view, for contrast: it cannot represent "nothing applies".
    scale = float(model.logit_scale_a.exp().detach())
    logits = cos * scale
    probs = np.exp(logits - logits.max(axis=1, keepdims=True))
    probs /= probs.sum(axis=1, keepdims=True)
    print(
        f"\nsoftmax (logit_scale {scale:.1f}): every window sums to 1.0, so "
        f"{100 * (probs.max(axis=1) > 0.12).mean():.0f}% of windows clear a 0.12 "
        "probability threshold regardless of whether anything is really there"
    )


if __name__ == "__main__":
    main()
