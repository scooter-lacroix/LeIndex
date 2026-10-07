#!/usr/bin/env python3
"""Head-to-head indexer benchmark: LeIndex vs real external indexers.

Runs the SAME queries against the SAME corpus directories through every
system and computes identical metrics (recall@10 = hit@10, MRR@10) so the
numbers are directly comparable:

  leindex    — the real installed binary (`leindex index` + `leindex.search`
               through `leindex tools run`), full hybrid (TF-IDF + neural +
               structural) exactly as an agent uses it.
  ripgrep    — the search backend the popular agent frontends (aider, cline,
               kilo, roo) shell out to. Emulated agent loop: per-token
               `rg -l` with perfect token bookkeeping, files ranked by number
               of matching query tokens, path-sort tie-break.
  ctags      — Universal Ctags symbol index (the classic indexer). Files
               ranked by symbol-name token matches.
  zoekt      — Sourcegraph's trigram indexer with its own ranking
               (zoekt-indexer + zoekt-query), when installed.

Corpora:
  cosqa  — the vendored CoSQA subset (63 real web queries, human-annotated
           Python code) materialized as one .py file per record; ground truth
           is the paired file. Single-relevant-doc per query.
  repo   — the LeIndex repository itself with curated file-level ground
           truths (real large-repo dimension).

Usage:
  python3 tools/headtohead/headtohead.py \
      --repo-root /path/to/LeIndexer-release-1.8.4 \
      --out docs/baselines/YYYY-MM-DD-headtohead-indexers.md

Results are informational evidence for the parity comparison; they are not
CI-gated (external binaries are not assumed present). Costs follow the W6
model: tokens = chars/4; naive/rg/ctags/zoekt "read" the top-3 files whole,
leindex returns top-10 snippets (~160 chars each).
"""

import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def run(cmd, timeout=120, cwd=None):
    proc = subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout, cwd=cwd
    )
    return proc.returncode, proc.stdout, proc.stderr


# ── metrics (same formulas as src/eval/metrics.rs) ─────────────────────────


def dedup_keep_order(items):
    seen = set()
    return [x for x in items if not (x in seen or seen.add(x))]


def recall_at_10(ranked, relevant):
    if not relevant:
        return 0.0
    ranked = dedup_keep_order(ranked)
    hits = sum(1 for item in ranked[:10] if item in relevant)
    return hits / len(relevant)


def rr_at_10(ranked, relevant):
    relevant = set(relevant)
    for i, item in enumerate(dedup_keep_order(ranked)[:10]):
        if item in relevant:
            return 1.0 / (i + 1)
    return 0.0


def aggregate(rows):
    """rows: list of (recall, rr, tokens)."""
    if not rows:
        return {"recall10": 0.0, "mrr10": 0.0, "tokens": 0.0, "n": 0}
    n = len(rows)
    return {
        "recall10": sum(r[0] for r in rows) / n,
        "mrr10": sum(r[1] for r in rows) / n,
        "tokens": sum(r[2] for r in rows) / n,
        "n": n,
    }


# ── corpora ────────────────────────────────────────────────────────────────


def build_cosqa_corpus(repo_root):
    """Materialize the vendored CoSQA subset into a temp dir of .py files."""
    src = os.path.join(repo_root, "src/eval/corpus/cosqa/cosqa_retrieval_test_subset.json")
    with open(src) as fh:
        records = json.load(fh)
    corpus_dir = tempfile.mkdtemp(prefix="headtohead_cosqa_")
    tasks = []
    for i, rec in enumerate(records):
        fname = f"{i:03d}_{re.sub(r'[^A-Za-z0-9_]', '_', rec['idx'])[-40:]}.py"
        path = os.path.join(corpus_dir, fname)
        with open(path, "w") as fh:
            fh.write(rec["code"] + "\n")
        tasks.append(
            {"query": rec["doc"], "relevant": [os.path.realpath(path)]}
        )
    return corpus_dir, tasks, "CoSQA subset (63 real web queries, Python)"


