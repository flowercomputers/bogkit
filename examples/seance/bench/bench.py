#!/usr/bin/env python3
"""Baseline drivers for seance's --bench-walk workload, plus a summarizer.

The workload (identical across contenders): materialize a repo's tree at
HEAD-n on the first-parent line, apply commits forward one at a time to
HEAD, and after each step run the same queries (top-10). Same content
rules as seance: skip blobs > 1MB, skip binaries (NUL sniff), index at
most the first 64KB of each file.

  bench.py sqlite <repo> <n> [--queries "a,b,c"]     FTS5 baseline (JSONL on stdout)
  bench.py grep   <repo> <n> [--queries ...] [--sample 50]
  bench.py summary label=path.jsonl [label=path.jsonl ...]

Fairness notes, disclosed rather than hidden:
  - seance's apply_ms includes ese embedding + HNSW maintenance per file,
    which no baseline performs at all.
  - FTS5 tokenizes with unicode61; seance's Bm25 uses ASCII-alnum. Queries
    are OR-of-terms in both.
  - git grep has zero apply cost by construction; its query scans the
    whole tree at that commit (sampled every --sample commits to keep
    runtime sane; apply_ms is reported as 0).

Human narration goes to stderr; stdout is machine-readable JSONL only.
"""

import json
import sqlite3
import subprocess
import sys
import time

MAX_BLOB = 1024 * 1024
INDEX_CAP = 64 * 1024
DEFAULT_QUERIES = ["btree balance", "wal checkpoint", "vdbe cursor"]


def log(msg):
    print(msg, file=sys.stderr)


def git(repo, *args, binary=False):
    out = subprocess.run(["git", "-C", repo, *args], capture_output=True, check=True)
    return out.stdout if binary else out.stdout.decode("utf-8", "replace")


