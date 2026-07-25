"""Speech: voice activity detection first, transcription only where it fires.

Two reasons the order matters. Transcribing 12 hours of street ambience blind
would waste most of the compute on wind and traffic — but worse, Whisper
hallucinates fluent text on non-speech audio, and a confident false transcript
is more damaging to a search index than a missing one. VAD gates it, and
several hallucination signatures are checked before a span is emitted:

* `no_speech_probability` — Whisper's own estimate, on a region VAD called
  speech. High values mean the two disagree, and Whisper is usually wrong.
* `avg_logprob` — low average token probability means guessing.
* `compression_ratio` — high values mean the text is repetitive, the classic
  "thank you thank you thank you" degenerate loop.

Rejected spans are written to `*.refused.jsonl` with the reason and the scores
that triggered it, so a threshold can be judged from the material it discarded
rather than from a count. The first pilot refused 40 of 60 candidates, 33 for
`avg_logprob` alone, which is what prompted retuning these.

Word timings come from Whisper's cross-attention alignment. The handoff
specifies WhisperX; this uses faster-whisper's `word_timestamps` instead, which
avoids WhisperX's heavier dependency tree while producing the same
`WordTiming` shape. If evaluation shows word-level precision is not good
enough, WhisperX drops in behind this interface — see the ADR.

    python speech_spans.py 9561 --asset-dir ../data/assets/9561 --out ../data/prepared/9561
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

from common import (
    BatchWriter,
    ModelStamp,
    config_hash,
    decode_asset,
    load_assets,
    progress,
    sha256_bytes,
    utc_now,
)

SAMPLE_RATE = 16_000  # both silero and whisper want 16 kHz mono

# VAD region assembly
MERGE_GAP_SECONDS = 0.6      # speech separated by less than this is one region
CONTEXT_PADDING_SECONDS = 0.3  # lead-in/out so a word is not clipped
MIN_REGION_SECONDS = 0.4     # shorter than this is a click, not an utterance
MAX_REGION_SECONDS = 30.0    # cap so one long region cannot stall the run

# Hallucination guards.
#
# Retuned after the first pilot, where they refused 40 of 60 candidates — 33 of
# them for `avg_logprob` alone. Distant speech across a park scores low on token
# probability *because it is distant*, not because it is invented, so a -1.0
# floor was discarding the very material this archive is made of. The
# no-speech and repetition guards target actual hallucination signatures and
# are left where they were.
#
# Everything refused is now written to `*.refused.jsonl` instead of being
# counted and dropped, so the threshold can be judged from evidence rather than
# from a total.
MAX_NO_SPEECH_PROB = 0.6
MIN_AVG_LOGPROB = -1.35
MAX_COMPRESSION_RATIO = 2.4
MIN_TEXT_CHARS = 2


def merge_regions(
    regions: list[tuple[float, float]], total_seconds: float
) -> list[tuple[float, float]]:
    """Merge nearby speech, pad for context, split over-long runs."""
    if not regions:
        return []
    merged: list[list[float]] = [list(regions[0])]
    for start, end in regions[1:]:
        if start - merged[-1][1] <= MERGE_GAP_SECONDS:
            merged[-1][1] = end
        else:
            merged.append([start, end])

    out: list[tuple[float, float]] = []
    for start, end in merged:
        start = max(0.0, start - CONTEXT_PADDING_SECONDS)
        end = min(total_seconds, end + CONTEXT_PADDING_SECONDS)
        if end - start < MIN_REGION_SECONDS:
            continue
        # a very long region is split rather than dropped or sent whole
        while end - start > MAX_REGION_SECONDS:
            out.append((start, start + MAX_REGION_SECONDS))
            start += MAX_REGION_SECONDS
        if end - start >= MIN_REGION_SECONDS:
            out.append((start, end))
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("stream_id", type=int)
    ap.add_argument("--asset-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--model", default="small", help="faster-whisper model size")
    ap.add_argument("--language", default=None, help="None = detect per region")
    # Lowered from silero's 0.5 default: on the first pilot the VAD flagged 76
    # regions where CLAP's speech prompt was the top label in 222 windows, so a
    # quiet, distant source needs a lower bar to be heard at all.
    ap.add_argument("--vad-threshold", type=float, default=0.35)
    ap.add_argument("--limit-assets", type=int, default=0)
    args = ap.parse_args()

    import torch
    from faster_whisper import WhisperModel
    from silero_vad import get_speech_timestamps, load_silero_vad

    cfg = {
        "sample_rate": SAMPLE_RATE,
        "vad_threshold": args.vad_threshold,
        "merge_gap_seconds": MERGE_GAP_SECONDS,
        "context_padding_seconds": CONTEXT_PADDING_SECONDS,
        "min_region_seconds": MIN_REGION_SECONDS,
        "max_region_seconds": MAX_REGION_SECONDS,
        "whisper_model": args.model,
        "language": args.language,
        "max_no_speech_prob": MAX_NO_SPEECH_PROB,
        "min_avg_logprob": MIN_AVG_LOGPROB,
        "max_compression_ratio": MAX_COMPRESSION_RATIO,
    }
    cfg_hash = config_hash(cfg)

    progress("speech: loading silero vad")
    vad = load_silero_vad()
    # CTranslate2 has no Metal backend, so whisper runs on CPU here; int8
    # keeps a 12-hour pass tractable
    progress(f"speech: loading faster-whisper {args.model} on cpu/int8")
    whisper = WhisperModel(args.model, device="cpu", compute_type="int8")

    assets = load_assets(args.stream_id, args.asset_dir)
    if args.limit_assets:
        assets = assets[: args.limit_assets]
    progress(f"speech: {len(assets)} assets to process")

    totals = {
        "regions": 0,
        "vad_seconds": 0.0,
        "audio_seconds": 0.0,
        "emitted": 0,
        "rejected_no_speech": 0,
        "rejected_logprob": 0,
        "rejected_repetitive": 0,
        "rejected_empty": 0,
    }

    for i, asset in enumerate(assets):
        writer = BatchWriter(args.out, "speech", asset.asset_id)
        if writer.is_done():
            progress(f"speech: {asset.asset_id} already done, skipping")
            continue

        audio = decode_asset(asset.file, SAMPLE_RATE, mono=True)
        total_seconds = len(audio) / SAMPLE_RATE
        totals["audio_seconds"] += total_seconds
        stamp = ModelStamp(
            model_name=f"silero-vad+faster-whisper-{args.model}",
            model_version="silero-vad/faster-whisper",
            checkpoint_hash=sha256_bytes(f"silero+{args.model}".encode()),
            config_hash=cfg_hash,
            input_hash=sha256_bytes(audio.tobytes()),
            created_at_utc=utc_now(),
        ).to_dict()

        stamps = get_speech_timestamps(
            torch.from_numpy(audio),
            vad,
            sampling_rate=SAMPLE_RATE,
            threshold=args.vad_threshold,
            return_seconds=True,
        )
        raw = [(float(s["start"]), float(s["end"])) for s in stamps]
        regions = merge_regions(raw, total_seconds)
        vad_seconds = sum(e - s for s, e in regions)
        totals["regions"] += len(regions)
        totals["vad_seconds"] += vad_seconds

        refused_path = args.out / "speech" / f"{asset.asset_id}.refused.jsonl"
        refused_f = refused_path.open("w")

        def refuse(reason, text, start_ms, end_ms, no_speech, logprob, compression):
            """Keep the evidence for a rejection, so the threshold is auditable."""
            refused_f.write(
                json.dumps(
                    {
                        "reason": reason,
                        "text": text,
                        "start_ms": start_ms,
                        "end_ms": end_ms,
                        "no_speech_probability": round(no_speech, 4),
                        "avg_logprob": round(logprob, 4),
                        "compression_ratio": round(compression, 4),
                    },
                    separators=(",", ":"),
                )
                + "\n"
            )

        with writer as w:
            for r_start, r_end in regions:
                clip = audio[int(r_start * SAMPLE_RATE) : int(r_end * SAMPLE_RATE)]
                segments, info = whisper.transcribe(
                    clip,
                    language=args.language,
                    word_timestamps=True,
                    # VAD already selected these regions; letting whisper
                    # re-gate them would double-filter
                    vad_filter=False,
                    condition_on_previous_text=False,
                )
                for seg in segments:
                    text = (seg.text or "").strip()
                    no_speech = float(getattr(seg, "no_speech_prob", 0.0))
                    avg_logprob = float(getattr(seg, "avg_logprob", 0.0))
                    compression = float(getattr(seg, "compression_ratio", 0.0))

                    seg_ms = (
                        asset.stream_ms(r_start + float(seg.start)),
                        asset.stream_ms(r_start + float(seg.end)),
                    )
                    if len(text) < MIN_TEXT_CHARS:
                        totals["rejected_empty"] += 1
                        refuse("empty", text, *seg_ms, no_speech, avg_logprob, compression)
                        continue
                    if no_speech > MAX_NO_SPEECH_PROB:
                        totals["rejected_no_speech"] += 1
                        refuse("no_speech", text, *seg_ms, no_speech, avg_logprob, compression)
                        continue
                    if avg_logprob < MIN_AVG_LOGPROB:
                        totals["rejected_logprob"] += 1
                        refuse("low_logprob", text, *seg_ms, no_speech, avg_logprob, compression)
                        continue
                    if compression > MAX_COMPRESSION_RATIO:
                        totals["rejected_repetitive"] += 1
                        refuse("repetitive", text, *seg_ms, no_speech, avg_logprob, compression)
                        continue

                    # region-relative -> asset-relative -> stream-relative
                    seg_start = r_start + float(seg.start)
                    seg_end = r_start + float(seg.end)
                    utt_start_ms = asset.stream_ms(seg_start)
                    utt_end_ms = asset.stream_ms(seg_end)
                    if utt_end_ms <= utt_start_ms:
                        totals["rejected_empty"] += 1
                        continue

                    words = []
                    for wd in getattr(seg, "words", None) or []:
                        ws = asset.stream_ms(r_start + float(wd.start))
                        we = asset.stream_ms(r_start + float(wd.end))
                        # the Rust store refuses words outside their utterance,
                        # so clamp here where the arithmetic is visible
                        ws = max(utt_start_ms, min(ws, utt_end_ms))
                        we = max(ws, min(we, utt_end_ms))
                        words.append(
                            {
                                "text": (wd.word or "").strip(),
                                "start_ms": ws,
                                "end_ms": we,
                                "confidence": round(float(getattr(wd, "probability", 0.0)), 4),
                            }
                        )

                    w.write(
                        {
                            "kind": "speech",
                            "stream_id": args.stream_id,
                            "utterance_start_ms": utt_start_ms,
                            "utterance_end_ms": utt_end_ms,
                            "text": text,
                            "language": getattr(info, "language", args.language) or "unknown",
                            "words": words,
                            "speaker_label": None,
                            # silero gave the region; treat that as the VAD's
                            # confidence signal for the whole span
                            "vad_confidence": 1.0,
                            "transcript_confidence": round(
                                float(np.exp(avg_logprob)), 4
                            ),
                            "no_speech_probability": round(no_speech, 4),
                            "asset_id": asset.asset_id,
                            "asset_offset_ms": int(round(seg_start * 1000)),
                            "model": stamp,
                        }
                    )
            totals["emitted"] += writer.count
        refused_f.close()

        progress(
            f"speech: [{i + 1}/{len(assets)}] {asset.asset_id} "
            f"{len(regions)} regions / {vad_seconds:.0f}s speech "
            f"({100 * vad_seconds / max(total_seconds, 1):.1f}% of audio) "
            f"-> {writer.count} spans"
        )

    rejected = sum(v for k, v in totals.items() if k.startswith("rejected"))
    progress(
        f"\nspeech: done. {totals['emitted']} spans kept, {rejected} refused "
        f"(no-speech {totals['rejected_no_speech']}, "
        f"low-logprob {totals['rejected_logprob']}, "
        f"repetitive {totals['rejected_repetitive']}, "
        f"empty {totals['rejected_empty']})"
    )
    progress(
        f"speech: VAD found {totals['vad_seconds']:.0f}s of speech in "
        f"{totals['audio_seconds']:.0f}s of audio "
        f"({100 * totals['vad_seconds'] / max(totals['audio_seconds'], 1):.1f}%), "
        f"so transcription ran on that fraction rather than the whole stream"
    )

    summary = args.out / "speech-summary.json"
    summary.write_text(json.dumps({"totals": totals, "config": cfg}, indent=2) + "\n")
    progress(f"speech: wrote {summary}")


if __name__ == "__main__":
    main()