REPO_TASKS = [
    ("run SCIP precision ingest", "src/intel/mod.rs"),
    ("merge scip facts into the pdg", "src/intel/merge.rs"),
    ("save pdg with edge level diffs", "src/storage/pdg_store.rs"),
    ("leiden community detection over the graph", "src/graph/community.rs"),
    ("tfidf embedder vocabulary build", "src/cli/index_builder/tfidf.rs"),
    ("feature flag environment variable lookup", "src/feature_flags.rs"),
    ("trim mcp tool output payload", "src/cli/mcp/output/trim.rs"),
    ("prune old index generations retention", "src/storage/generation/retention.rs"),
    ("cosqa external benchmark harness", "src/eval/external_suite.rs"),
    ("filesystem watcher events", "src/cli/watcher.rs"),
    ("embedding worker protocol messages", "src/embed/protocol.rs"),
    ("sqlite schema column migration", "src/storage/schema.rs"),
]


def build_repo_corpus(repo_root):
    tasks = []
    for query, rel in REPO_TASKS:
        tasks.append(
            {"query": query, "relevant": [os.path.realpath(os.path.join(repo_root, rel))]}
        )
    return repo_root, tasks, "LeIndex repository (real large repo, Rust)"


# ── systems ────────────────────────────────────────────────────────────────


def strip_ansi(text):
    return ANSI.sub("", text)


class LeIndexSystem:
    name = "leindex (full hybrid, real binary)"

    def __init__(self, repo_root):
        self.bin = shutil.which("leindex") or os.path.expanduser("~/.cargo/bin/leindex")
        self.repo_root = repo_root

    def available(self):
        return os.path.isfile(self.bin) and os.access(self.bin, os.X_OK)

    def prepare(self, corpus_dir):
        code, _, err = run([self.bin, "index", corpus_dir], timeout=900)
        if code != 0:
            raise RuntimeError(f"leindex index failed: {err[:400]}")
        # Warm the neural path once so the first timed query isn't cold-start.
        run(
            [
                self.bin, "tools", "run", "leindex.search",
                "--args", json.dumps(
                    {"query": "warmup", "top_k": 1, "project_path": corpus_dir}
                ),
            ],
            timeout=300,
        )

    def query(self, corpus_dir, query):
        args = json.dumps({"query": query, "top_k": 10, "project_path": corpus_dir})
        code, out, err = run(
            [self.bin, "tools", "run", "leindex.search", "--args", args], timeout=120
        )
        if code != 0:
            return [], 0
        files = []
        for line in strip_ansi(out).splitlines():
            m = re.match(r"\s*\d+\.\s+(\S+)", line)
            if m:
                files.append(os.path.realpath(m.group(1)))
        files = dedup_keep_order(files)
        return files, len(files) * 160 // 4


class RipgrepSystem:
    """The agent-backend class: aider/cline/kilo/roo search via ripgrep."""

    name = "ripgrep 15 (agent backend: aider/cline/kilo/roo)"

    def __init__(self, _repo_root):
        self.bin = shutil.which("rg")

    def available(self):
        return self.bin is not None

    def prepare(self, corpus_dir):
        pass

    def query(self, corpus_dir, query):
        tokens = [t.lower() for t in re.split(r"[^A-Za-z0-9_]+", query) if t]
        per_file_hits = {}
        for tok in tokens:
            code, out, _ = run(
                [self.bin, "-l", "-i", "--sort", "path", tok, corpus_dir], timeout=60
            )
            if code == 0:
                for path in out.splitlines():
                    per_file_hits[path] = per_file_hits.get(path, 0) + 1
        ranked = [
            path
            for path, _ in sorted(
                per_file_hits.items(), key=lambda kv: (-kv[1], kv[0])
            )
        ][:10]
        read_tokens = 0
        for path in ranked[:3]:
            try:
                with open(path, "rb") as fh:
                    read_tokens += len(fh.read()) // 4
            except OSError:
                pass
        return [os.path.realpath(p) for p in ranked], read_tokens


