#!/usr/bin/env python3
"""Screenplay library prep: HF IsmaelMousa/movies -> data/screen.jsonl.

1,172 full film scripts (columns Name, Script; Apache-2.0). Downloads the
parquet shards via the HF API, strips screenplay furniture (scene headings,
character cues, transitions), sentence-splits with the app's splitter, and
samples evenly across films to a target sentence count.

Usage: python3 scripts/prep_movies.py [target=70000] [outdir=data]
"""
import json, pathlib, re, sys, urllib.request

API = "https://huggingface.co/api/datasets/IsmaelMousa/movies/parquet/default/train"
SPLIT = re.compile(r'([^.!?]+[.!?”"]+)|([^.!?]+$)')
HONORIFIC = re.compile(r'\b(Mr|Mrs|Ms|Dr|St)\.$')
HEADING = re.compile(r'^\s*(INT|EXT|INT/EXT|I/E)[\s./]', re.I)
TRANSITION = re.compile(r'(TO:|FADE IN|FADE OUT|DISSOLVE|SMASH CUT|THE END)\s*$')
CUEISH = re.compile(r"^[A-Z0-9 .'()\-]+$")  # all-caps line: cue or heading


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


def prose_of(script: str) -> str:
    # keep action + dialogue lines; drop headings, transitions, cues, numbers
    keep = []
    for raw in script.split("\n"):
        line = raw.strip()
        if not line:
            keep.append("")  # paragraph boundary
            continue
        if HEADING.match(line) or TRANSITION.search(line):
            continue
        if len(line.split()) <= 6 and CUEISH.match(line):
            continue  # character cue / ALL-CAPS furniture
        if line.isdigit():
            continue
        keep.append(line)
    # collapse into paragraphs on blank lines
    return re.sub(r"\n{2,}", "\n\n", "\n".join(keep)).replace("\n", " ").replace("  ", " ") if False else "\n\n".join(
        p.strip() for p in re.split(r"\n\s*\n", "\n".join(keep)) if p.strip()
    ).replace("\n", " ")


def main():
    target = int(sys.argv[1]) if len(sys.argv) > 1 else 70000
    outdir = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else "data")
    outdir.mkdir(exist_ok=True)
    cache = outdir / "hf-movies"
    cache.mkdir(exist_ok=True)

    urls = json.loads(urllib.request.urlopen(API, timeout=60).read())
    files = []
    for i, url in enumerate(urls):
        dst = cache / f"movies-{i}.parquet"
        if not dst.exists() or dst.stat().st_size == 0:
            print(f"downloading shard {i + 1}/{len(urls)}…", flush=True)
            tmp = dst.with_suffix(".tmp")
            with urllib.request.urlopen(url, timeout=300) as r, open(tmp, "wb") as f:
                while chunk := r.read(1 << 20):
                    f.write(chunk)
            tmp.rename(dst)
        files.append(dst)

    import pyarrow.parquet as pq

    rows = []
    for f in files:
        t = pq.read_table(f, columns=["Name", "Script"])
        rows.extend(zip(t.column("Name").to_pylist(), t.column("Script").to_pylist()))
    print(f"{len(rows)} scripts", flush=True)

    per_film = max(1, target // max(1, len(rows)))
    total = 0
    with open(outdir / "screen.jsonl", "w") as out:
        for i, (name, script) in enumerate(rows):
            if not name or not script:
                continue
            sents = sentences(prose_of(script))
            if not sents:
                continue
            step = max(1, len(sents) // per_film)
            picked = sents[::step][:per_film]
            for s in picked:
                out.write(json.dumps({"book": i, "title": str(name).strip(), "text": s}) + "\n")
            total += len(picked)
            if (i + 1) % 200 == 0:
                print(f"{i + 1} films → {total} sentences", flush=True)
    print(f"DONE: {total} sentences -> {outdir/'screen.jsonl'}", flush=True)


if __name__ == "__main__":
    main()
