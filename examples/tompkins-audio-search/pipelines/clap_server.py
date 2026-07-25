"""A tiny sidecar holding the CLAP model, for query-time embedding.

Text-to-audio search needs the *text* tower of the same CLAP checkpoint that
produced the indexed audio embeddings — they only share a space because they
share a model. That model is a Python artifact, and loading it takes several
seconds, so shelling out per query is not viable and reimplementing the tower
in Rust would be a second thing to keep in sync.

So: one long-lived process, one HTTP endpoint, localhost only.

    POST /embed_text  {"texts": ["heavy rain"]}      -> {"embeddings": [[...512]]}
    POST /embed_audio {"path": "/abs/file.wav"}      -> {"embeddings": [[...512]]}
    GET  /health                                      -> {"ok": true, ...}

Embeddings are L2-normalised, matching `clap_windows.py`, so cosine distance in
the index means what the ranker thinks it means.

    python clap_server.py --port 8181
"""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import torch

CHECKPOINT = "laion/clap-htsat-unfused"
SAMPLE_RATE = 48_000

_model = None
_processor = None
_device = "cpu"


def l2_normalize(x: np.ndarray) -> np.ndarray:
    return x / np.maximum(np.linalg.norm(x, axis=-1, keepdims=True), 1e-12)


def joint_embedding(out):
    if isinstance(out, torch.Tensor):
        return out
    pooled = getattr(out, "pooler_output", None)
    if pooled is not None:
        return pooled
    for name in ("text_embeds", "audio_embeds"):
        v = getattr(out, name, None)
        if v is not None:
            return v
    raise TypeError(f"no joint embedding in {type(out).__name__}")


def load(device: str) -> None:
    global _model, _processor, _device
    from transformers import AutoProcessor, ClapModel

    _device = device
    _processor = AutoProcessor.from_pretrained(CHECKPOINT)
    _model = ClapModel.from_pretrained(CHECKPOINT).to(device).eval()


@torch.no_grad()
def embed_text(texts: list[str]) -> list[list[float]]:
    inputs = _processor(text=texts, return_tensors="pt", padding=True)
    inputs = {k: v.to(_device) for k, v in inputs.items()}
    feats = joint_embedding(_model.get_text_features(**inputs))
    return l2_normalize(feats.float().cpu().numpy()).tolist()


@torch.no_grad()
def embed_audio_file(path: str) -> list[list[float]]:
    """Embed an uploaded clip, for audio-example search.

    Decoded through ffmpeg to 48 kHz mono, the same path the indexed windows
    took, so an example clip and the archive are treated identically.
    """
    import subprocess

    proc = subprocess.run(
        [
            "ffmpeg", "-hide_banner", "-loglevel", "error",
            "-i", path,
            "-f", "f32le", "-acodec", "pcm_f32le", "-ac", "1", "-ar", str(SAMPLE_RATE),
            "-",
        ],
        capture_output=True,
        check=True,
    )
    audio = np.frombuffer(proc.stdout, dtype=np.float32)
    if audio.size == 0:
        raise ValueError("decoded to zero samples")
    # cap at 10 s to match the indexed window length
    audio = audio[: SAMPLE_RATE * 10]
    inputs = _processor(audio=[audio], sampling_rate=SAMPLE_RATE, return_tensors="pt", padding=True)
    inputs = {k: v.to(_device) for k, v in inputs.items()}
    feats = joint_embedding(_model.get_audio_features(**inputs))
    return l2_normalize(feats.float().cpu().numpy()).tolist()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):  # quiet; the Rust side logs queries
        pass

    def _send(self, code: int, payload: dict) -> None:
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._send(200, {"ok": True, "checkpoint": CHECKPOINT, "device": _device})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        try:
            req = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError as e:
            self._send(400, {"error": f"bad json: {e}"})
            return
        try:
            if self.path == "/embed_text":
                texts = req.get("texts") or []
                if not texts:
                    self._send(400, {"error": "no texts"})
                    return
                self._send(200, {"embeddings": embed_text(texts)})
            elif self.path == "/embed_audio":
                path = req.get("path")
                if not path:
                    self._send(400, {"error": "no path"})
                    return
                self._send(200, {"embeddings": embed_audio_file(path)})
            else:
                self._send(404, {"error": "not found"})
        except Exception as e:  # a bad query must not take the sidecar down
            self._send(500, {"error": f"{type(e).__name__}: {e}"})


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8181)
    ap.add_argument("--device", default=None)
    args = ap.parse_args()

    device = args.device or ("mps" if torch.backends.mps.is_available() else "cpu")
    print(f"clap-server: loading {CHECKPOINT} on {device}", flush=True)
    load(device)
    print(f"clap-server: listening on 127.0.0.1:{args.port}", flush=True)
    # localhost only: this endpoint has no auth and needs none
    ThreadingHTTPServer(("127.0.0.1", args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