class CtagsSystem:
    name = "universal-ctags 6.2 (symbol index)"

    def __init__(self, _repo_root):
        self.bin = shutil.which("ctags")

    def available(self):
        return self.bin is not None

    def prepare(self, corpus_dir):
        code, out, _ = run(
            [
                self.bin, "-R", "-x", "--fields=+n",
                "--exclude=target", "--exclude=.leindex",
                "--exclude=node_modules", "--exclude=.git",
                corpus_dir,
            ],
            timeout=600,
        )
        # ctags -x plain: name  kind  line  file ...
        self.symbols = []  # (name_lower, file)
        for line in out.splitlines():
            parts = line.split()
            if len(parts) >= 4:
                path = parts[3]
                if not os.path.isabs(path):
                    path = os.path.join(corpus_dir, path)
                self.symbols.append((parts[0].lower(), path))

    def query(self, corpus_dir, query):
        tokens = [t.lower() for t in re.split(r"[^A-Za-z0-9_]+", query) if t]
        per_file = {}
        for name, path in self.symbols:
            hits = sum(1 for t in tokens if t in name)
            if hits:
                per_file[path] = per_file.get(path, 0) + hits
        ranked = [
            p for p, _ in sorted(per_file.items(), key=lambda kv: (-kv[1], kv[0]))
        ][:10]
        read_tokens = 0
        for path in ranked[:3]:
            full = path if os.path.isabs(path) else os.path.join(corpus_dir, path)
            try:
                with open(full, "rb") as fh:
                    read_tokens += len(fh.read()) // 4
            except OSError:
                pass
        return [os.path.realpath(
            p if os.path.isabs(p) else os.path.join(corpus_dir, p)
        ) for p in ranked], read_tokens


class ZoektSystem:
    """Sourcegraph's zoekt. Two query modes, both reported honestly:

    - mode="and": zoekt's NATIVE semantics (space-separated atoms are ANDed)
      — how Sourcegraph users actually query.
    - mode="or": any-token regex alternation — the same OR semantics every
      other system in this harness receives. Long disjunctive queries flood
      the ranking, which is a real characteristic, not a bug.
    """

    def __init__(self, _repo_root, mode):
        self.mode = mode
        self.name = (
            "zoekt (Sourcegraph, native AND keywords)"
            if mode == "and"
            else "zoekt (Sourcegraph, OR-any-token parity semantics)"
        )
        self.bin_index = shutil.which("zoekt-index") or "/tmp/headtohead/bin/zoekt-index"
        self.bin_query = shutil.which("zoekt") or "/tmp/headtohead/bin/zoekt"

    def available(self):
        return os.path.isfile(self.bin_index) and os.path.isfile(self.bin_query)

    def prepare(self, corpus_dir):
        self.index_dir = tempfile.mkdtemp(prefix="headtohead_zoekt_")
        self.corpus_dir = corpus_dir
        code, _, err = run(
            [self.bin_index, "-index", self.index_dir, corpus_dir],
            timeout=600,
        )
        if code != 0:
            raise RuntimeError(f"zoekt-index failed: {err[:400]}")

    def query(self, corpus_dir, query):
        tokens = [t for t in re.split(r"[^A-Za-z0-9_]+", query) if t]
        if self.mode == "and":
            pattern = " ".join(tokens)
        else:
            pattern = "(" + "|".join(re.escape(t) for t in tokens) + ")"
        code, out, err = run(
            [self.bin_query, "-index_dir", self.index_dir, "-jsonl", pattern],
            timeout=60,
        )
        files = []
        for line in strip_ansi(out).splitlines():
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            name = record.get("FileName", "")
            if name:
                files.append(os.path.realpath(os.path.join(self.corpus_dir, name)))
        ranked = dedup_keep_order(files)[:10]
        read_tokens = 0
        for path in ranked[:3]:
            try:
                with open(path, "rb") as fh:
                    read_tokens += len(fh.read()) // 4
            except OSError:
                pass
        return ranked, read_tokens


# ── runner ─────────────────────────────────────────────────────────────────


