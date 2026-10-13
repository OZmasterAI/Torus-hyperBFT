#!/usr/bin/env python3
"""archive-campaign.py: add one finished bench campaign to the bench archive (docs/perf/bench-archive/).

USAGE
  python3 -I docs/perf/bench-archive/archive-campaign.py <camp> --meta meta/<camp>.toml [--dry-run] [--strict]

  <camp>        campaign tag: the dirs are /home/oz/bench-results-matched/ozarchy-<camp>-*
  --meta F      the only hand-written input (TOML or JSON): one-line description, arms (meaning + cells.md env),
                results-doc section + plan row, prose for the INDEX "Updated" paragraph, the cells.md heading, the
                non-cell dirs and the DELETABLE note, Tier A keep condition, outside-tree items (target dirs,
                worktrees), files to name as kept, the summary-table file + per-arm row labels. Prose stays manual
                in this file. Example with every key explained: meta/p3cloid.toml.
  --dry-run     print a unified diff of every file it would change or create; write nothing (the fence check and the
                smoke check still run and still fail the run)
  --strict      exit 2 without writing anything if any check warns
  --learn-fields  append the summary.json fields flagged by check (a) to summary-fields.txt (after you noted them)
  --bench-root / --archive-dir / --repo / --fields-file / --summary-file   override the defaults (tests use these)

  Typical run: write meta/<camp>.toml, `--dry-run`, read the warnings and the diff, fix the meta, run for real,
  review `git diff`, commit together with the results-doc section and the plan row (both still by hand).

WHAT IT WRITES (mechanical part)
  1. tools/ozarchy-<camp>-tools/: the bench tools dir (campaign / build scripts), each run dir's analyst a-*.py and
     *.txt outputs (not archive-*.txt, not json) under analyst/ (prefixed <sub>- if the campaign has several run
     dirs), md5s-all.txt from ozarchy-<camp>-build/md5s-all.txt or md5s.txt.
  2. INDEX.md: the "Updated" paragraph (after the last one) and one row per dir in sorted position; cells.md: the
     campaign section (before "## action-results:"); DELETABLE.md: the campaign block at the end (Tier A / Tier B
     rm lists with live sizes, outside-tree block). Rows come from each cell's summary.json, node-environ-trie.txt,
     analyst/cells.json (arm of the warm-up), <cell>.campaign.log (start times) and live file sizes.
  3. docs/perf/ozarchy-summary-*.md ([summary] in the meta): one row per arm after the last row of the table, numbers
     from analyst/arms.txt ("per arm mean" table) and {paired:METRIC:DECIMALS} deltas from analyst/paired.txt;
     optional definitions bullet. The "Sources:" sentence stays manual.

REFUSES (exit 3, writes nothing): the campaign is already in INDEX.md / cells.md / DELETABLE.md / tools/, or the
  results-doc section or plan row number is already owned by another entry (INDEX, cells, DELETABLE, summary table).
  It never overwrites or duplicates an entry; to redo one, revert it with git first.
REFUSES (exit 4, writes nothing): the fence check on the whole DELETABLE.md after the append fails (a ``` fence not
  closed, or a block that `bash -n` rejects). Success prints "fence check N/N OK".
REFUSES (exit 5, writes nothing): the smoke check fails (see SMOKE): a declared analyst command exits non-zero, crashes,
  times out or is not in the tools copy, or its output differs from the archived output. Success prints
  "smoke check N/N OK".
WARNS ("WARNING (x) ...", exit 2 with --strict, nothing written then): (a) summary.json fields not in
  summary-fields.txt (new metrics the tables do not show: add a note); (b) analyst *.txt outputs with no a-*.py that
  writes them; (c) deletable / outside-tree paths that do not exist, kept files that are in an rm list or in no
  cell; (d) results-doc heading missing or naming another campaign, plan row missing or not citing the section;
  (x) cell rows that differ from the analyst's archive-*-<camp>.txt; (m) meta gaps; (s) no [[smoke]] in the meta (the
  archived analysis was not test-run; older campaigns still archive, --strict refuses).

SMOKE (the [[smoke]] entries of the meta; proves the archived workflow runs as archived)
  Before writing, the script copies the staged tools tree (exactly what it would write to tools/ozarchy-<camp>-tools/)
  into a temp dir and runs each [[smoke]] entry in order from there, against the real cell dirs under --bench-root,
  with cwd = a temp output dir shared by all entries; then each [[smoke.compare]] is diffed line by line with the
  archived file in the same copy. Run it against one cell where the script allows (e.g. a-cells.py PREFIX r1-c0:c0
  against that cell's line of cells.txt). Copy the arguments from the analyst's driver (run-analysis.sh or the
  session log) verbatim: a wrong prefix then fails here instead of in the archive. Do not declare scripts that write
  to hard-coded paths (a-archive.py): only {out} is a scratch dir.
    script    path in the tools copy; .py runs as `python3 -I <script> <args>`, .sh as `bash <script> <args>`
    args      list of strings; {bench} = --bench-root, {out} = the scratch output dir, {tools} / {analyst} = the
              temp tools copy and its analyst/ dir, {camp} = the campaign tag; other braces stay as written
    timeout   seconds per entry (default 300); on expiry the process group is killed and the check fails
    name      optional label for the report
    [[smoke.compare]] (an entry without one only prepares input for a later entry, e.g. a cells.json)
      want      archived file in the tools copy, e.g. "analyst/cells.txt" (must be there)
      got       "stdout" (default), "stdout+stderr" (like `> f 2>&1`), or a file name in {out}
      select    regex: compare only the lines (of both sides) it matches, e.g. "^r1-c0 " for one cell
      ignore    list of regexes: lines dropped from both sides (timestamps, temp paths)
      tolerance relative tolerance for every number in a line (default 0: exact); the text between numbers must match
  Example and real use: meta/p3cloid.toml, meta/p3cloidprof.toml.

SAFETY: it never deletes anything (it only writes rm commands into DELETABLE.md for a human to run on ozarchy); the
  smoke check's temp dir is removed by Python's TemporaryDirectory.
"""

import argparse
import difflib
import fnmatch
import glob
import json
import os
import re
import stat
import subprocess
import sys
import time
from decimal import ROUND_HALF_UP, Decimal

DEFAULT_BENCH = "/home/oz/bench-results-matched"
DEFAULT_RESULTS_DOC = "docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md"
DEFAULT_MAIN_REPO = "/home/oz/projects/torus-economy/Torus-hyperBFT"
DEFAULT_SETUP = "tw, Classic, standard shape at 300 markets, shared NVMe, commit tail"
DEFAULT_TIER_B = ["buckets.csv", "sampler.csv", "tasks.txt", "val*.log.gz"]
DEFAULT_TIER_A_CELL = ["perf.data", "perf-*.data", "perf.folded"]
CELLS_ANCHOR = "## action-results:"
INDEX_HDR = "| dir | size | date | commit(s) / branch | what it measured | recorded in | st | del A | del B |"
CELLS_HDR = (
    "| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | "
    "matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | "
    "liveness | validity |"
)
CELLS_SEP = "|---" * 20 + "|"
PLACEHOLDERS = ("section", "plan_row", "camp", "target_size", "n_targets")


class Fatal(Exception):
    pass


# ---------------------------------------------------------------- helpers


def h(n):
    """Size as the archive writes it (same as the analysts' a-archive.py)."""
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


def dir_size(path):
    """Apparent size like `du -sb --apparent-size` (coreutils 9): lstat size of every non-directory entry (files,
    symlinks), each inode once; directories themselves count 0."""
    seen = set()
    total = 0

    def add(p):
        nonlocal total
        st = os.lstat(p)
        key = (st.st_dev, st.st_ino)
        if stat.S_ISDIR(st.st_mode) or key in seen:
            return
        seen.add(key)
        total += st.st_size

    add(path)
    if os.path.isdir(path) and not os.path.islink(path):
        for root, dirs, files in os.walk(path):
            for n in dirs + files:
                add(os.path.join(root, n))
    return total


