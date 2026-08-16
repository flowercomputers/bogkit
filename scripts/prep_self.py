#!/usr/bin/env python3
"""The "yours" shelf: your own past writing -> data/self.jsonl.

Reads a plain-text file of your writing (paragraphs separated by blank
lines), sentence-splits with the app's splitter, and writes the personal
reference library. The source file and the output stay on your machine —
data/ is gitignored, and this library is embedded by ese inside the binary's
own math. Nothing leaves the laptop.

Usage: python3 scripts/prep_self.py [src=~/.loupe-seed.txt] [outdir=data]
"""
import json, pathlib, re, sys

SPLIT = re.compile(r'([^.!?]+[.!?”"]+)|([^.!?]+$)')
HONORIFIC = re.compile(r'\b(Mr|Mrs|Ms|Dr|St)\.$')


def sentences(text: str):
    out = []
    for line in text.split("\n\n"):
        line = " ".join(line.split())
        if not line:
            continue
        parts = [m.group(0).strip() for m in SPLIT.finditer(line) if m.group(0).strip()]
        merged = []
        for p in parts:
            if merged and HONORIFIC.search(merged[-1]):
                merged[-1] += " " + p
            else:
                merged.append(p)
        out.extend(s for s in merged if len(s.split()) >= 4)
    return out


def main():
    src = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "~/.loupe-seed.txt").expanduser()
    outdir = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else "data")
    outdir.mkdir(exist_ok=True)
    sents = sentences(src.read_text(errors="replace"))
    with open(outdir / "self.jsonl", "w") as f:
        for s in sents:
            f.write(json.dumps({"book": 0, "title": "you, earlier", "text": s}) + "\n")
    print(f"DONE: {len(sents)} of your sentences -> {outdir/'self.jsonl'} (local only — data/ is gitignored)")


if __name__ == "__main__":
    main()