def bench_system(system, corpus_dir, tasks):
    rows = []
    per_task = []
    for task in tasks:
        try:
            ranked, tokens = system.query(corpus_dir, task["query"])
        except Exception as exc:  # noqa: BLE001 — record and continue
            print(f"    query failed ({exc})", file=sys.stderr)
            ranked, tokens = [], 0
        r = recall_at_10(ranked, task["relevant"])
        rr = rr_at_10(ranked, task["relevant"])
        rows.append((r, rr, tokens))
        per_task.append((task["query"][:44], r, rr))
    return aggregate(rows), per_task


def fmt_metrics(m):
    return (
        f"| {m['recall10']:.3f} | {m['mrr10']:.3f} | "
        f"{m['tokens']:.0f} | {m['n']} |"
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo-root", default=os.getcwd())
    ap.add_argument("--out", default="docs/baselines/headtohead.md")
    args = ap.parse_args()

    repo_root = os.path.realpath(args.repo_root)
    systems = [
        LeIndexSystem(repo_root),
        RipgrepSystem(repo_root),
        CtagsSystem(repo_root),
        ZoektSystem(repo_root, mode="and"),
        ZoektSystem(repo_root, mode="or"),
    ]

    corpora = [build_cosqa_corpus(repo_root), build_repo_corpus(repo_root)]
    results = {}  # (corpus_id, system_name) -> (metrics, per_task)
    skipped = []

    for corpus_dir, tasks, corpus_desc in corpora:
        corpus_id = os.path.basename(corpus_dir.split("headtohead_")[-1])[:30]
        print(f"== corpus: {corpus_desc}")
        for system in systems:
            if not system.available():
                skipped.append(system.name)
                print(f"   -- skipping {system.name} (not installed)")
                continue
            print(f"   preparing {system.name} ...")
            try:
                system.prepare(corpus_dir)
            except Exception as exc:  # noqa: BLE001
                skipped.append(f"{system.name} (prepare failed: {exc})")
                print(f"   !! prepare failed: {exc}")
                continue
            print(f"   running {len(tasks)} queries through {system.name} ...")
            metrics, per_task = bench_system(system, corpus_dir, tasks)
            results[(corpus_desc, system.name)] = (metrics, per_task)
            print(f"      recall@10={metrics['recall10']:.3f} mrr@10={metrics['mrr10']:.3f}")

    # Render markdown.
    md = ["# Head-to-Head: LeIndex vs Real Indexers (shared corpus, shared metrics)", ""]
    md.append(
        "Every system ran the same queries over the same corpus directories; "
        "recall@10 (= hit@10) and MRR@10 use the same formulas as the in-tree "
        "eval harness. Tokens = chars/4 (rg/ctags/zoekt read top-3 files "
        "whole; leindex returns top-10 snippets)."
    )
    md.append("")
    for corpus_dir, tasks, corpus_desc in corpora:
        md.append(f"## {corpus_desc}")
        md.append("")
        md.append("| system | recall@10 | MRR@10 | avg tokens | queries |")
        md.append("|---|---:|---:|---:|---:|")
        for system in systems:
            key = (corpus_desc, system.name)
            if key in results:
                md.append(f"| {system.name} {fmt_metrics(results[key][0])}")
        md.append("")
    if skipped:
        md.append("## Skipped systems")
        md.append("")
        for s in sorted(set(skipped)):
            md.append(f"- {s}")
        md.append("")
    md.append(
        "Notes: ripgrep/ctags rankings are the standard agent emulations "
        "(token bookkeeping over boolean matches, path-sorted ties); zoekt "
        "applies its own ranking. LeIndex ran the installed production "
        "binary with the full hybrid stack."
    )
    md.append("")

    out_path = os.path.join(repo_root, args.out)
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as fh:
        fh.write("\n".join(md))
    print(f"\nwrote {out_path}")

    # Cleanup materialized corpus (not the repo one).
    for corpus_dir, _, _ in corpora:
        if corpus_dir != repo_root and "headtohead_cosqa_" in corpus_dir:
            shutil.rmtree(corpus_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