def norm_key(k):
    k = re.sub(r"^val\d+$", "val*", k)
    k = re.sub(r":\d+$", ":*", k)
    return re.sub(r"^\d+$", "*", k)


def summary_fields(obj, prefix=""):
    """Normalised dotted key paths of a summary.json (valN -> val*, name:pid -> name:*; lists and {} are leaves)."""
    out = set()
    if isinstance(obj, dict) and obj:
        for k, v in obj.items():
            out |= summary_fields(
                v, (prefix + "." if prefix else "") + norm_key(str(k))
            )
    else:
        out.add(prefix)
    return out


def fill(text, vals):
    """Replace only the known {placeholders}; other braces (e.g. `{sat,fix}`) stay as written."""
    return re.sub(
        r"\{(%s)\}" % "|".join(PLACEHOLDERS),
        lambda m: str(vals[m.group(1)]),
        text or "",
    )


def plural(n, word):
    return "%d %s%s" % (n, word, "" if n == 1 else "s")


def num(v, f):
    return "" if v is None else f.format(v)


def rm_block(cmd, paths):
    if len(paths) == 1:
        return ["%s '%s'" % (cmd, paths[0])]
    out = [cmd + " \\"] + ["  '%s' \\" % p for p in paths]
    out[-1] = out[-1][:-2]
    return out


def read(path):
    with open(path) as f:
        return f.read()


