#!/usr/bin/env python3
"""Convert the MobileCLIP2-S0 *image* encoder to a Core ML .mlpackage.

(MobileCLIP-S0 itself is not distributed through open_clip; MobileCLIP2-S0 is the
same tiny S0-class encoder from the improved MobileCLIP2 release, also 512-d.)

The iOS app only needs the image tower (every appearance crop is an image;
text/descriptions go through ESE server-side). This script:

  1. Loads MobileCLIP-S0 and its preprocessing transforms.
  2. Auto-detects the native input resolution and the CLIP normalize mean/std
     from those transforms (nothing is hardcoded, so a wrong guess can't break it).
  3. Wraps the encoder so it takes a [0,1] image, applies (x-mean)/std internally,
     and L2-normalizes the output vector (the app then receives a unit vector).
  4. Traces the wrapper with a DUMMY input tensor -- torch.rand(1,3,H,W). This
     "image tensor" is just random pixels of the right shape; tracing records the
     op graph, so the values are irrelevant. You never supply a real image here.
  5. Converts to a Core ML ImageType-input model (Core ML feeds pixels in [0,255];
     scale=1/255 turns them into [0,1] for the wrapper).
  6. Saves the .mlpackage, reloads it, and asserts output shape [1,512], unit norm.

Primary load path is open_clip (auto-downloads weights, lean deps). If a raw
Apple checkpoint is passed via --checkpoint, the apple `mobileclip` package path
is used instead (supports reparameterization for a smaller/faster device model).

Usage:
    python convert_mobileclip.py --out MobileCLIPImageEncoder.mlpackage
    python convert_mobileclip.py --checkpoint checkpoints/mobileclip_s0.pt --out ...
"""
from __future__ import annotations

import argparse
import sys

import numpy as np
import torch
import torch.nn as nn


EXPECTED_DIM = 512  # matches appearanceEmbeddingDimensions in the Swift app


def detect_input_size(preprocess) -> int:
    """Pull the square input side length out of a torchvision Compose."""
    size = None
    for t in getattr(preprocess, "transforms", []):
        s = getattr(t, "size", None)
        if s is None:
            continue
        # CenterCrop wins over Resize; both may set .size (int or (h, w)).
        val = s[0] if isinstance(s, (list, tuple)) else s
        name = type(t).__name__
        if name == "CenterCrop":
            return int(val)
        size = int(val)
    if size is None:
        raise RuntimeError("Could not detect input resolution from preprocess transforms")
    return size


def detect_mean_std(preprocess):
    for t in getattr(preprocess, "transforms", []):
        mean = getattr(t, "mean", None)
        std = getattr(t, "std", None)
        if mean is not None and std is not None:
            return list(map(float, mean)), list(map(float, std))
    raise RuntimeError("Could not detect Normalize mean/std from preprocess transforms")


class ImageEncoderWrapper(nn.Module):
    """[0,1] image in -> L2-normalized embedding out, normalization baked in."""

    def __init__(self, model: nn.Module, mean, std):
        super().__init__()
        self.model = model
        self.register_buffer("mean", torch.tensor(mean).view(1, 3, 1, 1))
        self.register_buffer("std", torch.tensor(std).view(1, 3, 1, 1))

    def forward(self, pixel_values: torch.Tensor) -> torch.Tensor:
        x = (pixel_values - self.mean) / self.std
        feats = self.model.encode_image(x)
        return feats / feats.norm(dim=-1, keepdim=True).clamp_min(1e-12)


def load_open_clip(model_name: str, pretrained: str):
    import open_clip

    print(f"[load] open_clip {model_name} (pretrained={pretrained})", flush=True)
    model, _, preprocess = open_clip.create_model_and_transforms(
        model_name, pretrained=pretrained
    )
    return model.eval(), preprocess


