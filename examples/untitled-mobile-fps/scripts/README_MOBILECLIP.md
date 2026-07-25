# MobileCLIP2-S0 → Core ML

Converts the MobileCLIP2-S0 **image encoder** into a Core ML `.mlpackage` the iOS
app loads at runtime to embed appearance crops. You normally don't need to run
this yourself — the produced `MobileCLIPImageEncoder.mlpackage` is committed/handed
over. These notes exist so the conversion is reproducible.

> MobileCLIP-**S0** is not distributed through `open_clip`; **MobileCLIP2-S0**
> (`dfndr2b`) is the same tiny S0-class encoder from the improved MobileCLIP2
> release (still 512-d), so that's what the default command uses.

## What it produces

`MobileCLIPImageEncoder.mlpackage` — takes an `NxN` RGB image, returns a
512-dimension, L2-normalized (unit-length) float embedding. Preprocessing
(pixel scaling + CLIP mean/std normalization) is **baked into the model**, so the
Swift side just hands it a resized image and reads the vector back.

## Requirements & gotcha

Use a **clean** virtualenv from a Python that has the `_lzma` module (torchvision
imports it). A pyenv Python built without xz will fail with
`ModuleNotFoundError: No module named '_lzma'`; Homebrew's `python3.11` works.
Do *not* use `--system-site-packages`: a global `scipy`/`sklearn` built against
NumPy 1.x collides with NumPy 2.x and makes `import coremltools` fail with
`_ARRAY_API not found`. Pin torch to a coremltools-tested version.

```bash
/opt/homebrew/bin/python3.11 -m venv mlenv    # clean, isolated, has _lzma
mlenv/bin/pip install --upgrade pip
mlenv/bin/pip install "numpy<2" "torch==2.7.0" "torchvision==0.22.0" \
    coremltools open_clip_torch
```

## Run

The default path uses `open_clip`, which auto-downloads the MobileCLIP-S0 weights
(no manual checkpoint):

```bash
mlenv/bin/python scripts/convert_mobileclip.py \
    --out MobileCLIPImageEncoder.mlpackage
```

Optional: to use Apple's raw checkpoint instead (supports reparameterization for a
smaller/faster on-device model), install the `apple/ml-mobileclip` package, grab
`mobileclip_s0.pt`, and pass it:

```bash
mlenv/bin/pip install git+https://github.com/apple/ml-mobileclip.git
curl -L -o mobileclip_s0.pt \
  https://docs-assets.developer.apple.com/ml-research/datasets/mobileclip/mobileclip_s0.pt
mlenv/bin/python scripts/convert_mobileclip.py \
    --checkpoint mobileclip_s0.pt --out MobileCLIPImageEncoder.mlpackage
```

The script auto-detects the input resolution and CLIP mean/std from the model's
own preprocessing transforms (nothing hardcoded), then prints them and asserts the
reloaded model returns a `[1, 512]` unit-length vector.

## Wiring into the app

1. Drag `MobileCLIPImageEncoder.mlpackage` into the Xcode **app** target
   (`UntitledMobileFPS`), "Copy items if needed", membership = app target only.
   (Not the SwiftPM core library — it must stay Core ML-free.)
2. `UntitledMobileFPS/MobileCLIPEmbedder.swift` loads it lazily and feeds every
   appearance crop through it; `AppearanceFeatureExtractor.embedding` calls it and
   falls back to the deterministic color-grid hash if the model is unavailable.

## License

The MobileCLIP-S0 weights are under Apple's ML Research Model Terms of Use. Fine
for a hackathon demo; review the terms before shipping / TestFlight distribution.
