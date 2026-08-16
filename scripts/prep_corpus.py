#!/usr/bin/env python3
"""G2 corpus prep: Project Gutenberg -> canon.jsonl (one sentence per line).

Splitter matches the app's semantics: regex sentence split + honorific rejoin,
drop fragments under 4 words. Output: {"book": id, "title": t, "text": sentence}.
Usage: python3 scripts/prep_corpus.py [outdir=data]
"""
import json, pathlib, re, sys, urllib.request

BOOKS = {
    2701: "Moby-Dick", 205: "Walden", 1342: "Pride and Prejudice",
    98: "A Tale of Two Cities", 219: "Heart of Darkness", 84: "Frankenstein",
    1952: "The Yellow Wallpaper", 408: "The Souls of Black Folk",
    1661: "The Adventures of Sherlock Holmes", 174: "The Picture of Dorian Gray",
    46: "A Christmas Carol", 1260: "Jane Eyre", 345: "Dracula",
    1400: "Great Expectations", 36: "The War of the Worlds", 158: "Emma",
    145: "Middlemarch", 215: "The Call of the Wild", 5200: "Metamorphosis",
    2600: "War and Peace",
}
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

def strip_gutenberg(text: str) -> str:
    s = re.search(r"\*\*\* ?START OF (?:THE|THIS) PROJECT GUTENBERG.*?\*\*\*", text, re.S)
    e = re.search(r"\*\*\* ?END OF (?:THE|THIS) PROJECT GUTENBERG", text)
    return text[s.end() if s else 0 : e.start() if e else len(text)]

def main():
    outdir = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "data")
    outdir.mkdir(exist_ok=True)
    total = 0
    with open(outdir / "canon.jsonl", "w") as f:
        for bid, title in BOOKS.items():
            url = f"https://www.gutenberg.org/cache/epub/{bid}/pg{bid}.txt"
            try:
                raw = urllib.request.urlopen(url, timeout=60).read().decode("utf-8", "replace")
            except Exception as ex:
                print(f"SKIP {title}: {ex}", file=sys.stderr)
                continue
            sents = sentences(strip_gutenberg(raw))
            for s in sents:
                f.write(json.dumps({"book": bid, "title": title, "text": s}) + "\n")
            total += len(sents)
            print(f"{title}: {len(sents)} sentences (total {total})")
    print(f"DONE: {total} sentences -> {outdir/'canon.jsonl'}")

if __name__ == "__main__":
    main()