def load_apple(checkpoint: str):
    import mobileclip  # apple/ml-mobileclip

    print(f"[load] apple mobileclip_s0 from {checkpoint}", flush=True)
    model, _, preprocess = mobileclip.create_model_and_transforms(
        "mobileclip_s0", pretrained=checkpoint
    )
    model.eval()
    try:
        from mobileclip.modules.common.mobileone import reparameterize_model

        model = reparameterize_model(model)
        print("[load] reparameterized for inference", flush=True)
    except Exception as exc:  # noqa: BLE001 - reparameterization is best-effort
        print(f"[load] reparameterization skipped ({exc})", flush=True)
    return model, preprocess


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default="MobileCLIPImageEncoder.mlpackage")
    ap.add_argument("--checkpoint", default=None,
                    help="Path to a raw Apple mobileclip_s0.pt (uses the apple package path)")
    # MobileCLIP-S0 is not distributed through open_clip; MobileCLIP2-S0 is the
    # same tiny S0-class encoder from the improved MobileCLIP2 release (512-d).
    ap.add_argument("--model", default="MobileCLIP2-S0", help="open_clip model name")
    ap.add_argument("--pretrained", default="dfndr2b", help="open_clip pretrained tag")
    args = ap.parse_args()

    import coremltools as ct

    model, preprocess = (
        load_apple(args.checkpoint) if args.checkpoint else load_open_clip(args.model, args.pretrained)
    )

    side = detect_input_size(preprocess)
    mean, std = detect_mean_std(preprocess)
    print(f"[detect] input={side}x{side}  mean={mean}  std={std}", flush=True)

    wrapper = ImageEncoderWrapper(model, mean, std).eval()

    # The dummy trace tensor. Random pixels of shape (batch, channels, H, W).
    example = torch.rand(1, 3, side, side)
    with torch.no_grad():
        probe = wrapper(example)
    out_dim = int(probe.shape[-1])
    print(f"[detect] output embedding dim = {out_dim}", flush=True)
    if out_dim != EXPECTED_DIM:
        print(f"[warn] embedding dim {out_dim} != expected {EXPECTED_DIM}; "
              "reconcile appearanceEmbeddingDimensions before wiring.", flush=True)

    with torch.no_grad():
        traced = torch.jit.trace(wrapper, example)

    print("[convert] running coremltools...", flush=True)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.ImageType(
            name="image",
            shape=(1, 3, side, side),
            scale=1.0 / 255.0,      # Core ML feeds [0,255]; wrapper wants [0,1]
            bias=[0.0, 0.0, 0.0],
            color_layout=ct.colorlayout.RGB,
        )],
        outputs=[ct.TensorType(name="embedding")],
        minimum_deployment_target=ct.target.iOS16,
        compute_precision=ct.precision.FLOAT16,
        convert_to="mlprogram",
    )
    mlmodel.short_description = (
        f"MobileCLIP-S0 image encoder, {side}x{side} RGB -> {out_dim}-d L2-normalized embedding"
    )
    mlmodel.input_description["image"] = f"{side}x{side} RGB image"
    mlmodel.output_description["embedding"] = f"{out_dim}-d unit-length appearance embedding"
    mlmodel.save(args.out)
    print(f"[save] {args.out}", flush=True)

    # --- Reload + sanity check -------------------------------------------------
    from PIL import Image

    reloaded = ct.models.MLModel(args.out)
    rng = np.random.default_rng(0)
    img = Image.fromarray(rng.integers(0, 256, (side, side, 3), dtype=np.uint8))
    pred = reloaded.predict({"image": img})
    vec = np.array(next(iter(pred.values()))).reshape(-1)
    norm = float(np.linalg.norm(vec))
    print(f"[verify] output len={vec.shape[0]}  L2norm={norm:.4f}", flush=True)
    assert vec.shape[0] == out_dim, "reloaded output dim mismatch"
    assert abs(norm - 1.0) < 1e-2, f"embedding not unit-length (norm={norm})"
    print("[verify] OK -- model produces a unit-length embedding.", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