def write_atomic(path, data, mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".archive-tmp"
    with open(tmp, "wb" if isinstance(data, bytes) else "w") as f:
        f.write(data)
    if mode is not None:
        os.chmod(tmp, mode)
    os.replace(tmp, path)


def load_meta(path):
    if path.endswith(".json"):
        with open(path) as f:
            return json.load(f)
    import tomllib

    with open(path, "rb") as f:
        return tomllib.load(f)


# ---------------------------------------------------------------- campaign model


class Campaign:
    def __init__(self, camp, meta, bench, repo):
        self.camp, self.meta, self.bench, self.repo = (
            camp,
            meta,
            bench.rstrip("/"),
            repo,
        )
        self.warn = []
        self.prefix = "ozarchy-%s-" % camp
        excl = meta.get("exclude_dirs", [])
        names = sorted(
            n
            for n in os.listdir(self.bench)
            if n.startswith(self.prefix)
            and os.path.isdir(os.path.join(self.bench, n))
            and not os.path.islink(os.path.join(self.bench, n))
            and not any(fnmatch.fnmatch(n, e) for e in excl)
        )
        if not names:
            raise Fatal("no dirs %s* in %s" % (self.prefix, self.bench))
        self.cells, self.others = [], []
        for n in names:
            (
                self.cells
                if os.path.isfile(os.path.join(self.bench, n, "summary.json"))
                else self.others
            ).append(n)
        if not self.cells:
            raise Fatal("no cell dir with a summary.json among %s*" % self.prefix)
        order = list(meta.get("dirs", {}))
        rank = {k: i for i, k in enumerate(order)}
        default = ["run", "tools", "build", "stage"]
        self.others.sort(
            key=lambda n: (
                rank.get(n[len(self.prefix) :], len(order)),
                default.index(n[len(self.prefix) :].split("-")[-1])
                if n[len(self.prefix) :].split("-")[-1] in default
                else 9,
                n,
            )
        )
        self.run_dirs = [
            n
            for n in self.others
            if n == self.prefix + "run"
            or (n.startswith(self.prefix) and n.endswith("-run"))
        ]
        self.info = {n: self.cell_info(n) for n in self.cells}
        self.by_time = sorted(self.cells, key=lambda n: (self.info[n]["generated"], n))
        self.date = meta.get("date") or self.info[self.by_time[-1]]["generated"][:10]

    # -- cells

    def parse_name(self, name):
        rest = name[len(self.prefix) :]
        toks = rest.split("-")
        k = next((i for i, t in enumerate(toks) if re.fullmatch(r"\d+m", t)), None)
        if k is None:
            return "", "smoke" in toks, rest
        pre = toks[:k]
        return (
            "-".join(t for t in pre if t != "smoke"),
            "smoke" in pre,
            "-".join(toks[k + 1 :]),
        )

    def run_dir_for(self, sub):
        for cand in ([self.prefix + sub + "-run"] if sub else []) + [
            self.prefix + "run"
        ]:
            if cand in self.others:
                return cand
        return None

    def cell_info(self, name):
        d = os.path.join(self.bench, name)
        with open(os.path.join(d, "summary.json")) as f:
            s = json.load(f)
        sub, smoke, tag = self.parse_name(name)
        md5 = (s.get("binaries") or {}).get("torus_node_md5", "")[:8]
        env_md5 = md5
        p = os.path.join(d, "node-environ-trie.txt")
        if os.path.isfile(p):
            with open(p) as f:
                line = f.readline()
            if "exe_md5=" in line:
                env_md5 = line.split("exe_md5=")[1][:8]
        arms = self.meta.get("arms", {})
        arm = self.meta.get("cell_arms", {}).get(name)
        rd = self.run_dir_for(sub)
        if arm is None and rd:
            cj = os.path.join(self.bench, rd, "analyst", "cells.json")
            if os.path.isfile(cj):
                with open(cj) as f:
                    c = json.load(f)
                if isinstance(c, dict) and isinstance(c.get(tag), dict):
                    arm = c[tag].get("arm")
        if arm is None and tag.split("-")[-1] in arms:
            arm = tag.split("-")[-1]
        if arm not in arms:
            self.warn.append(
                (
                    "m",
                    'cell %s: arm %r not in [arms] (set [cell_arms] "%s" = "<arm>")'
                    % (name, arm, name),
                )
            )
        warm = bool(re.fullmatch(r"w\d+", tag))
        dur = (s.get("cell") or {}).get("duration_s")
        start = None
        cl = os.path.join(self.bench, name + ".campaign.log")
        if os.path.isfile(cl):
            with open(cl, errors="replace") as f:
                m = re.match(r"\[(\d\d:\d\d):\d\d\]", f.readline())
            start = m.group(1) if m else None
        if start is None:
            t0 = (s.get("timing") or {}).get("t_bench0")
            start = (
                time.strftime("%H:%M", time.localtime(t0))
                if t0
                else s.get("generated_at", "")[11:16]
            )
        return {
            "summary": s,
            "sub": sub,
            "smoke": smoke,
            "warm": warm,
            "tag": tag,
            "arm": arm,
            "md5": md5,
            "env_md5": env_md5,
            "dur": dur,
            "start": start,
            "generated": s.get("generated_at", ""),
        }

    def role(self, n):
        i = self.info[n]
        if i["smoke"]:
            return "smoke %s s, excluded" % i["dur"]
        if i["warm"]:
            return "warm-up %s s, excluded" % i["dur"]
        return "%s %s s" % (self.meta.get("cell_role", "throughput"), i["dur"])

    # -- deletable lists

    def tier_lists(self):
        b, meta = self.bench, self.meta.get("deletable", {})
        ta = []
        if "tier_a" in meta:
            for e in meta["tier_a"]:
                pat = e if os.path.isabs(e) else os.path.join(b, e)
                hits = (
                    sorted(glob.glob(pat))
                    if any(c in e for c in "*?[")
                    else ([pat] if os.path.lexists(pat) else [])
                )
                if not hits:
                    self.warn.append(
                        ("c", "Tier A entry does not exist, left out: %s" % pat)
                    )
                ta += [p for p in hits if os.path.isfile(p)]
        else:
            pats = meta.get("tier_a_cell_files", DEFAULT_TIER_A_CELL)
            for n in self.by_time:
                for f in sorted(os.listdir(os.path.join(b, n))):
                    if any(fnmatch.fnmatch(f, p) for p in pats) and os.path.isfile(
                        os.path.join(b, n, f)
                    ):
                        ta.append(os.path.join(b, n, f))
            st = os.path.join(b, self.prefix + "stage")
            if os.path.isdir(st):
                for root, dirs, files in os.walk(st):
                    dirs.sort()
                    for f in sorted(files, key=lambda x: (x != "torus-node", x)):
                        p = os.path.join(root, f)
                        if os.path.isfile(p) and not os.path.islink(p):
                            ta.append(p)
        tb = []
        rules = meta.get("tier_b_rules", [])
        dflt = meta.get("tier_b_files", DEFAULT_TIER_B)
        for n in self.by_time:
            pats = next((r[1] for r in rules if fnmatch.fnmatch(n, r[0])), dflt)
            for f in sorted(os.listdir(os.path.join(b, n))):
                p = os.path.join(b, n, f)
                if (
                    any(fnmatch.fnmatch(f, x) for x in pats)
                    and os.path.isfile(p)
                    and p not in ta
                ):
                    tb.append(p)
        return ta, tb

    def outside(self):
        o = self.meta.get("deletable", {}).get("outside", {})
        tds, wts = [], []
        for t in o.get("target_dirs", []):
            if os.path.isdir(t):
                tds.append(t)
            else:
                self.warn.append(
                    ("c", "outside-tree target dir does not exist, left out: %s" % t)
                )
        for w in o.get("worktrees", []):
            if os.path.isdir(w):
                wts.append(w)
            else:
                self.warn.append(
                    ("c", "outside-tree worktree does not exist, left out: %s" % w)
                )
        return o, tds, wts

    # -- non-cell commit column

    def binaries_text(self):
        t = self.meta.get("binaries") or self.meta.get("dirs", {}).get("binaries")
        if t:
            return t
        bd = os.path.join(self.bench, self.prefix + "build")
        lines = []
        for fn in ("md5s.txt", "md5s-all.txt"):
            if os.path.isfile(os.path.join(bd, fn)):
                lines = [
                    l.split()
                    for l in read(os.path.join(bd, fn)).splitlines()
                    if len(l.split()) >= 3
                ]
                break
        if not lines:
            i = self.info[self.by_time[-1]]
            return "%s (node %s)" % (i["summary"].get("commit", "")[:8], i["env_md5"])
        nodes = [l for l in lines if l[0] != "b"]
        bench = [l for l in lines if l[0] == "b"]
        if len(nodes) == 1:
            parts = ["%s (node %s)" % (nodes[0][1][:8], nodes[0][2][:8])]
        else:
            parts = ["%s %s (%s)" % (l[0], l[1][:8], l[2][:8]) for l in nodes]
        parts += ["bench %s (%s)" % (l[1][:8], l[2][:8]) for l in bench]
        return ", ".join(parts)


# ---------------------------------------------------------------- rendering


def section_ref(c):
    m = c.meta
    doc = m.get("results_doc", DEFAULT_RESULTS_DOC)
    sec = str(m["section"])
    title = None
    p = os.path.join(c.repo, doc)
    if os.path.isfile(p):
        for line in read(p).splitlines():
            mm = re.match(r"^#{1,3} (%s\. .*)$" % re.escape(sec), line)
            if mm:
                title = mm.group(1)
                break
    if title is None:
        if not m.get("section_title"):
            raise Fatal(
                "section %s: no heading '## %s. ...' in %s and no section_title in the meta"
                % (sec, sec, p)
            )
        c.warn.append(
            (
                "d",
                "section %s: no heading in %s yet, using section_title from the meta"
                % (sec, doc),
            )
        )
        title = m["section_title"]
    elif m.get("section_title") and m["section_title"] != title:
        c.warn.append(
            (
                "d",
                "section_title in the meta differs from the %s heading; using the heading"
                % doc,
            )
        )
    if not re.search(r"campaign ozarchy-%s\b(?!-)" % re.escape(c.camp), title):
        c.warn.append(
            (
                "d",
                "section %s heading does not name campaign ozarchy-%s (already used?): %s"
                % (sec, c.camp, title),
            )
        )
    return doc, sec, title


def render(c, ta, tb, tds, wts, o):
    m = c.meta
    doc, sec, title = section_ref(c)
    recorded = "`%s` section '%s'" % (doc, title)
    st = m.get("status", "R")
    dirs_all = c.cells + c.others
    sizes = {n: dir_size(os.path.join(c.bench, n)) for n in dirs_all}
    tsize = sum(dir_size(t) for t in tds)
    vals = {
        "section": sec,
        "plan_row": m.get("plan_row", "?"),
        "camp": c.camp,
        "target_size": h(tsize),
        "n_targets": len(tds),
    }
    sz = {p: os.lstat(p).st_size for p in ta + tb}

    def in_dir(lst, n):
        return sum(sz[p] for p in lst if p.startswith(os.path.join(c.bench, n) + "/"))

    # INDEX rows
    arms = m.get("arms", {})
    rows = []
    for n in c.cells:
        i = c.info[n]
        s = i["summary"]
        commit = "%s @ %s (node %s%s)" % (
            os.path.basename(s.get("worktree", "")),
            s.get("commit", "")[:8],
            i["env_md5"],
            "; wrapper md5 " + i["md5"] if i["md5"] != i["env_md5"] else "",
        )
        desc = m.get("group_description", {}).get(i["sub"], m.get("description", ""))
        a = arms.get(i["arm"], {})
        what = "%s, %s; arm %s (%s); %s" % (
            desc,
            m.get("setup", DEFAULT_SETUP),
            i["arm"],
            a.get("meaning", ""),
            c.role(n),
        )
        if n in m.get("cell_notes", {}):
            what += "; " + m["cell_notes"][n]
        sa, sb = in_dir(ta, n), in_dir(tb, n)
        rows.append(
            "| `%s` | %s | %s | %s | %s | %s | %s | %s | %s |"
            % (
                n,
                h(sizes[n]),
                c.date,
                commit,
                what,
                recorded,
                st,
                h(sa) if sa else "-",
                h(sb) if sb else "-",
            )
        )
    binaries = c.binaries_text()
    dprose = m.get("dirs", {})
    for n in c.others:
        key = n[len(c.prefix) :]
        if key not in dprose:
            c.warn.append(("m", "no [dirs] %s prose for %s (the row says: no prose in the meta)" % (key, n)))
        sa, sb = in_dir(ta, n), in_dir(tb, n)
        rows.append(
            "| `%s` | %s | %s | %s | %s | %s | %s | %s | %s |"
            % (
                n,
                h(sizes[n]),
                c.date,
                binaries,
                fill(dprose.get(key, "(no prose in the meta)"), vals),
                recorded,
                st,
                h(sa) if sa else "-",
                h(sb) if sb else "-",
            )
        )
    rows.sort(key=lambda r: r.split(" | ")[0])

    # INDEX paragraph
    ix = m.get("index", {})
    timing = ix.get("timing")
    if not timing:
        parts = []
        smoke = [n for n in c.by_time if c.info[n]["smoke"]]
        if smoke:
            parts.append(
                "smoke %s-%s"
                % (c.info[smoke[0]]["start"], c.info[smoke[-1]]["generated"][11:16])
            )
        subs = []
        for n in c.by_time:
            if not c.info[n]["smoke"] and c.info[n]["sub"] not in subs:
                subs.append(c.info[n]["sub"])
        for sub in subs:
            g = [
                n
                for n in c.by_time
                if not c.info[n]["smoke"] and c.info[n]["sub"] == sub
            ]
            label = "%s cells" % sub.upper() if len(subs) > 1 and sub else "cells"
            parts.append(
                "%s %s-%s"
                % (label, c.info[g[0]]["start"], c.info[g[-1]]["generated"][11:16])
            )
        timing = ", ".join(parts)
    dsum = ix.get("dirs_summary")
    if not dsum:
        main = [n for n in c.cells if not c.info[n]["smoke"] and not c.info[n]["warm"]]
        warm = [n for n in c.cells if c.info[n]["warm"]]
        smoke = [n for n in c.by_time if c.info[n]["smoke"]]
        parts = []
        if main:
            parts.append("%d %s cells" % (len(main), m.get("cell_role", "throughput")))
        tags = sorted({c.info[n]["tag"] for n in warm})
        if len(warm) == 1:
            parts.append("the warm-up `%s`" % c.info[warm[0]]["tag"])
        elif warm:
            word = {2: "two", 3: "three", 4: "four"}.get(len(warm), str(len(warm)))
            parts.append("%s warm-ups %s" % (word, ", ".join("`%s`" % t for t in tags)))
        if len(smoke) == 1:
            i = c.info[smoke[0]]
            parts.append("the %s s %s smoke cell `%s`" % (i["dur"], i["arm"], smoke[0]))
        elif smoke:
            parts.append(
                "%d smoke cells %s" % (len(smoke), ", ".join("`%s`" % n for n in smoke))
            )
        for k, n in enumerate(c.others):
            parts.append("`%s`" % n if k == 0 else "`-%s`" % n[len(c.prefix) :])
        dsum = ", ".join(parts)
    where = "the ozarchy doc" if doc == DEFAULT_RESULTS_DOC else "`%s`" % doc
    para = (
        "**Updated %s (%s).** After `ozarchy-%s` finished (%s) its %d dirs were added (%s: %s; section %s of %s)."
        % (
            c.date,
            c.camp,
            c.camp,
            timing,
            len(dirs_all),
            h(sum(sizes.values())),
            dsum,
            sec,
            where,
        )
    )
    if ix.get("update"):
        para += " " + fill(ix["update"], vals)
    para += " The cells are in `cells.md`, the deletable files in `DELETABLE.md`."

    # cells.md section
    heading = m.get("cells", {}).get("heading")
    if not heading:
        c.warn.append(("m", "no [cells] heading; using the description"))
        heading = m.get("description", c.camp)
    crow = []
    groups = []
    for n in c.by_time:
        if c.info[n]["sub"] not in groups:
            groups.append(c.info[n]["sub"])
    for g in groups:
        for n in sorted(x for x in c.cells if c.info[x]["sub"] == g):
            i = c.info[n]
            s = i["summary"]
            hd, cl = s.get("headline") or {}, s.get("cell") or {}
            env = "; ".join(
                x
                for x in (m.get("env_base", ""), arms.get(i["arm"], {}).get("env", ""))
                if x
            )
            env += "; smoke" if i["smoke"] else "; warm-up" if i["warm"] else ""
            valid = (s.get("validity") or {}).get("verdict")
            crow.append(
                "| `%s` | %s | %s | %s @ %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |"
                % (
                    n,
                    s.get("status") if valid == "ACCEPT" else "INVALID",
                    i["generated"][:16].replace("T", " "),
                    os.path.basename(s.get("worktree", "")),
                    s.get("commit", "")[:8],
                    i["env_md5"],
                    num(cl.get("markets"), "{:d}"),
                    num(cl.get("duration_s"), "{:d}"),
                    num(cl.get("block_cap"), "{:d}"),
                    env,
                    num(hd.get("matched_s_avg"), "{:,.1f}"),
                    num(hd.get("matched_s_first120"), "{:,.1f}"),
                    num(hd.get("matched_s_best60"), "{:,.1f}"),
                    num(hd.get("placed_s_avg"), "{:,.1f}"),
                    num(hd.get("blk_s_avg"), "{:.1f}"),
                    num(hd.get("txs_per_block_avg"), "{:.1f}"),
                    num(hd.get("chain_ms"), "{:.2f}"),
                    num(hd.get("engine_ms_per_1k_fills"), "{:.2f}"),
                    hd.get("agreement_verdict") or "",
                    (s.get("liveness") or {}).get("verdict") or "",
                    valid or "",
                )
            )
    cells_block = (
        ["## %s: %s" % (c.camp, fill(heading, vals)), "", CELLS_HDR, CELLS_SEP]
        + crow
        + [""]
    )

    # DELETABLE block
    dm = m.get("deletable", {})
    sa_all, sb_all = sum(sz[p] for p in ta), sum(sz[p] for p in tb)
    order = c.by_time + c.others
    head = "### `%s` (A %s, B %s; dirs: %s" % (
        c.camp,
        h(sa_all),
        h(sb_all),
        ", ".join("`%s`" % n for n in order),
    )
    if tds:
        head += "; outside the tree: %s, %s" % (
            plural(len(tds), "target dir"),
            h(tsize),
        )
    blk = ["", head + ")", ""]
    note = fill(dm.get("note", ""), vals)
    if not note:
        c.warn.append(
            ("m", "no [deletable] note (what the section is built on, what to keep)")
        )
    extra = [k for k in dm.get("kept", []) if "`%s`" % k not in note]
    if extra:
        note = (note + " " if note else "") + "Also kept: %s." % ", ".join(
            "`%s`" % k for k in extra
        )
    if note:
        blk += [note, ""]
    if ta:
        keep = dm.get("tier_a_keep")
        blk += (
            [
                "Tier A, %s, %s%s:"
                % (
                    h(sa_all),
                    plural(len(ta), "file"),
                    " (%s)" % fill(keep, vals) if keep else "",
                ),
                "",
                "```",
            ]
            + rm_block("rm --", ta)
            + ["```", ""]
        )
    if tb:
        blk += (
            ["Tier B, %s, %s:" % (h(sb_all), plural(len(tb), "file")), "", "```"]
            + rm_block("rm --", tb)
            + ["```"]
        )
    else:
        blk += ["Tier B: none."]
    if tds or wts or o.get("extra_lines"):
        onote = (
            o.get("note")
            or "the build target dir(s), {target_size} apparent, and the build worktree(s) (`git worktree remove`)"
        )
        lines = (rm_block("rm -r --", tds) if tds else []) + list(
            o.get("extra_lines", [])
        )
        lines += [
            "git -C %s worktree remove %s" % (o.get("repo", DEFAULT_MAIN_REPO), w)
            for w in wts
        ]
        blk += (
            ["", "Outside the tree: %s:" % fill(onote, vals), "", "```"]
            + lines
            + ["```"]
        )
    return {
        "para": para,
        "rows": rows,
        "cells_block": cells_block,
        "deletable": blk,
        "sizes": sizes,
        "tsize": tsize,
        "sa": sa_all,
        "sb": sb_all,
        "recorded": recorded,
    }


# ---------------------------------------------------------------- archive file edits


def present(c, texts, tools_dir):
    idx, cel, dl = texts
    hits = []
    if re.search(r"^\*\*Updated \S+ \(%s\)\.\*\*" % re.escape(c.camp), idx, re.M):
        hits.append("INDEX.md paragraph")
    names = set(c.cells + c.others)
    if any(
        l.startswith("| `") and l.split(" | ")[0][3:-1] in names
        for l in idx.split("\n")
    ):
        hits.append("INDEX.md rows")
    if re.search(r"^## %s: " % re.escape(c.camp), cel, re.M):
        hits.append("cells.md section")
    if re.search(r"^### `%s` \(" % re.escape(c.camp), dl, re.M):
        hits.append("DELETABLE.md block")
    if os.path.isdir(tools_dir):
        hits.append(
            os.path.relpath(tools_dir, os.path.dirname(os.path.dirname(tools_dir)))
        )
    return hits


def q(v, nd):
    """Half-up decimal rounding of a number as printed by the analyst scripts (9.35 -> 9.4, like a human)."""
    return Decimal(v).quantize(Decimal(1).scaleb(-nd), rounding=ROUND_HALF_UP)


def parse_arms(path):
    """analyst/arms.txt "per arm mean (sd):" table -> {arm: {metric: str}}."""
    lines = read(path).split("\n")
    k = next((i for i, l in enumerate(lines) if l.startswith("per arm mean")), None)
    if k is None or k + 1 >= len(lines):
        raise Fatal("%s: no 'per arm mean' table" % path)
    arms = lines[k + 1].split()[1:]
    out = {a: {} for a in arms}
    for l in lines[k + 2:]:
        if not l.strip():
            break
        name, rest = l.split(None, 1) if len(l.split(None, 1)) == 2 else (l.strip(), "")
        vals = re.findall(r"(nan|-?\d+(?:\.\d+)?)\s*(?:\(\s*-?[\d.]+\))?", rest)
        for a, v in zip(arms, vals, strict=False):
            out[a][name] = v
    return out


def parse_paired(path):
    """analyst/paired.txt -> {metric: (pct, lo, hi)} as printed (strings)."""
    num_re = r"[+-]?(?:\d+(?:\.\d+)?|nan)"
    pat = re.compile(r"^(\S+)\s.*?\]\s+(%s)%%\s+\[\s*(%s),\s*(%s)\]" % (num_re, num_re, num_re))
    out = {}
    for l in read(path).splitlines():
        m = pat.match(l)
        if m:
            out[m.group(1)] = m.group(2, 3, 4)
    return out


def render_summary(c, text):
    """Per-arm rows for the ozarchy summary table (+ optional definitions bullet) -> new text."""
    sm = c.meta["summary"]
    rows = []
    for spec in sm.get("rows", []):
        rd = c.run_dir_for(spec.get("run", ""))
        an = os.path.join(c.bench, rd or "", "analyst")
        try:
            arms = parse_arms(os.path.join(an, "arms.txt"))
        except (OSError, Fatal) as e:
            raise Fatal("summary row %s: %s" % (spec.get("arm"), e)) from e
        v = arms.get(spec["arm"])
        if v is None:
            raise Fatal("summary row: arm %r not in %s/arms.txt" % (spec["arm"], an))

        def g(metric, nd, comma=False, _v=v, _arm=spec["arm"]):
            x = _v.get(metric)
            if x is None or x == "nan":
                c.warn.append(("m", "summary row %s: no %s in arms.txt" % (_arm, metric)))
                return "?"
            return ("{:,.%df}" if comma else "{:.%df}").replace("%d", str(nd)).format(q(x, nd))

        st = v.get("ck_settle")
        settle = "n/a" if st in (None, "nan") or q(st, 1) == 0 else g("ck_settle", 1)
        cols = {"matched": g("ms", 0, True), "blk": g("blk", 2), "p99": g("ci99", 0, True), "cpu": g("cpu", 2),
                "rss": "%s / %s" % (g("rss_mean", 2), g("rss_pk", 2)), "writes": g("wr", 1), "settle": settle}
        cols.update(spec.get("cols", {}))
        delta = spec.get("delta", "")
        if "{paired:" in delta:
            paired = parse_paired(os.path.join(an, "paired.txt"))

            def sub(m, _p=paired):
                if m.group(1) not in _p:
                    raise Fatal("summary delta: no %s row in paired.txt" % m.group(1))
                nd = int(m.group(2))
                pct, lo, hi = ("{:+.%df}" % nd).format(q(_p[m.group(1)][0], nd)), *(
                    ("{:+.%df}" % nd).format(q(x, nd)) for x in _p[m.group(1)][1:])
                return "%s%% [%s, %s]" % (pct, lo, hi)

            delta = re.sub(r"\{paired:([^:}]+):(\d+)\}", sub, delta)
        cells = [spec["label"], spec["text"], cols["matched"], cols["blk"], cols["p99"], cols["cpu"], cols["rss"],
                 cols["writes"], cols["settle"], delta]
        rows.append("| " + " | ".join(cells) + (" |" if delta else "|"))
    L = text.split("\n")
    hi = next((i for i, l in enumerate(L) if l.startswith("| Sec |")), None)
    if hi is None:
        raise Fatal("summary file: no '| Sec |' table header")
    end = hi + 1
    while end < len(L) and L[end].startswith("|"):
        end += 1
    L[end:end] = rows
    note = sm.get("note")
    if note:
        anchor = sm.get("note_before")
        k = next((i for i, l in enumerate(L) if anchor and l.startswith(anchor)), None)
        if k is None:
            c.warn.append(("m", "summary note_before %r not found: note appended at the end" % anchor))
            if L and L[-1] == "":
                L.pop()
            L += note.split("\n") + [""]
        else:
            L[k:k] = note.split("\n")
    return "\n".join(L)


def fence_check(text, name="DELETABLE.md"):
    """Every ``` fence closes and every fenced block passes `bash -n`. -> (ok, total, errors)."""
    blocks, cur = [], None
    for i, l in enumerate(text.split("\n"), 1):
        if l.strip().startswith("```"):
            if cur is None:
                cur = (i, [])
            else:
                blocks.append(cur)
                cur = None
        elif cur is not None:
            cur[1].append(l)
    errors = []
    if cur is not None:
        errors.append("%s:%d: code fence opened here is not closed" % (name, cur[0]))
    ok = 0
    for start, body in blocks:
        r = subprocess.run(["bash", "-n"], input="\n".join(body) + "\n", capture_output=True, text=True)
        if r.returncode:
            msg = r.stderr.strip().splitlines()
            errors.append("%s:%d: bash -n: %s" % (name, start, msg[-1] if msg else "rc %d" % r.returncode))
        else:
            ok += 1
    return ok, len(blocks) + (cur is not None), errors


SMOKE_KEYS = {"script", "args", "compare", "timeout", "name"}
CMP_KEYS = {"got", "want", "select", "ignore", "tolerance"}
SMOKE_VARS = ("bench", "out", "tools", "analyst", "camp")
NUM_RE = re.compile(r"([-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?)")


def smoke_specs(meta):
    """Validate the meta's [[smoke]] entries -> list of dicts (empty if none). Malformed -> Fatal."""
    specs = meta.get("smoke", [])
    if isinstance(specs, dict):
        specs = [specs]
    if not isinstance(specs, list):
        raise Fatal("[[smoke]] must be a list of tables")
    for k, s in enumerate(specs, 1):
        where = "smoke entry %d" % k
        if (
            not isinstance(s, dict)
            or not isinstance(s.get("script"), str)
            or not s["script"]
        ):
            raise Fatal(
                '%s: needs script = "<path in the tools copy, e.g. analyst/a-cells.py>"'
                % where
            )
        bad = set(s) - SMOKE_KEYS
        if bad:
            raise Fatal(
                "%s: unknown keys %s (known: %s)"
                % (where, sorted(bad), sorted(SMOKE_KEYS))
            )
        if os.path.splitext(s["script"])[1] not in (".py", ".sh"):
            raise Fatal(
                "%s: script must be a .py (run with python3 -I) or .sh (run with bash): %s"
                % (where, s["script"])
            )
        if not isinstance(s.get("args", []), list) or not all(
            isinstance(a, str) for a in s.get("args", [])
        ):
            raise Fatal("%s: args must be a list of strings" % where)
        t = s.get("timeout", 300)
        if isinstance(t, bool) or not isinstance(t, (int, float)) or t <= 0:
            raise Fatal("%s: timeout must be a positive number of seconds" % where)
        cmps = s.get("compare", [])
        if not isinstance(cmps, list):
            raise Fatal("%s: compare must be a list of tables" % where)
        for cm in cmps:
            if not isinstance(cm, dict) or not isinstance(cm.get("want"), str):
                raise Fatal(
                    '%s: each compare needs want = "<archived file in the tools copy>"'
                    % where
                )
            bad = set(cm) - CMP_KEYS
            if bad:
                raise Fatal(
                    "%s: unknown compare keys %s (known: %s)"
                    % (where, sorted(bad), sorted(CMP_KEYS))
                )
            for rx in ([cm["select"]] if "select" in cm else []) + list(
                cm.get("ignore", [])
            ):
                try:
                    re.compile(rx)
                except (re.error, TypeError) as e:
                    raise Fatal("%s: bad regex %r: %s" % (where, rx, e)) from e
    if specs and not any(s.get("compare") for s in specs):
        raise Fatal("[[smoke]] declares no compare: nothing would be checked")
    return specs


def smoke_fill(text, vals):
    return re.sub(r"\{(%s)\}" % "|".join(SMOKE_VARS), lambda m: vals[m.group(1)], text)


def smoke_lines(text, cm):
    sel = re.compile(cm["select"]) if "select" in cm else None
    ign = [re.compile(x) for x in cm.get("ignore", [])]
    return [
        l
        for l in text.splitlines()
        if (sel is None or sel.search(l)) and not any(x.search(l) for x in ign)
    ]


def smoke_same(a, b, tol):
    """Lines equal, or (tol > 0) equal outside the numbers and every number within relative tolerance tol."""
    if a == b:
        return True
    if not tol:
        return False
    pa, pb = NUM_RE.split(a), NUM_RE.split(b)
    if len(pa) != len(pb):
        return False
    for i, (x, y) in enumerate(zip(pa, pb, strict=True)):
        if i % 2 == 0:
            if x != y:
                return False
        elif abs(float(x) - float(y)) > tol * max(abs(float(x)), abs(float(y))):
            return False
    return True


def run_smoke(c, specs, staged):
    """Run the declared analyst commands from a temp copy of the staged tools tree, against the real cells under the
    bench root, and diff their output with the archived files in the same copy. -> (ok, total, errors)."""
    import signal
    import tempfile

    errors, ok = [], 0
    with tempfile.TemporaryDirectory(prefix="archive-smoke-") as tmp:
        tools, out = os.path.join(tmp, "tools"), os.path.join(tmp, "out")
        os.makedirs(out)
        for rel, data, mode in staged:
            write_atomic(os.path.join(tools, rel), data, mode)
        vals = {
            "bench": c.bench,
            "out": out,
            "tools": tools,
            "analyst": os.path.join(tools, "analyst"),
            "camp": c.camp,
        }
        for k, s in enumerate(specs, 1):
            label = "smoke %d (%s)" % (k, s.get("name") or s["script"])
            script = os.path.join(tools, s["script"])
            if not os.path.isfile(script):
                errors.append("%s: %s is not in the tools copy" % (label, s["script"]))
                continue
            interp = [sys.executable, "-I"] if script.endswith(".py") else ["bash"]
            cmd = interp + [script] + [smoke_fill(a, vals) for a in s.get("args", [])]
            t = s.get("timeout", 300)
            try:
                p = subprocess.Popen(
                    cmd,
                    cwd=out,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    start_new_session=True,
                )
            except OSError as e:
                errors.append("%s: cannot run %s: %s" % (label, " ".join(cmd), e))
                continue
            try:
                so, se = p.communicate(timeout=t)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(p.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                p.communicate()
                errors.append(
                    "%s: timed out after %g s: %s" % (label, t, " ".join(cmd))
                )
                continue
            so, se = so.decode(errors="replace"), se.decode(errors="replace")
            if p.returncode:
                tail = se.strip().splitlines()[-6:] or ["(no stderr)"]
                errors.append(
                    "%s: exit status %d: %s\n    %s"
                    % (label, p.returncode, " ".join(cmd), "\n    ".join(tail))
                )
                continue
            bad = False
            for cm in s.get("compare", []):
                got_k = cm.get("got", "stdout")
                if got_k == "stdout":
                    got = so
                elif got_k == "stdout+stderr":
                    got = so + se
                else:
                    gp = os.path.join(out, smoke_fill(got_k, vals))
                    if not os.path.isfile(gp):
                        errors.append("%s: output %s was not written" % (label, got_k))
                        bad = True
                        continue
                    got = read(gp)
                wp = os.path.join(tools, cm["want"])
                if not os.path.isfile(wp):
                    errors.append(
                        "%s: archived output %s is not in the tools copy"
                        % (label, cm["want"])
                    )
                    bad = True
                    continue
                want_l, got_l = smoke_lines(read(wp), cm), smoke_lines(got, cm)
                if not want_l:
                    errors.append(
                        "%s: no line of %s left after select %r / ignore: nothing to compare"
                        % (label, cm["want"], cm.get("select"))
                    )
                    bad = True
                    continue
                tol = float(cm.get("tolerance", 0))
                if len(want_l) != len(got_l) or not all(
                    smoke_same(a, b, tol) for a, b in zip(want_l, got_l, strict=True)
                ):
                    d = list(
                        difflib.unified_diff(
                            want_l,
                            got_l,
                            "archived " + cm["want"],
                            "smoke " + got_k,
                            lineterm="",
                        )
                    )
                    more = (
                        ["... (%d more diff lines)" % (len(d) - 40)]
                        if len(d) > 40
                        else []
                    )
                    errors.append(
                        "%s: output differs from the archived %s%s:\n    %s"
                        % (
                            label,
                            cm["want"],
                            " (tolerance %g)" % tol if tol else "",
                            "\n    ".join(d[:40] + more),
                        )
                    )
                    bad = True
            if not bad:
                ok += 1
    return ok, len(specs), errors


def udiff(old, new, label, new_file=False):
    a = old.splitlines(True) if old else []
    b = new.splitlines(True)
    out = difflib.unified_diff(a, b, "/dev/null" if new_file else "a/" + label, "b/" + label)
    return "".join(l if l.endswith("\n") else l + "\n\\ No newline at end of file\n" for l in out)


def insert_index(idx, para, rows):
    L = idx.split("\n")
    upd = [k for k, l in enumerate(L) if l.startswith("**Updated ")]
    if upd:
        k = upd[-1] + 1
        L[k:k] = ["", para]
    else:
        k = next((k for k, l in enumerate(L) if l.startswith("* **Scope.**")), None)
        if k is None:
            raise Fatal(
                "INDEX.md: no **Updated paragraph and no * **Scope.** line to anchor the paragraph"
            )
        L[k:k] = [para, ""]
    try:
        hi = L.index(INDEX_HDR)
    except ValueError:
        raise Fatal("INDEX.md: dir table header not found") from None
    j = hi + 2
    end = j
    while end < len(L) and L[end].startswith("| "):
        end += 1
    first = rows[0].split(" | ")[0]
    pos = j
    for k in range(j, end):
        if L[k].split(" | ")[0] < first:
            pos = k + 1
    L[pos:pos] = rows
    return "\n".join(L)


def insert_cells(cel, block):
    L = cel.split("\n")
    k = next((k for k, l in enumerate(L) if l.startswith(CELLS_ANCHOR)), None)
    if k is None:
        if L and L[-1] == "":
            L.pop()
        L += [""] + block
        return "\n".join(L) + ("" if block[-1] == "" else "\n")
    L[k:k] = block
    return "\n".join(L)


def append_deletable(dl, block):
    if not dl.endswith("\n"):
        dl += "\n"
    return dl + "\n".join(block) + "\n"


# ---------------------------------------------------------------- tools copy


def tools_plan(c):
    """[(src, dst relative to tools/ozarchy-<camp>-tools/)], md5s-all source or None."""
    plan = []
    b = c.bench
    td = os.path.join(b, c.prefix + "tools")
    excl = c.meta.get("tools", {}).get("exclude", [])
    if os.path.isdir(td):
        for root, dirs, files in os.walk(td):
            dirs.sort()
            for f in sorted(files):
                p = os.path.join(root, f)
                rel = os.path.relpath(p, td)
                if (
                    os.path.isfile(p)
                    and not os.path.islink(p)
                    and rel != "md5s-all.txt"
                    and not any(fnmatch.fnmatch(rel, e) for e in excl)
                ):
                    plan.append((p, rel))
    multi = len(c.run_dirs) > 1
    py_seen = {}
    for rd in c.run_dirs:
        sub = rd[len(c.prefix) : -len("-run")] if rd != c.prefix + "run" else ""
        an = os.path.join(b, rd, "analyst")
        files = sorted(os.listdir(an)) if os.path.isdir(an) else []
        for f in files:
            p = os.path.join(an, f)
            if not os.path.isfile(p) or any(fnmatch.fnmatch(f, e) for e in excl):
                continue
            if fnmatch.fnmatch(f, "a-*.py"):
                with open(p, "rb") as fh:
                    data = fh.read()
                if f in py_seen and py_seen[f] != data:
                    plan.append((p, "analyst/%s-%s" % (sub, f)))
                elif f not in py_seen:
                    py_seen[f] = data
                    plan.append((p, "analyst/" + f))
            elif f.endswith(".txt") and not f.startswith("archive-"):
                plan.append(
                    (p, "analyst/%s%s" % (sub + "-" if multi and sub else "", f))
                )
        for f in sorted(os.listdir(os.path.join(b, rd))):
            p = os.path.join(b, rd, f)
            if f.endswith(".txt") and os.path.isfile(p):
                plan.append(
                    (p, "analyst/%s%s" % (sub + "-" if multi and sub else "", f))
                )
    for src, dst in c.meta.get("tools", {}).get("extra", []):
        plan.append((src if os.path.isabs(src) else os.path.join(b, src), dst))
    bd = os.path.join(b, c.prefix + "build")
    md5 = next(
        (
            os.path.join(bd, f)
            for f in ("md5s-all.txt", "md5s.txt")
            if os.path.isfile(os.path.join(bd, f))
        ),
        None,
    )
    return plan, md5


# ---------------------------------------------------------------- checks


def check_fields(c, fields_file):
    known = set()
    if os.path.isfile(fields_file):
        known = {
            l.strip()
            for l in read(fields_file).splitlines()
            if l.strip() and not l.startswith("#")
        }
    else:
        c.warn.append(("a", "no fields file %s: check (a) skipped" % fields_file))
        return []
    pats = c.meta.get("checks", {}).get("known_fields", [])
    count = {}
    for n in c.cells:
        for f in summary_fields(c.info[n]["summary"]):
            if f not in known and not any(fnmatch.fnmatchcase(f, p) for p in pats):
                count[f] = count.get(f, 0) + 1
    if count:
        c.warn.append(
            (
                "a",
                "summary.json fields not known to the script (%d; the tables do not show them, add a note "
                "to the write-up, then list them in [checks] known_fields or rerun with --learn-fields): %s"
                % (
                    len(count),
                    ", ".join("%s (%d cells)" % (f, count[f]) for f in sorted(count)),
                ),
            )
        )
    return sorted(count)


def check_outputs(c):
    silenced = c.meta.get("checks", {}).get("produced_by", {})
    orphans = []
    for rd in c.run_dirs:
        an = os.path.join(c.bench, rd, "analyst")
        if not os.path.isdir(an):
            continue
        scripts = {}
        for f in sorted(os.listdir(an)):
            if fnmatch.fnmatch(f, "a-*.py"):
                scripts[f] = read(os.path.join(an, f)).splitlines()
        for f in sorted(os.listdir(an)):
            if not f.endswith(".txt") or f.startswith("archive-") or f in silenced:
                continue
            if "a-%s.py" % f[:-4] in scripts:
                continue
            lit = re.compile(r"""["'/]%s["']""" % re.escape(f))
            if any(
                lit.search(l) and ("open(" in l or ".write" in l or ">" in l)
                for lines in scripts.values()
                for l in lines
            ):
                continue
            orphans.append("%s/analyst/%s" % (rd, f))
    if orphans:
        c.warn.append(
            (
                "b",
                "analyst outputs with no saved script that writes them (save it as analyst/a-<name>.py "
                "or name the producer in [checks] produced_by): %s"
                % ", ".join(orphans),
            )
        )


def check_kept(c, ta, tb):
    for k in c.meta.get("deletable", {}).get("kept", []):
        bad = [p for p in ta + tb if fnmatch.fnmatch(os.path.basename(p), k)]
        if bad:
            c.warn.append(
                (
                    "c",
                    "kept file `%s` is in an rm list (%d paths, e.g. %s)"
                    % (k, len(bad), bad[0]),
                )
            )
        if not any(
            fnmatch.fnmatch(f, k)
            for n in c.cells
            for f in os.listdir(os.path.join(c.bench, n))
        ):
            c.warn.append(("c", "kept file `%s` is in no cell dir" % k))


def used_numbers(c, named_texts):
    """Refusal check: is the section / plan row already owned by another entry? -> list of 'file:line: text'."""
    m = c.meta
    sec = re.escape(str(m["section"]))
    own = re.compile(r"ozarchy-%s(?![a-z0-9])|\(%s\)|`%s`" % ((re.escape(c.camp),) * 3))
    # ownership forms only (INDEX recorded-in column, "section N of the ...", "results doc section N,", a cells.md
    # heading "(section N)", summary-table rows "| N ..." and bullets "- **N (...)"); "see section 7.1" is not one
    sec_pats = [
        re.compile(r"section '%s\. " % sec),
        re.compile(r"\bsections? (?:[0-9a-z]+(?:, | and ))*%s(?![0-9a-z.]) of " % sec),
        re.compile(r"\bresults doc section %s[,)]" % sec),
        re.compile(r"\(section %s\)" % sec),
    ]
    sum_pats = [re.compile(r"^\| *%s(?: [^|]*)? \|" % sec), re.compile(r"^- \*\*%s[ (:*]" % sec)]
    row = m.get("plan_row")
    row_pat = re.compile(r"\bplan row %s(?![0-9])" % re.escape(str(row))) if row is not None else None
    hits = []
    for name, text, is_summary in named_texts:
        for ln, l in enumerate(text.split("\n"), 1):
            if own.search(l) and not is_summary:
                continue
            if any(p.search(l) for p in (sum_pats if is_summary else sec_pats)):
                hits.append("section %s already in %s:%d: %s" % (m["section"], name, ln, l[:160]))
            if row_pat and row_pat.search(l):
                hits.append("plan row %s already in %s:%d: %s" % (row, name, ln, l[:160]))
    return hits


def check_plan_row(c):
    m = c.meta
    row = m.get("plan_row")
    if row is None or not m.get("plan_doc"):
        return
    p = os.path.join(c.repo, m["plan_doc"])
    if not os.path.isfile(p):
        c.warn.append(("d", "plan doc %s not found" % p))
        return
    rl = [l for l in read(p).splitlines() if re.match(r"^\| *%s *\|" % re.escape(str(row)), l)]
    if not rl:
        c.warn.append(("d", "plan row %s not in %s (write it first?)" % (row, m["plan_doc"])))
    elif not re.search(r"\bsection %s(?![0-9a-z])" % re.escape(str(m["section"])), rl[0]):
        c.warn.append(("d", "plan row %s in %s does not mention section %s (used by another campaign?): %s"
                       % (row, m["plan_doc"], m["section"], rl[0][:160])))


def cross_check(c, r):
    """Compare generated cell rows with the analyst's a-archive.py output, if there is one."""
    gen_cells = {l.split(" | ")[0][3:-1]: l for l in r["rows"]}
    gen_c = {
        l.split(" | ")[0][3:-1]: l for l in r["cells_block"] if l.startswith("| `")
    }
    for rd in c.run_dirs:
        an = os.path.join(c.bench, rd, "analyst")
        for kind, gen, cols in (
            ("index", gen_cells, (0, 2, 3, 4, 6)),
            ("cells", gen_c, None),
        ):
            p = os.path.join(an, "archive-%s-%s.txt" % (kind, c.camp))
            if not os.path.isfile(p):
                continue
            for l in read(p).splitlines():
                if not l.startswith("| `"):
                    continue
                n = l.split(" | ")[0][3:-1]
                if n not in gen or n not in c.cells:
                    continue
                a, g = l.split(" | "), gen[n].split(" | ")
                if cols:
                    a, g = (
                        [a[i] for i in cols if i < len(a)],
                        [g[i] for i in cols if i < len(g)],
                    )
                if a != g:
                    c.warn.append(
                        (
                            "x",
                            "%s row of %s differs from %s"
                            % (kind, n, os.path.relpath(p, c.bench)),
                        )
                    )


# ---------------------------------------------------------------- main


def main(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("camp")
    ap.add_argument("--meta", required=True)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--strict", action="store_true")
    ap.add_argument("--learn-fields", action="store_true")
    ap.add_argument("--bench-root", default=DEFAULT_BENCH)
    ap.add_argument("--archive-dir", default=here)
    ap.add_argument("--repo", default=None)
    ap.add_argument("--fields-file", default=os.path.join(here, "summary-fields.txt"))
    ap.add_argument("--summary-file", default=None, help="override [summary] file (path)")
    a = ap.parse_args(argv)
    archive = os.path.abspath(a.archive_dir)
    repo = os.path.abspath(a.repo or os.path.join(archive, "..", "..", ".."))
    try:
        meta = load_meta(a.meta)
        if meta.get("camp", a.camp) != a.camp:
            raise Fatal("meta camp %r does not match %r" % (meta.get("camp"), a.camp))
        if "section" not in meta:
            raise Fatal("meta has no section")
        specs = smoke_specs(meta)
        c = Campaign(a.camp, meta, a.bench_root, repo)
        names = ["INDEX.md", "cells.md", "DELETABLE.md"]
        paths = [os.path.join(archive, f) for f in names]
        texts = [read(p) for p in paths]
        labels = list(names)
        spath = None
        if meta.get("summary"):
            spath = os.path.abspath(a.summary_file or os.path.join(repo, meta["summary"]["file"]))
            stext = read(spath)
            labels.append(os.path.relpath(spath, repo) if spath.startswith(repo + "/") else meta["summary"]["file"])
        else:
            c.warn.append(("m", "no [summary] in the meta: summary table not updated"))
        tools_dir = os.path.join(archive, "tools", "ozarchy-%s-tools" % a.camp)
        refuse = ["campaign %s already in the archive: %s" % (a.camp, h_) for h_ in present(c, texts, tools_dir)]
        named = [(n, t, False) for n, t in zip(names, texts, strict=True)]
        if spath:
            named.append((labels[3], stext, True))
        refuse += used_numbers(c, named)
        if refuse:
            for x in refuse:
                print("archive-campaign: refused: %s" % x, file=sys.stderr)
            print("archive-campaign: an entry exists already; nothing written (never overwrites or duplicates; "
                  "revert the old entry with git if it must be redone)", file=sys.stderr)
            return 3
        ta, tb = c.tier_lists()
        o, tds, wts = c.outside()
        unknown = check_fields(c, a.fields_file)
        check_outputs(c)
        check_kept(c, ta, tb)
        check_plan_row(c)
        r = render(c, ta, tb, tds, wts, o)
        cross_check(c, r)
        plan, md5 = tools_plan(c)
        new = [insert_index(texts[0], r["para"], r["rows"]), insert_cells(texts[1], r["cells_block"]),
               append_deletable(texts[2], r["deletable"])]
        if spath:
            paths.append(spath)
            texts.append(stext)
            new.append(render_summary(c, stext))
        # read every tool source now: a missing or unreadable one must stop the run before anything is written
        rel_tools = os.path.relpath(tools_dir, archive)
        sources = [(src, dst, "%s/%s" % (rel_tools, dst)) for src, dst in plan]
        if md5:
            sources.append((md5, "md5s-all.txt", rel_tools + "/md5s-all.txt"))
        files = []
        for src, dst, label in sources:
            try:
                with open(src, "rb") as f:
                    data = f.read()
                mode = 0o644 if src == md5 else stat.S_IMODE(os.stat(src).st_mode)
            except OSError as e:
                raise Fatal("tools source %s: %s (nothing written)" % (src, e.strerror or e)) from e
            files.append((os.path.join(tools_dir, dst), label, data, mode))
        staged = [(os.path.relpath(p_, tools_dir), d_, m_) for p_, _, d_, m_ in files]
        if not specs:
            msg = ("no [[smoke]] in the meta: the archived analysis was not test-run against a cell "
                   "(declare one: see SMOKE in the usage header)")
            c.warn.append(("s", msg))
    except Fatal as e:
        print("archive-campaign: error: %s" % e, file=sys.stderr)
        return 1
    for cat, msg in c.warn:
        print("WARNING (%s) %s" % (cat, msg), file=sys.stderr)
    print("archive-campaign: %s: %d dirs (%d cells), %s; Tier A %s (%d files), Tier B %s (%d files); outside %s; "
          "tools %d files%s" % (a.camp, len(r["sizes"]), len(c.cells), h(sum(r["sizes"].values())), h(r["sa"]),
                                len(ta), h(r["sb"]), len(tb), h(r["tsize"]), len(plan) + (1 if md5 else 0),
                                "; %d warnings" % len(c.warn) if c.warn else ""))
    ok, total, errs = fence_check(new[2])
    if errs:
        print("archive-campaign: fence check %d/%d OK, FAILED on DELETABLE.md after the append; nothing written:"
              % (ok, total), file=sys.stderr)
        for e in errs:
            print("  " + e, file=sys.stderr)
        return 4
    print("fence check %d/%d OK" % (ok, total))
    if specs:
        ok, total, errs = run_smoke(c, specs, staged)
        if errs:
            print("archive-campaign: smoke check %d/%d OK, FAILED: the archived analysis does not reproduce its archived "
                  "output from the tools copy; nothing written:" % (ok, total), file=sys.stderr)
            for e in errs:
                print("  " + e, file=sys.stderr)
            return 5
        print("smoke check %d/%d OK" % (ok, total))
    if a.strict and c.warn:
        print("archive-campaign: --strict and %d warnings: nothing written" % len(c.warn), file=sys.stderr)
        return 2
    if a.dry_run:
        for label, old, nw in zip(labels, texts, new, strict=True):
            sys.stdout.write(udiff(old, nw, label))
        for dst, label, data, _ in files:
            text = data.decode(errors="replace")
            old = read(dst) if os.path.isfile(dst) else None
            if old != text:
                sys.stdout.write(udiff(old, text, label, new_file=old is None))
        print("(dry run: nothing written)")
        return 0
    # tools first, the four docs last (each file via a temp file + rename)
    for dst, _, data, mode in files:
        write_atomic(dst, data, mode)
    for p, t in zip(paths, new, strict=True):
        write_atomic(p, t)
    if a.learn_fields and unknown:
        cur = {l.strip() for l in read(a.fields_file).splitlines() if l.strip()}
        write_atomic(a.fields_file, "\n".join(sorted(cur | set(unknown))) + "\n")
        print("archive-campaign: added %d fields to %s" % (len(unknown), a.fields_file))
    print("archive-campaign: wrote %s and %s/ (%d files)" % (", ".join(labels), rel_tools, len(files)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
