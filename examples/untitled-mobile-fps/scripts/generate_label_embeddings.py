#!/usr/bin/env python3
"""Precompute MobileCLIP2-S0 *text* embeddings for outfit labels.

The app embeds a garment crop with the bundled image encoder, then cosine-matches
it against these label vectors to name the top and bottom (zero-shot). Because the
text tower shares MobileCLIP's joint space with the image tower we exported, the
image-vs-text cosine is meaningful. Vectors are unit-length; each label averages a
few prompt templates for robustness.

Output JSON is bundled in the app as OutfitLabels.json.

Usage:
    python generate_label_embeddings.py --out ../UntitledMobileFPS/OutfitLabels.json
"""
from __future__ import annotations

import argparse
import json

import torch
import open_clip


MODEL = "MobileCLIP2-S0"
PRETRAINED = "dfndr2b"
# Must match MobileCLIPEmbedder.modelVersion so the app can reject a mismatch.
MODEL_TAG = "mobileclip2-s0-image-512-v1"

COLORS = ["black", "white", "gray", "red", "orange", "yellow",
          "green", "blue", "purple", "pink", "brown", "beige"]
TOPS = ["t-shirt", "shirt", "hoodie", "sweater", "jacket", "coat", "tank top", "dress"]
BOTTOMS = ["jeans", "trousers", "shorts", "skirt", "leggings"]
TEMPLATES = [
    "a photo of a person wearing a {}",
    "a person in a {}",
    "a close-up photo of a {}",
    "{}",
]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default="../UntitledMobileFPS/OutfitLabels.json")
    args = ap.parse_args()

    print(f"[load] {MODEL} ({PRETRAINED})", flush=True)
    model, _, _ = open_clip.create_model_and_transforms(MODEL, pretrained=PRETRAINED)
    model.eval()
    tokenizer = open_clip.get_tokenizer(MODEL)

    def label_vector(description: str) -> list[float]:
        phrases = [tpl.format(description) for tpl in TEMPLATES]
        with torch.no_grad():
            tokens = tokenizer(phrases)
            embeddings = model.encode_text(tokens)
            embeddings = embeddings / embeddings.norm(dim=-1, keepdim=True)
            mean = embeddings.mean(dim=0)
            mean = mean / mean.norm()
        return [round(float(x), 6) for x in mean.tolist()]

    def build(colors, garments):
        out = []
        for color in colors:
            for garment in garments:
                out.append({
                    "color": color,
                    "garment": garment,
                    "text": f"{color} {garment}",
                    "vector": label_vector(f"{color} {garment}"),
                })
        return out

    tops = build(COLORS, TOPS)
    bottoms = build(COLORS, BOTTOMS)
    dim = len(tops[0]["vector"])
    print(f"[build] {len(tops)} top labels + {len(bottoms)} bottom labels, dim={dim}", flush=True)
    assert dim == 512, f"unexpected text embedding dim {dim}"

    payload = {"model": MODEL_TAG, "dim": dim, "tops": tops, "bottoms": bottoms}
    with open(args.out, "w") as handle:
        json.dump(payload, handle)
    print(f"[save] {args.out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
