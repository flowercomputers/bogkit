#!/usr/bin/env python3
"""Contracts library prep: Stanford Materials Contracts Corpus (10-K exhibits,
2026) -> data/contracts.jsonl.

Source layout (downloaded from mcc.law.stanford.edu):
  <src>/metadata.parquet                 5,984 rows; company_name, label,
                                         ocr_labeled, archive_name (join key)
  <src>/documents/{cik}/{accession}/*.htm   EDGAR SGML preamble + HTML

Skips OCR'd (image-heavy) filings, strips the SGML wrapper and HTML,
unescapes entities, drops ALL-CAPS heading lines, sentence-splits with the
app's splitter (legal-abbreviation guard added), and samples evenly across
contracts. Card title = "Company Name · label".

Usage: python3 scripts/prep_contracts.py [src=~/orca/bogkit/dataset/archive] [target=70000] [outdir=data]
"""
import html, json, pathlib, re, sys

SPLIT = re.compile(r'([^.!?]+[.!?”"]+)|([^.!?]+$)')
ABBREV = re.compile(r'\b(Mr|Mrs|Ms|Dr|St|No|Sec|Art|Inc|Corp|Ltd|Co|U\.S|Exh)\.$')
TAG = re.compile(r"<[^>]+>")
BLOCK = re.compile(r"</?(p|div|br|tr|li|h[1-6]|table)[^>]*>", re.I)
DROP = re.compile(r"<(style|script|head)[^>]*>.*?</\1>", re.I | re.S)


def sentences(text: str):
    out = []
    for line in text.split("\n"):
        line = " ".join(line.split())
        if not line or len(line.split()) < 4:
            continue
        letters = [c for c in line if c.isalpha()]
        if letters and sum(c.isupper() for c in letters) / len(letters) > 0.8:
            continue  # ALL-CAPS heading / party block
        parts = [m.group(0).strip() for m in SPLIT.finditer(line) if m.group(0).strip()]
        merged = []
        for p in parts:
            if merged and ABBREV.search(merged[-1]):
                merged[-1] += " " + p
            else:
                merged.append(p)
        out.extend(s for s in merged if len(s.split()) >= 4)
    return out


def text_of(path: pathlib.Path) -> str:
    raw = path.read_text(errors="replace")
    i = raw.find("<TEXT>")
    if i >= 0:
        raw = raw[i + 6 :]
    raw = raw.replace("</TEXT>", "").replace("</DOCUMENT>", "")
    raw = DROP.sub(" ", raw)
    raw = BLOCK.sub("\n", raw)
    raw = TAG.sub(" ", raw)
    raw = html.unescape(raw)
    raw = raw.replace("\xa0", " ")
    return "\n".join(" ".join(l.split()) for l in raw.split("\n"))


def main():
    src = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "~/orca/bogkit/dataset/archive").expanduser()
    target = int(sys.argv[2]) if len(sys.argv) > 2 else 70000
    outdir = pathlib.Path(sys.argv[3] if len(sys.argv) > 3 else "data")
    outdir.mkdir(exist_ok=True)

    import pyarrow.parquet as pq

    meta = pq.read_table(src / "metadata.parquet")
    cols = {c: meta.column(c).to_pylist() for c in ["company_name", "label", "ocr_labeled", "archive_name", "content_type"]}
    rows = [
        (n, lb, an)
        for n, lb, ocr, an, ct in zip(cols["company_name"], cols["label"], cols["ocr_labeled"], cols["archive_name"], cols["content_type"])
        if not ocr and ct == "text/html"
    ]
    print(f"{len(rows)} usable contracts (of {meta.num_rows})", flush=True)

    per_doc = max(1, round(target / max(1, len(rows))))
    total = 0
    with open(outdir / "contracts.jsonl", "w") as out:
        for i, (name, label, an) in enumerate(rows):
            p = src / "documents" / an
            if not p.exists():
                continue
            try:
                sents = sentences(text_of(p))
            except Exception:
                continue
            if not sents:
                continue
            step = max(1, len(sents) // per_doc)
            picked = sents[::step][:per_doc]
            title = f"{str(name).title()} · {label}" if label and label != "na" else str(name).title()
            for s in picked:
                out.write(json.dumps({"book": i, "title": title, "text": s}) + "\n")
            total += len(picked)
            if (i + 1) % 500 == 0:
                print(f"{i + 1} contracts → {total} sentences", flush=True)
    print(f"DONE: {total} sentences -> {outdir/'contracts.jsonl'}", flush=True)


if __name__ == "__main__":
    main()