class Blobs:
    """Persistent `git cat-file --batch` child, like seance's."""

    def __init__(self, repo):
        self.p = subprocess.Popen(
            ["git", "-C", repo, "cat-file", "--batch"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
        )

    def read(self, spec):
        self.p.stdin.write(spec.encode() + b"\n")
        self.p.stdin.flush()
        header = self.p.stdout.readline().decode("utf-8", "replace").rstrip().rsplit(" ", 2)
        if len(header) < 3 or not header[2].isdigit():
            return None  # missing
        typ, size = header[1], int(header[2])
        body = self.p.stdout.read(size + 1)[:-1]
        return body if typ == "blob" else None


def commit_list(repo):
    return git(repo, "log", "--first-parent", "--reverse", "--format=%H").split()


def changes_by_commit(repo, old, young):
    """oid -> changed paths, one git spawn for the whole range."""
    out = git(
        repo, "-c", "core.quotepath=false", "log", "--first-parent",
        "--format=%x01%H", "--name-only", f"{old}..{young}",
    )
    result = {}
    for record in out.split("\x01")[1:]:
        lines = record.splitlines()
        result[lines[0].strip()] = [l for l in lines[1:] if l and not l.startswith('"')]
    return result


def indexable(blobs, oid, path):
    """(text, line_count) under seance's rules, or None."""
    body = blobs.read(f"{oid}:{path}")
    if body is None or len(body) > MAX_BLOB or b"\x00" in body:
        return None
    text = body.decode("utf-8", "replace")[:INDEX_CAP]
    return text, text.count("\n")


def parse(argv):
    repo, n = argv[0], int(argv[1])
    queries, sample, vecs = DEFAULT_QUERIES, 50, None
    rest = argv[2:]
    while rest:
        flag = rest.pop(0)
        if flag == "--queries":
            queries = [q.strip() for q in rest.pop(0).split(",")]
        elif flag == "--sample":
            sample = int(rest.pop(0))
        elif flag == "--vecs":
            vecs = rest.pop(0)
    return repo, n, queries, sample, vecs


DB_PATH = "/tmp/seance-bench-fts.db"


def run_sqlite(argv):
    import os

    repo, n, queries, _, vecs_path = parse(argv)
    commits = commit_list(repo)
    head, start = len(commits) - 1, max(0, len(commits) - 1 - n)
    blobs = Blobs(repo)

    for suffix in ("", "-wal", "-shm", ".ckpt"):
        try:
            os.remove(DB_PATH + suffix)
        except FileNotFoundError:
            pass
    db = sqlite3.connect(DB_PATH)
    db.executescript(
        """
        PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
        CREATE TABLE docs (id INTEGER PRIMARY KEY AUTOINCREMENT,
                           path TEXT UNIQUE, lines INT);
        CREATE VIRTUAL TABLE fts USING fts5(content);
        """
    )

    def upsert(path, text, lines):
        row = db.execute("SELECT id FROM docs WHERE path=?", (path,)).fetchone()
        if row:
            db.execute("DELETE FROM fts WHERE rowid=?", (row[0],))
            db.execute("UPDATE docs SET lines=? WHERE id=?", (lines, row[0]))
            doc_id = row[0]
        else:
            doc_id = db.execute(
                "INSERT INTO docs(path, lines) VALUES(?,?)", (path, lines)
            ).lastrowid
        db.execute("INSERT INTO fts(rowid, content) VALUES(?,?)", (doc_id, text))

    def delete(path):
        row = db.execute("SELECT id FROM docs WHERE path=?", (path,)).fetchone()
        if row:
            db.execute("DELETE FROM fts WHERE rowid=?", (row[0],))
            db.execute("DELETE FROM docs WHERE id=?", (row[0],))

    def apply(oid, paths):
        with db:  # one transaction per commit, matching seance
            for path in paths:
                doc = indexable(blobs, oid, path)
                if doc is None:
                    delete(path)
                else:
                    upsert(path, doc[0], doc[1])

    t = time.perf_counter()
    apply(commits[start], git(repo, "ls-tree", "-r", "--name-only", commits[start]).split("\n"))
    log(f"sqlite: baseline materialized at idx {start} in {(time.perf_counter()-t)*1000:.0f} ms")

    changed = changes_by_commit(repo, commits[start], commits[head])
    match = {q: " OR ".join(f'"{t}"' for t in q.split()) for q in queries}

    for i in range(start + 1, head + 1):
        paths = changed.get(commits[i], [])
        t = time.perf_counter()
        apply(commits[i], paths)
        apply_ms = (time.perf_counter() - t) * 1000

        query_us, hits = [], []
        for q in queries:
            t = time.perf_counter()
            rows = db.execute(
                "SELECT d.path, bm25(fts) FROM fts JOIN docs d ON d.id = fts.rowid "
                "WHERE fts MATCH ? ORDER BY bm25(fts) LIMIT 10",
                (match[q],),
            ).fetchall()
            query_us.append(int((time.perf_counter() - t) * 1e6))
            hits.append(len(rows))
        t = time.perf_counter()
        db.execute("SELECT count(*), sum(lines) FROM docs").fetchone()
        stats_us = int((time.perf_counter() - t) * 1e6)

        print(json.dumps({"idx": i, "apply_ms": round(apply_ms, 1), "changed": len(paths),
                          "query_us": query_us, "hits": hits, "stats_us": stats_us}))

    # ---- capability benches; state sits at HEAD ----

    # semantic + hybrid over bog's exact ese vectors, brute-force cosine —
    # the honest "no vector index" shape. Doc norms precompute untimed
    # (that's index-build work). DISCLOSED: this is interpreter-bound
    # CPython; a native flat scan would be faster, an index sub-linear.
    if vecs_path:
        import array
        import math

        dump = json.load(open(vecs_path))
        qvecs = {name: array.array("f", v) for name, v in dump["queries"]}
        docs = [(p, array.array("f", v)) for p, v in dump["docs"]]
        norms = [math.sqrt(sum(x * x for x in v)) or 1.0 for _, v in docs]
        log(f"sqlite: flat-scanning {len(docs)} vectors (pure python, no numpy)")

        def cosine_top10(qv):
            qn = math.sqrt(sum(x * x for x in qv)) or 1.0
            scored = []
            for k, (path, dv) in enumerate(docs):
                dot = 0.0
                for a, b in zip(qv, dv):
                    dot += a * b
                scored.append((dot / (qn * norms[k]), path))
            scored.sort(reverse=True)
            return scored[:10]

        sem_us, hyb_us = [], []
        for _ in range(5):
            for q in queries:
                t = time.perf_counter()
                cosine_top10(qvecs[q])
                sem_us.append(int((time.perf_counter() - t) * 1e6))
                t = time.perf_counter()
                kw = db.execute(
                    "SELECT d.path, bm25(fts) FROM fts JOIN docs d ON d.id = fts.rowid "
                    "WHERE fts MATCH ? ORDER BY bm25(fts) LIMIT 10",
                    (match[q],),
                ).fetchall()
                sm = cosine_top10(qvecs[q])
                fused = {}
                for rank, (path, _) in enumerate(kw):
                    fused[path] = fused.get(path, 0) + 1.0 / (60 + rank + 1)
                for rank, (_, path) in enumerate(sm):
                    fused[path] = fused.get(path, 0) + 1.0 / (60 + rank + 1)
                sorted(fused.items(), key=lambda kv: -kv[1])[:10]
                hyb_us.append(int((time.perf_counter() - t) * 1e6))
        print(json.dumps({"cap": "semantic", "query_us": sem_us}))
        print(json.dumps({"cap": "hybrid", "query_us": hyb_us}))

    # era snapshot: WAL-checkpoint then CoW file copy — sqlite's honest
    # equivalent of a checkpoint master (single-file databases warp well!)
    t = time.perf_counter()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.commit()
    subprocess.run(["cp", "-c", DB_PATH, DB_PATH + ".ckpt"], check=True)
    print(json.dumps({"cap": "checkpoint_create",
                      "ms": round((time.perf_counter() - t) * 1000, 1)}))

    # retraction-exact time travel, no snapshot: one direct diff-replay
    far = max(0, head - 10_000)
    paths = [p for p in git(repo, "diff-tree", "-r", "--no-renames", "--name-only",
                            commits[head], commits[far]).split("\n") if p]
    t = time.perf_counter()
    apply(commits[far], paths)
    print(json.dumps({"cap": "cold_jump", "commits": head - far, "changed": len(paths),
                      "ms": round((time.perf_counter() - t) * 1000, 1)}))

    # warp back to HEAD: copy the snapshot back, open, query
    t = time.perf_counter()
    subprocess.run(["cp", "-c", DB_PATH + ".ckpt", DB_PATH + ".warp"], check=True)
    db2 = sqlite3.connect(DB_PATH + ".warp")
    open_ms = (time.perf_counter() - t) * 1000
    rows = db2.execute(
        "SELECT d.path, bm25(fts) FROM fts JOIN docs d ON d.id = fts.rowid "
        "WHERE fts MATCH ? ORDER BY bm25(fts) LIMIT 10",
        (match[queries[0]],),
    ).fetchall()
    print(json.dumps({"cap": "warp", "ms": round((time.perf_counter() - t) * 1000, 1),
                      "open_ms": round(open_ms, 1), "hits": len(rows)}))


def run_grep(argv):
    repo, n, queries, sample, _ = parse(argv)
    commits = commit_list(repo)
    head, start = len(commits) - 1, max(0, len(commits) - 1 - n)
    log(f"grep: sampling every {sample} commits (no index, apply cost is 0 by construction)")
    for i in range(start + 1, head + 1, sample):
        query_us, hits = [], []
        for q in queries:
            args = ["grep", "-i", "-l"]
            for k, term in enumerate(q.split()):
                if k:
                    args.append("--or")
                args += ["-e", term]
            args.append(commits[i])
            t = time.perf_counter()
            out = subprocess.run(["git", "-C", repo, *args], capture_output=True)
            query_us.append(int((time.perf_counter() - t) * 1e6))
            hits.append(min(10, len(out.stdout.splitlines())))
        print(json.dumps({"idx": i, "apply_ms": 0, "changed": 0,
                          "query_us": query_us, "hits": hits, "stats_us": None}))
    # time travel and snapshots are free by construction: git already
    # stores every commit; no state ever moves. Semantic/hybrid: absent.
    print(json.dumps({"cap": "checkpoint_create", "ms": 0}))
    print(json.dumps({"cap": "cold_jump", "commits": 10_000, "changed": 0, "ms": 0}))
    print(json.dumps({"cap": "warp", "ms": 0, "open_ms": 0, "hits": None}))


def pct(sorted_vals, p):
    return sorted_vals[min(len(sorted_vals) - 1, int(len(sorted_vals) * p))]


def run_summary(argv):
    print(f"| system | steps | apply p50 | apply p99 | query p50 | query p99 | stats p50 |")
    print(f"|---|---|---|---|---|---|---|")
    for spec in argv:
        label, path = spec.split("=", 1)
        rows = [r for l in open(path) if l.strip() if "cap" not in (r := json.loads(l))]
        applies = sorted(r["apply_ms"] for r in rows)
        qs = sorted(u for r in rows for u in r["query_us"])
        stats = sorted(r["stats_us"] for r in rows if r.get("stats_us") is not None)
        stats_p50 = f"{pct(stats, 0.5)}µs" if stats else "—"
        print(
            f"| {label} | {len(rows)} | {pct(applies, 0.5)}ms | {pct(applies, 0.99)}ms "
            f"| {pct(qs, 0.5)/1000:.1f}ms | {pct(qs, 0.99)/1000:.1f}ms | {stats_p50} |"
        )


def run_capsummary(argv):
    systems = []
    for spec in argv:
        label, path = spec.split("=", 1)
        caps = {}
        for line in open(path):
            r = json.loads(line)
            if "cap" in r:
                caps[r["cap"]] = r
        systems.append((label, caps))

    def cell(caps, cap, kind):
        r = caps.get(cap)
        if r is None:
            return "—"
        if kind == "q":
            qs = sorted(r["query_us"])
            return f"{pct(qs, 0.5)/1000:.1f}ms (p99 {pct(qs, 0.99)/1000:.1f}ms)"
        return f"{r['ms']}ms"

    rows = [
        ("semantic top-10 (ese cosine)", "semantic", "q"),
        ("hybrid keyword+semantic+rrf", "hybrid", "q"),
        ("era snapshot: create", "checkpoint_create", "ms"),
        ("time travel: 10k-commit jump", "cold_jump", "ms"),
        ("warp to snapshot (+1 query)", "warp", "ms"),
    ]
    print("| capability | " + " | ".join(l for l, _ in systems) + " |")
    print("|---" * (len(systems) + 1) + "|")
    for title, cap, kind in rows:
        cells = " | ".join(cell(c, cap, kind) for _, c in systems)
        print(f"| {title} | {cells} |")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "sqlite":
        run_sqlite(sys.argv[2:])
    elif cmd == "grep":
        run_grep(sys.argv[2:])
    elif cmd == "summary":
        run_summary(sys.argv[2:])
    elif cmd == "capsummary":
        run_capsummary(sys.argv[2:])
    else:
        log(__doc__)
        sys.exit(2)
