#!/usr/bin/env python3
"""test_archive_campaign.py: tests for archive-campaign.py (stdlib unittest, no pytest needed).

Usage: python3 -I docs/perf/bench-archive/test_archive_campaign.py [-v]

* Fixture tests build a tiny fake campaign `ozarchy-tc` (4 cells, run / tools / build / stage dirs, a target dir
  and a worktree outside the tree) plus minimal INDEX.md / cells.md / DELETABLE.md / results doc / plan doc in a
  temp dir, run the script on them and check the generated rows, rm lists, sizes, tools tree, idempotency,
  --dry-run (unified diffs, nothing written), --strict, the refusals (campaign / section / plan row already in the
  archive or the summary table; DELETABLE.md fence or `bash -n` failure) and each check (a)-(d).
* Smoke tests: the meta's [[smoke]] analyst commands run from the staged tools copy against the fixture cells; a
  match passes, a mismatch / crash (the p3cloidprof prefix bug) / missing script or output / timeout refuses with
  exit 5 and writes nothing (also in --dry-run); no declaration warns (s), --strict refuses.
* GoldenP3cloid re-archives campaign ozarchy-p3cloid from the live data in /home/oz/bench-results-matched onto
  the archive files of 6b643edb^ with meta/p3cloid.toml and compares byte for byte with what 6b643edb committed
  (INDEX.md, cells.md, DELETABLE.md, tools/ozarchy-p3cloid-tools/ incl. file modes) and runs the meta's smoke check
  on the live cells. Skipped when that data or the
  git history is not there. It is a snapshot test: it stays valid while the p3cloid raw data on ozarchy is intact.
"""

import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "archive-campaign.py")
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
BENCH = "/home/oz/bench-results-matched"


def load_module():
    spec = importlib.util.spec_from_file_location("archive_campaign", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def du(path):
    return int(
        subprocess.check_output(["du", "-sb", "--apparent-size", path]).split()[0]
    )


def h(n):
    if n >= 1 << 30:
        return "%.2f GiB" % (n / (1 << 30))
    if n >= 1 << 20:
        return "%.1f MiB" % (n / (1 << 20))
    return "%d KiB" % round(n / 1024)


def write(path, data, mode=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb" if isinstance(data, bytes) else "w") as f:
        f.write(data)
    if mode is not None:
        os.chmod(path, mode)


INDEX_MD = """# Bench results archive index

Made 2026-10-07.

**Updated 2026-10-12 (old).** After `ozarchy-old` finished its 2 dirs were added.

* **Scope.** test scope.

| dir | size | date | commit(s) / branch | what it measured | recorded in | st | del A | del B |
|---|---|---|---|---|---|---|---|---|
| `ozarchy-aaa-run` | 1 KiB | 2026-10-01 | x | y | `docs/r.md` section '3. A thing (campaign ozarchy-aaa, 2026-10-01)' | R | - | - |
| `ozarchy-zzz-run` | 1 KiB | 2026-10-02 | x | y | `docs/r.md` section '4. Z thing (campaign ozarchy-zzz, 2026-10-02)' | R | - | - |
| `presuite-1` | 1 KiB | 2026-10-02 | x | y | - | R | - | - |

Trailing text.
"""

CELLS_HDR = (
    "| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | "
    "matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | "
    "liveness | validity |"
)
CELLS_SEP = "|---" * 20 + "|"

CELLS_MD = """# Per-cell results

Intro.

## old: Old campaign

%s
%s
| `ozarchy-old-300m-r1` | OK | x |

## action-results: Per-action results

%s
%s
| `action-r1` | OK | x |
""" % (CELLS_HDR, CELLS_SEP, CELLS_HDR, CELLS_SEP)

DELETABLE_MD = """# Deletable raw files

Intro.

### `old` (A 0 KiB, B 1 KiB; dirs: `ozarchy-old-300m-r1`)

Campaign ozarchy-old (results doc section 4, plan row 8).

Tier B, 1 KiB, 1 files:

```
rm -- \\
  '/nowhere/ozarchy-old-300m-r1/val0.log.gz'
```
"""

RESULTS_MD = """# Results

## 3. A thing (campaign ozarchy-aaa, 2026-10-01)

text

## 4. Z thing (campaign ozarchy-zzz, 2026-10-02)

## 7. Test campaign tc (campaign ozarchy-tc, 2026-10-13)

### 7.1 Sub

text
"""

PLAN_MD = """# Plan

| # | item |
|---|---|
| 8 | old campaign, results doc section 4 |
| 9 | tc campaign, results doc section 7 |
"""

SUMMARY_HDR = (
    "| Sec | arm (sha, allocator) | matched/s: whole cell [min 0-2 / 6-10] | native blk/s | blk time p99 ms | "
    "node CPU-s/1M | RSS mean / peak GB per node | dev writes MB/s/val | settle ms/native blk | "
    "key delta (CI), as in the write-up |"
)
SUMMARY_MD = (
    """# summary

Sources: x.

%s
|---|---|---|---|---|---|---|---|---|---|
| 4 SAT 120 s | old | 1 | 2 | 3 | 4 | 5 / 6 | 7 | n/a | |

Definitions and caveats

- **A:** a.
- **Commit tail everywhere** x.
"""
    % SUMMARY_HDR
)

ARMS_TXT = """cells per arm: {'a': ['r1-a'], 'b': ['r1-b']}

per arm mean (sd):
metric                          a                  b
ms            188654.275 (4312.179)  166551.175 (3574.323)
blk                3.409 ( 0.101)       3.171 ( 0.163)
ci99            1250.611 (49.545)    1402.059 (101.394)
cpu               26.432 ( 0.237)      28.768 ( 0.447)
rss_pk             4.246 ( 0.082)       4.565 ( 0.080)
rss_mean           3.619 ( 0.050)       3.859 ( 0.044)
layer_mb                       nan                   nan
ck_settle          0.000 ( 0.000)       2.450 ( 0.100)
wr               145.068 ( 4.582)     147.729 ( 4.888)

b vs a  (delta, % [95% CI])
  ms           188654.275 -> 166551.175  delta -22103.100   -11.72% [ -15.35,   -8.08]
"""

PAIRED_TXT = """paired rounds: ['r1']  (b - a); CI = mean +- t(n-1) * sd / sqrt(n)
metric            a mean    b mean      delta                 95% CI        %                 % CI  per-round deltas
ms             188654.275   166551.175 -22103.100 [-26090.885, -18115.315]  -11.72% [ -13.83,   -9.60]  -18945.400
cpu                26.432       28.768     +2.337 [   +1.947,    +2.726]   +8.84% [  +7.37,  +10.31]  +2.071
exec_p99          720.427      856.395   +135.968 [  +67.340,  +204.597]  +18.87% [  +9.35,  +28.40]  +190.920
"""

# The fixture's analyst a-cells.py: PREFIX + tag paths like the real ones (the p3cloidprof prefix bug needs the "-").
A_CELLS_PY = """import json, sys
P = sys.argv[1]
for ta in sys.argv[2:]:
    tag, arm = ta.split(":")
    s = json.load(open(P + tag + "/summary.json"))
    print(tag, arm, "ms=%.1f" % s["headline"]["matched_s_avg"])
"""
CELLS_TXT = "w0 a ms=1000.0\nr1-a a ms=1000.0\nr1-b b ms=1000.0\n"
# the fixture meta's smoke declaration: one cell through the archived a-cells.py, compared with its line of cells.txt
SMOKE = {
    "script": "analyst/a-cells.py",
    "args": ["{bench}/ozarchy-tc-300m-", "r1-b:b"],
    "compare": [{"want": "analyst/cells.txt", "select": "^r1-b "}],
}


# cell name, generated_at, campaign.log start, duration
CELLS = [
    ("ozarchy-tc-smoke-300m-s0-b", "2026-10-13T10:05:40", "10:03:30", 60),
    ("ozarchy-tc-300m-w0", "2026-10-13T10:10:00", "10:07:10", 60),
    ("ozarchy-tc-300m-r1-a", "2026-10-13T10:15:00", "10:12:00", 120),
    ("ozarchy-tc-300m-r1-b", "2026-10-13T10:20:30", "10:17:00", 120),
]
TIER_B_SIZES = {
    "val0.log.gz": 1000,
    "buckets.csv": 200,
    "tasks.txt": 30,
    "sampler.csv": 500,
}


def summary(name, gen, dur, extra=None):
    s = {
        "status": "OK",
        "label": name,
        "generated_at": gen,
        "worktree": "/home/x/wt/h-1234",
        "commit": "1234abcd" + "0" * 32,
        "binaries": {
            "torus_node_md5": "aaaa1111" + "0" * 24,
            "bench_throughput_md5": "bbbb2222" + "0" * 24,
        },
        "cell": {"markets": 300, "duration_s": dur, "block_cap": 400},
        "headline": {
            "matched_s_avg": 1000.0,
            "matched_s_first120": 990.04,
            "matched_s_best60": 1100.5,
            "placed_s_avg": 1500.25,
            "blk_s_avg": 3.46,
            "txs_per_block_avg": 200.0,
            "chain_ms": 250.123,
            "engine_ms_per_1k_fills": 4.0,
            "agreement_verdict": "AGREE",
        },
        "funnel_by_node": {"val0": {"samples": 3}, "val1": {"samples": 3}},
        "liveness": {"verdict": "PASS"},
        "validity": {"verdict": "ACCEPT"},
    }
    if extra:
        for k, v in extra.items():
            s["headline"][k] = v
    return s


class Fixture:
    """A fake campaign `tc` in a temp dir: bench root, archive dir, repo dir, meta, fields file."""

    def __init__(self, tmp):
        self.tmp = tmp
        self.bench = os.path.join(tmp, "bench")
        self.archive = os.path.join(tmp, "archive")
        self.repo = os.path.join(tmp, "repo")
        self.target = os.path.join(tmp, "target-tc")
        self.worktree = os.path.join(tmp, "wt-tc")
        b = self.bench
        for name, gen, start, dur in CELLS:
            d = os.path.join(b, name)
            write(os.path.join(d, "summary.json"), json.dumps(summary(name, gen, dur)))
            write(
                os.path.join(d, "node-environ-trie.txt"),
                "pid=1 exe_md5=aaaa1111" + "0" * 24 + " env=x\n",
            )
            write(os.path.join(d, "run.log"), "run\n")
            for f, n in TIER_B_SIZES.items():
                write(os.path.join(d, f), b"x" * n)
            write(
                os.path.join(b, name + ".campaign.log"),
                "[%s] cell=%s\n[%s] end\n" % (start, name, start),
            )
        write(os.path.join(b, "ozarchy-tc-300m-r1-a", "perf.data"), b"p" * 4096)
        an = os.path.join(b, "ozarchy-tc-run", "analyst")
        write(os.path.join(b, "ozarchy-tc-run", "campaign.log"), "[10:07:00] start\n")
        write(os.path.join(an, "a-cells.py"), A_CELLS_PY)
        write(os.path.join(an, "cells.txt"), CELLS_TXT)
        write(
            os.path.join(an, "cells.json"),
            json.dumps(
                {"w0": {"arm": "a"}, "r1-a": {"arm": "a"}, "r1-b": {"arm": "b"}}
            ),
        )
        write(os.path.join(an, "archive-index-tc.txt"), "| row |\n")
        write(os.path.join(an, "a-arms.py"), 'print("arms")\n')
        write(os.path.join(an, "a-paired.py"), 'print("paired")\n')
        write(os.path.join(an, "arms.txt"), ARMS_TXT)
        write(os.path.join(an, "paired.txt"), PAIRED_TXT)
        write(
            os.path.join(b, "ozarchy-tc-tools", "ozarchy-tc-campaign.sh"),
            "#!/bin/sh\necho tc\n",
            0o755,
        )
        self.md5s = (
            "n 1234abcd%s aaaa1111%s 0 (built 2026-10-13 09:00:00)\n"
            "b 1234abcd%s bbbb2222%s 1 (built 2026-10-13 09:00:01)\n"
        ) % ("0" * 32, "0" * 24, "0" * 32, "0" * 24)
        write(os.path.join(b, "ozarchy-tc-build", "md5s.txt"), self.md5s)
        write(
            os.path.join(b, "ozarchy-tc-stage", "n", "release", "torus-node"),
            b"n" * 2048,
        )
        write(
            os.path.join(b, "ozarchy-tc-stage", "n", "release", "bench-throughput"),
            b"b" * 1024,
        )
        write(
            os.path.join(b, "ozarchy-tcx-300m-r1-a", "summary.json"), "{}"
        )  # other campaign, same prefix
        write(os.path.join(self.target, "release", "big"), b"t" * 3000)
        os.makedirs(self.worktree)
        write(os.path.join(self.archive, "INDEX.md"), INDEX_MD)
        write(os.path.join(self.archive, "cells.md"), CELLS_MD)
        write(os.path.join(self.archive, "DELETABLE.md"), DELETABLE_MD)
        os.makedirs(os.path.join(self.archive, "tools"))
        write(os.path.join(self.repo, "docs", "r.md"), RESULTS_MD)
        write(os.path.join(self.repo, "docs", "plan.md"), PLAN_MD)
        self.summary = os.path.join(self.repo, "docs", "summary.md")
        write(self.summary, SUMMARY_MD)
        self.meta = {
            "smoke": [json.loads(json.dumps(SMOKE))],
            "summary": {
                "file": "docs/summary.md",
                "rows": [
                    {
                        "label": "7 SAT 120 s",
                        "arm": "a",
                        "text": "a text",
                        "delta": "b: matched/s {paired:ms:1}; CPU {paired:cpu:2}",
                    },
                    {"label": "7 SAT 120 s", "arm": "b", "text": "b text"},
                ],
                "note": "- **7 (tc):** test note,\n  second line.",
                "note_before": "- **Commit tail everywhere**",
            },
            "camp": "tc",
            "description": "test A/B",
            "section": "7",
            "plan_row": 9,
            "results_doc": "docs/r.md",
            "plan_doc": "docs/plan.md",
            "env_base": "base env",
            "arms": {
                "a": {"meaning": "arm A build", "env": "A=1"},
                "b": {"meaning": "arm B build", "env": "B=1"},
            },
            "dirs": {
                "run": "run dir prose",
                "tools": "tools prose",
                "build": "build prose",
                "stage": "stage prose",
            },
            "index": {"update": "Update prose."},
            "cells": {"heading": "tc heading (section 7)"},
            "deletable": {
                "note": "Campaign ozarchy-{camp} (results doc section {section}, plan row {plan_row}); keep `summary.json`.",
                "tier_a_keep": "keep until X",
                "kept": ["summary.json", "run.log"],
                "outside": {
                    "note": "the target dir, {target_size} apparent",
                    "target_dirs": [self.target],
                    "worktrees": [self.worktree],
                    "repo": "/repo/main",
                },
            },
        }
        self.fields = os.path.join(tmp, "summary-fields.txt")
        mod = load_module()
        known = sorted(mod.summary_fields(summary("x", "2026-10-13T10:00:00", 60)))
        write(self.fields, "\n".join(known) + "\n")

    def write_meta(self, meta=None, name="tc.json"):
        p = os.path.join(self.tmp, name)
        write(p, json.dumps(meta or self.meta, indent=1))
        return p

    def run(self, *extra, meta=None, cwd=None):
        cmd = [
            sys.executable,
            "-I",
            SCRIPT,
            "tc",
            "--meta",
            self.write_meta(meta),
            "--bench-root",
            self.bench,
            "--archive-dir",
            self.archive,
            "--repo",
            self.repo,
            "--fields-file",
            self.fields,
        ] + list(extra)
        return subprocess.run(cmd, capture_output=True, text=True, cwd=cwd)

    def read(self, name):
        with open(os.path.join(self.archive, name)) as f:
            return f.read()

    def snapshot(self):
        with open(self.summary, "rb") as fh:
            out = {"<summary>": fh.read()}
        for root, _, files in os.walk(self.archive):
            for f in files:
                p = os.path.join(root, f)
                with open(p, "rb") as fh:
                    out[os.path.relpath(p, self.archive)] = fh.read()
        return out


class FixtureCase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.fx = Fixture(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def assertOk(self, r):
        self.assertEqual(
            r.returncode,
            0,
            "rc=%d\nstdout:\n%s\nstderr:\n%s" % (r.returncode, r.stdout, r.stderr),
        )


class EndToEnd(FixtureCase):
    def test_full_archive(self):
        fx = self.fx
        r = fx.run()
        self.assertOk(r)
        self.assertNotIn("WARNING", r.stderr + r.stdout)
        cells = [c[0] for c in CELLS]
        dirs = cells + [
            "ozarchy-tc-run",
            "ozarchy-tc-tools",
            "ozarchy-tc-build",
            "ozarchy-tc-stage",
        ]
        sizes = {d: du(os.path.join(fx.bench, d)) for d in dirs}

        # INDEX.md: Updated paragraph after the last one, rows in sorted position.
        idx = fx.read("INDEX.md").split("\n")
        i = idx.index(
            "**Updated 2026-10-12 (old).** After `ozarchy-old` finished its 2 dirs were added."
        )
        self.assertEqual(idx[i + 1], "")
        para = idx[i + 2]
        self.assertEqual(idx[i + 3], "")
        self.assertTrue(idx[i + 4].startswith("* **Scope.**"))
        self.assertEqual(
            para,
            "**Updated 2026-10-13 (tc).** After `ozarchy-tc` finished (smoke 10:03-10:05, cells 10:07-10:20) its 8 dirs "
            "were added (%s: 2 throughput cells, the warm-up `w0`, the 60 s b smoke cell `ozarchy-tc-smoke-300m-s0-b`, "
            "`ozarchy-tc-run`, `-tools`, `-build`, `-stage`; section 7 of `docs/r.md`). Update prose. The cells are in "
            "`cells.md`, the deletable files in `DELETABLE.md`."
            % h(sum(sizes.values())),
        )
        rows = [l for l in idx if l.startswith("| `")]
        names = [l.split(" | ")[0][3:-1] for l in rows]
        self.assertEqual(
            names,
            ["ozarchy-aaa-run"] + sorted(dirs) + ["ozarchy-zzz-run", "presuite-1"],
        )
        sec = "`docs/r.md` section '7. Test campaign tc (campaign ozarchy-tc, 2026-10-13)'"
        tb = sum(TIER_B_SIZES.values())
        self.assertIn(
            "| `ozarchy-tc-300m-r1-a` | %s | 2026-10-13 | h-1234 @ 1234abcd (node aaaa1111) | test A/B, tw, Classic, "
            "standard shape at 300 markets, shared NVMe, commit tail; arm a (arm A build); throughput 120 s | %s | R | "
            "4 KiB | %s |" % (h(sizes["ozarchy-tc-300m-r1-a"]), sec, h(tb)),
            rows,
        )
        self.assertIn(
            "| `ozarchy-tc-300m-w0` | %s | 2026-10-13 | h-1234 @ 1234abcd (node aaaa1111) | test A/B, tw, Classic, "
            "standard shape at 300 markets, shared NVMe, commit tail; arm a (arm A build); warm-up 60 s, excluded | %s | "
            "R | - | %s |" % (h(sizes["ozarchy-tc-300m-w0"]), sec, h(tb)),
            rows,
        )
        self.assertIn(
            "| `ozarchy-tc-smoke-300m-s0-b` | %s | 2026-10-13 | h-1234 @ 1234abcd (node aaaa1111) | test A/B, tw, "
            "Classic, standard shape at 300 markets, shared NVMe, commit tail; arm b (arm B build); smoke 60 s, excluded "
            "| %s | R | - | %s |"
            % (h(sizes["ozarchy-tc-smoke-300m-s0-b"]), sec, h(tb)),
            rows,
        )
        self.assertIn(
            "| `ozarchy-tc-stage` | %s | 2026-10-13 | 1234abcd (node aaaa1111), bench 1234abcd (bbbb2222) | stage prose "
            "| %s | R | 3 KiB | - |" % (h(sizes["ozarchy-tc-stage"]), sec),
            rows,
        )
        self.assertIn(
            "| `ozarchy-tc-run` | %s | 2026-10-13 | 1234abcd (node aaaa1111), bench 1234abcd (bbbb2222) | run dir prose "
            "| %s | R | - | - |" % (h(sizes["ozarchy-tc-run"]), sec),
            rows,
        )
        self.assertNotIn("ozarchy-tcx", fx.read("INDEX.md"))

        # cells.md: section before action-results, rows sorted by name.
        cm = fx.read("cells.md").split("\n")
        j = cm.index("## tc: tc heading (section 7)")
        self.assertEqual(cm[j + 1 : j + 4], ["", CELLS_HDR, CELLS_SEP])
        crow = cm[j + 4 : j + 8]
        self.assertEqual([l.split(" | ")[0][3:-1] for l in crow], sorted(cells))
        self.assertEqual(cm[j + 8], "")
        self.assertTrue(cm[j + 9].startswith("## action-results:"))
        self.assertEqual(
            crow[0],
            "| `ozarchy-tc-300m-r1-a` | OK | 2026-10-13 10:15 | h-1234 @ 1234abcd | aaaa1111 | 300 | 120 | 400 | base env; "
            "A=1 | 1,000.0 | 990.0 | 1,100.5 | 1,500.2 | 3.5 | 200.0 | 250.12 | 4.00 | AGREE | PASS | ACCEPT |",
        )
        self.assertTrue(crow[2].split(" | ")[8].endswith("A=1; warm-up"))
        self.assertTrue(crow[3].split(" | ")[8].endswith("B=1; smoke"))

        # DELETABLE.md: block appended, run order, Tier A / B / outside, fences closed.
        dl = fx.read("DELETABLE.md")
        self.assertTrue(dl.startswith(DELETABLE_MD))
        block = dl[len(DELETABLE_MD) :]
        order = [
            "ozarchy-tc-smoke-300m-s0-b",
            "ozarchy-tc-300m-w0",
            "ozarchy-tc-300m-r1-a",
            "ozarchy-tc-300m-r1-b",
        ]
        ta = [
            os.path.join(fx.bench, "ozarchy-tc-300m-r1-a", "perf.data"),
            os.path.join(fx.bench, "ozarchy-tc-stage", "n", "release", "torus-node"),
            os.path.join(
                fx.bench, "ozarchy-tc-stage", "n", "release", "bench-throughput"
            ),
        ]
        tbl = [
            os.path.join(fx.bench, c, f) for c in order for f in sorted(TIER_B_SIZES)
        ]
        sa, sb, st = 4096 + 2048 + 1024, tb * 4, du(fx.target)
        exp = [
            "",
            "### `tc` (A %s, B %s; dirs: %s; outside the tree: 1 target dir, %s)"
            % (h(sa), h(sb), ", ".join("`%s`" % d for d in order + dirs[4:]), h(st)),
            "",
            "Campaign ozarchy-tc (results doc section 7, plan row 9); keep `summary.json`. Also kept: `run.log`.",
            "",
            "Tier A, %s, 3 files (keep until X):" % h(sa),
            "",
            "```",
            "rm -- \\",
        ]
        exp += ["  '%s' \\" % p for p in ta]
        exp[-1] = exp[-1][:-2]
        exp += ["```", "", "Tier B, %s, 16 files:" % h(sb), "", "```", "rm -- \\"] + [
            "  '%s' \\" % p for p in tbl
        ]
        exp[-1] = exp[-1][:-2]
        exp += [
            "```",
            "",
            "Outside the tree: the target dir, %s apparent:" % h(st),
            "",
            "```",
            "rm -r -- '%s'" % fx.target,
            "git -C /repo/main worktree remove %s" % fx.worktree,
            "```",
            "",
        ]
        self.assertEqual(block.split("\n"), exp)

        # tools tree
        t = os.path.join(fx.archive, "tools", "ozarchy-tc-tools")
        got = sorted(
            os.path.relpath(os.path.join(r_, f), t)
            for r_, _, fs in os.walk(t)
            for f in fs
        )
        self.assertEqual(
            got,
            [
                "analyst/a-arms.py",
                "analyst/a-cells.py",
                "analyst/a-paired.py",
                "analyst/arms.txt",
                "analyst/cells.txt",
                "analyst/paired.txt",
                "md5s-all.txt",
                "ozarchy-tc-campaign.sh",
            ],
        )
        # fence check over the whole DELETABLE.md after the append: 1 old + Tier A + Tier B + outside
        self.assertIn("fence check 4/4 OK", r.stdout)

        # summary table: per-arm rows after the last row, note bullet before the anchor bullet
        with open(fx.summary) as f:
            sm = f.read().split("\n")
        k = sm.index("| 4 SAT 120 s | old | 1 | 2 | 3 | 4 | 5 / 6 | 7 | n/a | |")
        self.assertEqual(
            sm[k + 1 : k + 4],
            [
                "| 7 SAT 120 s | a text | 188,654 | 3.41 | 1,251 | 26.43 | 3.62 / 4.25 | 145.1 | n/a | "
                "b: matched/s -11.7% [-13.8, -9.6]; CPU +8.84% [+7.37, +10.31] |",
                "| 7 SAT 120 s | b text | 166,551 | 3.17 | 1,402 | 28.77 | 3.86 / 4.57 | 147.7 | 2.5 | |",
                "",
            ],
        )
        n = sm.index("- **Commit tail everywhere** x.")
        self.assertEqual(
            sm[n - 3 : n], ["- **A:** a.", "- **7 (tc):** test note,", "  second line."]
        )
        with open(os.path.join(t, "md5s-all.txt")) as f:
            self.assertEqual(f.read(), fx.md5s)
        self.assertTrue(
            os.stat(os.path.join(t, "ozarchy-tc-campaign.sh")).st_mode & stat.S_IXUSR
        )
        self.assertFalse(
            os.stat(os.path.join(t, "analyst", "cells.txt")).st_mode & stat.S_IXUSR
        )

    def test_toml_meta(self):
        # Same meta as TOML: identical output to the JSON run.
        fx = self.fx
        m = fx.meta
        toml = [
            'camp = "tc"',
            'description = "test A/B"',
            'section = "7"',
            "plan_row = 9",
            'results_doc = "docs/r.md"',
            'plan_doc = "docs/plan.md"',
            'env_base = "base env"',
            "[arms.a]",
            'meaning = "arm A build"',
            'env = "A=1"',
            "[arms.b]",
            'meaning = "arm B build"',
            'env = "B=1"',
            "[dirs]",
            'run = "run dir prose"',
            'tools = "tools prose"',
            'build = "build prose"',
            'stage = "stage prose"',
            "[index]",
            'update = "Update prose."',
            "[cells]",
            'heading = "tc heading (section 7)"',
            "[deletable]",
            "note = '''%s'''" % m["deletable"]["note"],
            'tier_a_keep = "keep until X"',
            'kept = ["summary.json", "run.log"]',
            "[deletable.outside]",
            'note = "the target dir, {target_size} apparent"',
            'target_dirs = ["%s"]' % fx.target,
            'worktrees = ["%s"]' % fx.worktree,
            'repo = "/repo/main"',
        ]
        toml += [
            "[summary]",
            'file = "docs/summary.md"',
            "note = '''- **7 (tc):** test note,\n  second line.'''",
            'note_before = "- **Commit tail everywhere**"',
            "[[summary.rows]]",
            'label = "7 SAT 120 s"',
            'arm = "a"',
            'text = "a text"',
            'delta = "b: matched/s {paired:ms:1}; CPU {paired:cpu:2}"',
            "[[summary.rows]]",
            'label = "7 SAT 120 s"',
            'arm = "b"',
            'text = "b text"',
            "[[smoke]]",
            'script = "analyst/a-cells.py"',
            'args = ["{bench}/ozarchy-tc-300m-", "r1-b:b"]',
            "[[smoke.compare]]",
            'want = "analyst/cells.txt"',
            'select = "^r1-b "',
        ]
        r1 = fx.run()
        self.assertOk(r1)
        snap_json = fx.snapshot()
        # the same campaign in a second fixture, archived from the TOML meta
        fx2 = Fixture(os.path.join(fx.tmp, "second"))
        body = (
            "\n".join(toml)
            .replace(fx.target, fx2.target)
            .replace(fx.worktree, fx2.worktree)
        )
        p = os.path.join(fx2.tmp, "tc.toml")
        write(p, body + "\n")
        r2 = subprocess.run(
            [
                sys.executable,
                "-I",
                SCRIPT,
                "tc",
                "--meta",
                p,
                "--bench-root",
                fx2.bench,
                "--archive-dir",
                fx2.archive,
                "--repo",
                fx2.repo,
                "--fields-file",
                fx2.fields,
            ],
            capture_output=True,
            text=True,
        )
        self.assertOk(r2)
        snap_toml = {
            k: v.replace(fx2.tmp.encode(), fx.tmp.encode())
            for k, v in fx2.snapshot().items()
        }
        self.assertEqual(snap_toml, snap_json)


class Idempotency(FixtureCase):
    def test_refuses_second_run(self):
        fx = self.fx
        self.assertOk(fx.run())
        snap = fx.snapshot()
        r = fx.run()
        self.assertEqual(r.returncode, 3)
        self.assertIn("already", r.stderr)
        self.assertEqual(fx.snapshot(), snap)

    def test_no_force_option(self):
        # never overwrite an existing entry: there is no --force
        r = self.fx.run("--force")
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(self.fx.snapshot()["INDEX.md"], INDEX_MD.encode())

    def test_dry_run_prints_diffs_writes_nothing(self):
        fx = self.fx
        snap = fx.snapshot()
        r = fx.run("--dry-run")
        self.assertOk(r)
        self.assertEqual(fx.snapshot(), snap)
        for f in ("INDEX.md", "cells.md", "DELETABLE.md", "docs/summary.md"):
            self.assertIn("--- a/%s\n+++ b/%s\n" % (f, f), r.stdout)
        self.assertIn("+### `tc` (A ", r.stdout)
        self.assertIn(
            "--- /dev/null\n+++ b/tools/ozarchy-tc-tools/md5s-all.txt\n", r.stdout
        )
        self.assertIn("+| 7 SAT 120 s | a text |", r.stdout)
        self.assertIn("fence check 4/4 OK", r.stdout)

    def test_refuses_section_in_summary_table(self):
        fx = self.fx
        write(
            fx.summary,
            SUMMARY_MD.replace("| 4 SAT 120 s | old |", "| 7 SAT 120 s | old |"),
        )
        snap = fx.snapshot()
        r = fx.run()
        self.assertEqual(r.returncode, 3)
        self.assertIn("section 7", r.stderr)
        self.assertIn("summary.md", r.stderr)
        self.assertEqual(fx.snapshot(), snap)


class Atomicity(FixtureCase):
    def test_bad_tool_source_writes_nothing_and_retry_works(self):
        fx = self.fx
        m = dict(fx.meta, tools={"extra": [["ozarchy-tc-run/no-such-file.txt", "analyst/x.txt"]]})
        snap = fx.snapshot()
        r = fx.run(meta=m)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("no-such-file.txt", r.stderr)
        self.assertEqual(fx.snapshot(), snap)  # nothing written, not even the docs
        self.assertFalse(os.path.exists(os.path.join(fx.archive, "tools", "ozarchy-tc-tools")))
        self.assertOk(fx.run())  # retry after fixing the meta is not refused

    def test_unreadable_tool_source_writes_nothing(self):
        fx = self.fx
        p = os.path.join(fx.bench, "ozarchy-tc-tools", "ozarchy-tc-campaign.sh")
        os.chmod(p, 0)
        try:
            if os.access(p, os.R_OK):
                self.skipTest("running as root")
            snap = fx.snapshot()
            r = fx.run()
            self.assertEqual(r.returncode, 1, r.stderr)
            self.assertEqual(fx.snapshot(), snap)
        finally:
            os.chmod(p, 0o755)
        self.assertOk(fx.run())


class Fences(FixtureCase):
    def test_unclosed_fence_refused(self):
        fx = self.fx
        # a keep-both merge dropped the closing fence of the last rm block
        write(
            os.path.join(fx.archive, "DELETABLE.md"),
            DELETABLE_MD.rstrip("\n")[: -len("```")],
        )
        snap = fx.snapshot()
        r = fx.run()
        self.assertEqual(r.returncode, 4)
        self.assertIn("fence check", r.stderr)
        self.assertIn("not closed", r.stderr)
        self.assertEqual(fx.snapshot(), snap)

    def test_bash_syntax_error_refused(self):
        fx = self.fx
        write(
            os.path.join(fx.archive, "DELETABLE.md"),
            DELETABLE_MD.replace("val0.log.gz'", "val0.log.gz"),
        )  # unterminated quote
        snap = fx.snapshot()
        r = fx.run()
        self.assertEqual(r.returncode, 4)
        self.assertIn("bash -n", r.stderr)
        self.assertIn("DELETABLE.md:", r.stderr)
        self.assertIn("fence check 3/4", r.stderr)
        self.assertEqual(fx.snapshot(), snap)

    def test_dry_run_reports_fence_failure(self):
        fx = self.fx
        write(
            os.path.join(fx.archive, "DELETABLE.md"),
            DELETABLE_MD.replace("val0.log.gz'", "val0.log.gz"),
        )
        r = fx.run("--dry-run")
        self.assertEqual(r.returncode, 4)

    def test_camp_mismatch_rejected(self):
        fx = self.fx
        m = dict(fx.meta, camp="other")
        r = fx.run(meta=m)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(fx.snapshot()["INDEX.md"], INDEX_MD.encode())


class CheckA(FixtureCase):
    def add_field(self):
        p = os.path.join(self.fx.bench, "ozarchy-tc-300m-r1-b", "summary.json")
        write(
            p,
            json.dumps(
                summary(
                    "ozarchy-tc-300m-r1-b", "2026-10-13T10:20:30", 120, {"brand_new": 1}
                )
            ),
        )

    def test_unknown_field_warned(self):
        self.add_field()
        r = self.fx.run()
        self.assertOk(r)
        self.assertIn("WARNING (a)", r.stderr)
        self.assertIn("headline.brand_new", r.stderr)
        self.assertNotIn(
            "funnel_by_node", r.stderr
        )  # val0/val1 normalised to val*, known

    def test_strict_fails_and_writes_nothing(self):
        self.add_field()
        snap = self.fx.snapshot()
        r = self.fx.run("--strict")
        self.assertEqual(r.returncode, 2)
        self.assertEqual(self.fx.snapshot(), snap)

    def test_silenced_by_meta(self):
        self.add_field()
        m = dict(self.fx.meta, checks={"known_fields": ["headline.brand_*"]})
        r = self.fx.run("--strict", meta=m)
        self.assertOk(r)
        self.assertNotIn("WARNING", r.stderr)

    def test_learn_fields(self):
        self.add_field()
        self.assertOk(self.fx.run("--learn-fields"))
        with open(self.fx.fields) as f:
            self.assertIn("headline.brand_new\n", f.read())


class CheckB(FixtureCase):
    def test_orphan_output(self):
        an = os.path.join(self.fx.bench, "ozarchy-tc-run", "analyst")
        write(os.path.join(an, "orphan.txt"), "x\n")
        # a mention in a description string is not a producer
        write(
            os.path.join(an, "a-archive.py"),
            'EXTRA = "analyst/ (cells.txt, orphan.txt)"\nopen(O + "archive-x.txt", "w")\n',
        )
        # a write in an a-*.py is a producer even when the stem differs
        write(
            os.path.join(an, "a-gen.py"),
            'with open(O + "made.txt", "w") as f:\n    f.write("x")\n',
        )
        write(os.path.join(an, "made.txt"), "x\n")
        r = self.fx.run("--dry-run")
        self.assertOk(r)
        warn = [l for l in r.stderr.splitlines() if l.startswith("WARNING (b)")]
        self.assertEqual(len(warn), 1, r.stderr)
        self.assertIn("orphan.txt", warn[0])
        self.assertNotIn("made.txt", warn[0])
        self.assertNotIn("cells.txt", warn[0])
        m = dict(self.fx.meta, checks={"produced_by": {"orphan.txt": "inline python"}})
        r = self.fx.run("--strict", meta=m)
        self.assertOk(r)


class CheckC(FixtureCase):
    def test_missing_paths_and_kept_conflict(self):
        fx = self.fx
        m = json.loads(json.dumps(fx.meta))
        m["deletable"]["tier_a"] = [
            "ozarchy-tc-stage/n/release/torus-node",
            "ozarchy-tc-stage/gone/torus-node",
        ]
        m["deletable"]["outside"]["worktrees"] = [
            fx.worktree,
            os.path.join(fx.tmp, "wt-gone"),
        ]
        m["deletable"]["kept"] = ["summary.json", "sampler.csv", "no-such-file.txt"]
        r = fx.run(meta=m)
        self.assertOk(r)
        c = [l for l in r.stderr.splitlines() if l.startswith("WARNING (c)")]
        joined = "\n".join(c)
        self.assertIn("ozarchy-tc-stage/gone/torus-node", joined)
        self.assertIn("wt-gone", joined)
        self.assertIn("sampler.csv", joined)
        self.assertIn("no-such-file.txt", joined)
        dl = fx.read("DELETABLE.md")
        self.assertNotIn("gone/torus-node", dl)
        self.assertNotIn("wt-gone", dl)
        self.assertIn("Tier A, 2 KiB, 1 file (keep until X):", dl)
        self.assertIn(
            "rm -- '%s'"
            % os.path.join(fx.bench, "ozarchy-tc-stage", "n", "release", "torus-node"),
            dl,
        )


class CheckD(FixtureCase):
    def test_section_and_row_used(self):
        fx = self.fx
        # another campaign already cites section 7 in INDEX; plan row 9 in DELETABLE
        write(
            os.path.join(fx.archive, "INDEX.md"),
            INDEX_MD.replace("section '4. Z thing", "section '7. Z thing"),
        )
        write(
            os.path.join(fx.archive, "DELETABLE.md"),
            DELETABLE_MD.replace("plan row 8", "plan row 9"),
        )
        snap = fx.snapshot()
        r = fx.run()
        self.assertEqual(
            r.returncode, 3, r.stderr
        )  # refused: never duplicate a section / row
        self.assertEqual(fx.snapshot(), snap)
        self.assertIn("section 7", r.stderr)
        self.assertIn("INDEX.md", r.stderr)
        self.assertIn("plan row 9", r.stderr)
        self.assertIn("DELETABLE.md", r.stderr)

    def test_cells_heading_section_used(self):
        fx = self.fx
        write(
            os.path.join(fx.archive, "cells.md"),
            CELLS_MD.replace(
                "## old: Old campaign", "## old: Old campaign (section 7)"
            ),
        )
        r = fx.run()
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertIn("cells.md", r.stderr)

    def test_unrelated_mentions_not_refused(self):
        fx = self.fx
        # a pointer to a subsection or to another doc's section 7 is not ownership
        write(
            os.path.join(fx.archive, "INDEX.md"),
            INDEX_MD.replace(
                "Trailing text.", "Trailing text, see section 7.1 and plan row 90."
            ),
        )
        self.assertOk(fx.run())

    def test_results_heading_other_campaign_and_plan_row_mismatch(self):
        fx = self.fx
        write(
            os.path.join(fx.repo, "docs", "r.md"),
            RESULTS_MD.replace("campaign ozarchy-tc,", "campaign ozarchy-other,"),
        )
        write(
            os.path.join(fx.repo, "docs", "plan.md"),
            PLAN_MD.replace(
                "tc campaign, results doc section 7", "other, results doc section 4"
            ),
        )
        r = fx.run()
        self.assertOk(r)
        d = "\n".join(l for l in r.stderr.splitlines() if l.startswith("WARNING (d)"))
        self.assertIn("ozarchy-other", d)
        self.assertIn("plan row 9", d)

    def test_section_missing_from_results_doc(self):
        fx = self.fx
        m = dict(fx.meta, section="12")
        r = fx.run(meta=m)
        self.assertNotEqual(
            r.returncode, 0
        )  # no heading and no section_title: cannot render the rows
        self.assertIn("section 12", r.stderr)
        m["section_title"] = "12. Not written yet (campaign ozarchy-tc, 2026-10-13)"
        r = fx.run(meta=m)
        self.assertOk(r)
        self.assertIn("WARNING (d)", r.stderr)
        self.assertIn("section '12. Not written yet", fx.read("INDEX.md"))


FULL_COLS = {
    "matched": "193,398",
    "blk": "2.61",
    "p99": "1,196",
    "cpu": "27.93",
    "rss": "4.67 / 4.91",
    "writes": "231.6",
    "settle": "n/a",
}


class SummaryRows(FixtureCase):
    """[summary] rows: numbers from analyst/arms.txt unless [[summary.rows]] cols gives them."""

    def meta_rows(self, *rows):
        m = json.loads(json.dumps(self.fx.meta))
        m["summary"]["rows"] = list(rows)
        return m

    def summary_lines(self):
        with open(self.fx.summary) as f:
            return f.read().split("\n")

    def test_all_cols_overridden_needs_no_arms_txt(self):
        # a soak row whose numbers all come from another table: arms.txt is not read (it may not exist)
        fx = self.fx
        os.rename(
            os.path.join(fx.bench, "ozarchy-tc-run", "analyst", "arms.txt"),
            os.path.join(fx.tmp, "arms.txt.away"),
        )
        row = {"label": "7 soak 600 s", "arm": "a", "text": "a soak", "cols": FULL_COLS}
        r = fx.run(meta=self.meta_rows(row))
        self.assertOk(r)
        self.assertNotIn("WARNING", r.stderr)
        self.assertIn(
            "| 7 soak 600 s | a soak | 193,398 | 2.61 | 1,196 | 27.93 | 4.67 / 4.91 | 231.6 | n/a | |",
            self.summary_lines(),
        )

    def test_all_cols_overridden_arm_not_in_arms_txt(self):
        row = {"label": "7 soak 600 s", "arm": "soak", "text": "soak", "cols": FULL_COLS}
        self.assertOk(self.fx.run(meta=self.meta_rows(row)))

    def test_missing_arms_txt_still_fatal_when_a_col_is_not_given(self):
        fx = self.fx
        os.rename(
            os.path.join(fx.bench, "ozarchy-tc-run", "analyst", "arms.txt"),
            os.path.join(fx.tmp, "arms.txt.away"),
        )
        cols = dict(FULL_COLS)
        del cols["settle"]
        row = {"label": "7 soak 600 s", "arm": "a", "text": "a soak", "cols": cols}
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_rows(row))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("arms.txt", r.stderr)
        self.assertEqual(fx.snapshot(), snap)

    def test_overridden_col_missing_from_arms_txt_does_not_warn(self):
        # wr is not in arms.txt, but the row gives writes: no "no wr in arms.txt" warning
        fx = self.fx
        p = os.path.join(fx.bench, "ozarchy-tc-run", "analyst", "arms.txt")
        write(p, "\n".join(l for l in ARMS_TXT.split("\n") if not l.startswith("wr ")))
        row = {"label": "7 SAT 120 s", "arm": "a", "text": "a text", "cols": {"writes": "145.1"}}
        r = fx.run(meta=self.meta_rows(row))
        self.assertOk(r)
        self.assertNotIn("WARNING", r.stderr)
        self.assertIn(
            "| 7 SAT 120 s | a text | 188,654 | 3.41 | 1,251 | 26.43 | 3.62 / 4.25 | 145.1 | n/a | |",
            self.summary_lines(),
        )
        # without the override the gap still warns (a fresh fixture: this one is archived now)
        row = {"label": "7 SAT 120 s", "arm": "a", "text": "a text"}
        fx2 = Fixture(os.path.join(fx.tmp, "second"))
        write(
            os.path.join(fx2.bench, "ozarchy-tc-run", "analyst", "arms.txt"),
            "\n".join(l for l in ARMS_TXT.split("\n") if not l.startswith("wr ")),
        )
        m = json.loads(json.dumps(fx2.meta))
        m["summary"]["rows"] = [row]
        r = fx2.run("--dry-run", meta=m)
        self.assertOk(r)
        self.assertIn("WARNING (m) summary row a: no wr in arms.txt", r.stderr)


class Smoke(FixtureCase):
    """The smoke-run check: the archived analysis is run from the staged tools copy against real cells and its output
    is diffed with the archived output; any failure refuses (exit 5) and writes nothing, like the fence check."""

    def an(self, *p):
        return os.path.join(self.fx.bench, "ozarchy-tc-run", "analyst", *p)

    def meta_with(self, *smoke):
        m = json.loads(json.dumps(self.fx.meta))
        m["smoke"] = list(smoke)
        return m

    def assertRefused(self, r, snap, *needles):
        self.assertEqual(
            r.returncode,
            5,
            "rc=%d\nstdout:\n%s\nstderr:\n%s" % (r.returncode, r.stdout, r.stderr),
        )
        self.assertIn("smoke check", r.stderr)
        self.assertIn("nothing written", r.stderr)
        for n in needles:
            self.assertIn(n, r.stderr)
        self.assertEqual(self.fx.snapshot(), snap)
        self.assertFalse(
            os.path.exists(os.path.join(self.fx.archive, "tools", "ozarchy-tc-tools"))
        )

    def test_match_passes_and_writes(self):
        fx = self.fx
        r = fx.run()
        self.assertOk(r)
        self.assertIn("smoke check 1/1 OK", r.stdout)
        self.assertNotIn("WARNING", r.stderr)
        self.assertTrue(
            os.path.isfile(
                os.path.join(
                    fx.archive, "tools", "ozarchy-tc-tools", "analyst", "a-cells.py"
                )
            )
        )
        self.assertIn("## tc: ", fx.read("cells.md"))

    def test_mismatch_refuses_and_writes_nothing(self):
        fx = self.fx
        write(
            self.an("cells.txt"),
            CELLS_TXT.replace("r1-b b ms=1000.0", "r1-b b ms=999.0"),
        )
        snap = fx.snapshot()
        r = fx.run()
        self.assertRefused(
            r, snap, "analyst/cells.txt", "-r1-b b ms=999.0", "+r1-b b ms=1000.0"
        )

    def test_dry_run_runs_the_smoke(self):
        fx = self.fx
        snap = fx.snapshot()
        r = fx.run("--dry-run")
        self.assertOk(r)
        self.assertIn("smoke check 1/1 OK", r.stdout)
        self.assertEqual(fx.snapshot(), snap)
        write(
            self.an("cells.txt"),
            CELLS_TXT.replace("r1-b b ms=1000.0", "r1-b b ms=999.0"),
        )
        r = fx.run("--dry-run")
        self.assertRefused(r, snap)

    def test_crash_refuses_and_writes_nothing(self):
        # the p3cloidprof bug: the cell-path prefix without its trailing "-" -> FileNotFoundError in the analyst script
        fx = self.fx
        bad = dict(SMOKE, args=["{bench}/ozarchy-tc-300m", "r1-b:b"])
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with(bad))
        self.assertRefused(r, snap, "FileNotFoundError", "exit status 1")

    def test_script_not_in_tools_copy_refuses(self):
        fx = self.fx
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with(dict(SMOKE, script="analyst/a-gone.py")))
        self.assertRefused(r, snap, "a-gone.py")

    def test_archived_output_not_in_tools_copy_refuses(self):
        fx = self.fx
        m = self.meta_with(SMOKE)
        m["tools"] = {"exclude": ["cells.txt"]}
        snap = fx.snapshot()
        r = fx.run(meta=m)
        self.assertRefused(r, snap, "analyst/cells.txt")

    def test_select_matching_nothing_refuses(self):
        fx = self.fx
        snap = fx.snapshot()
        cmp_ = [{"want": "analyst/cells.txt", "select": "^r9-x "}]
        r = fx.run(meta=self.meta_with(dict(SMOKE, compare=cmp_)))
        self.assertRefused(r, snap, "^r9-x ")

    def test_missing_declaration_warns_and_strict_refuses(self):
        fx = self.fx
        m = json.loads(json.dumps(fx.meta))
        del m["smoke"]
        snap = fx.snapshot()
        r = fx.run("--strict", meta=m)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("WARNING (s)", r.stderr)
        self.assertEqual(fx.snapshot(), snap)
        r = fx.run(meta=m)
        self.assertOk(r)
        self.assertIn("WARNING (s)", r.stderr)
        self.assertNotIn("smoke check", r.stdout)
        self.assertIn("## tc: ", fx.read("cells.md"))

    def test_timeout_refuses(self):
        fx = self.fx
        write(
            self.an("a-slow.py"),
            "import time\ntime.sleep(60)\nprint('r1-b b ms=1000.0')\n",
        )
        slow = dict(SMOKE, script="analyst/a-slow.py", args=[], timeout=1)
        snap = fx.snapshot()
        t0 = __import__("time").monotonic()
        r = fx.run(meta=self.meta_with(slow))
        self.assertLess(__import__("time").monotonic() - t0, 30)
        self.assertRefused(r, snap, "timed out after 1 s")

    def test_output_file_ignore_and_tolerance(self):
        # compare a file the script writes into {out}; ignore a timestamp line; a declared numeric tolerance
        fx = self.fx
        write(
            self.an("a-res.py"),
            "import sys\nopen(sys.argv[1] + '/res.txt', 'w').write('made 2026-10-14 09:00\\nms=1000.04 n=3\\n')\n",
        )
        write(self.an("res.txt"), "made 2026-10-13 17:00\nms=1000.0 n=3\n")
        cmp_ = {"got": "res.txt", "want": "analyst/res.txt", "ignore": ["^made "]}
        smoke = {"script": "analyst/a-res.py", "args": ["{out}"], "compare": [cmp_]}
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with(smoke))
        self.assertRefused(r, snap, "ms=1000.04")  # exact by default
        r = fx.run(
            meta=self.meta_with(dict(smoke, compare=[dict(cmp_, tolerance=1e-4)]))
        )
        self.assertOk(r)
        self.assertIn("smoke check 1/1 OK", r.stdout)

    def test_steps_share_out_and_run_from_the_staged_copy(self):
        # step 1 writes {out}/x.json, step 2 reads it; scripts run from the staged copy ({tools}), cwd = {out}
        fx = self.fx
        write(
            self.an("a-one.py"),
            "import json\njson.dump({'n': 3}, open('x.json', 'w'))\n",
        )
        write(
            self.an("a-two.py"),
            "import json, os, sys\nprint('n=%d' % json.load(open(sys.argv[1]))['n'])\n"
            "print('staged' if os.path.dirname(os.path.abspath(__file__)) == sys.argv[2] else 'NOT staged')\n",
        )
        write(self.an("two.txt"), "n=3\nstaged\n")
        m = self.meta_with(
            {"script": "analyst/a-one.py", "args": []},
            {
                "script": "analyst/a-two.py",
                "args": ["{out}/x.json", "{analyst}"],
                "compare": [{"want": "analyst/two.txt"}],
            },
            SMOKE,
        )
        r = fx.run(meta=m)
        self.assertOk(r)
        self.assertIn("smoke check 3/3 OK", r.stdout)

    def test_stdout_stderr_is_merged_in_order_like_2_redirect(self):
        # got = "stdout+stderr" is one pipe for both streams (`> f 2>&1`), so the interleaving is kept
        fx = self.fx
        write(
            self.an("a-mix.py"),
            "import sys\nfor i in range(3):\n    print('out', i, flush=True)\n    print('err', i, file=sys.stderr, flush=True)\n",
        )
        write(self.an("mix.txt"), "out 0\nerr 0\nout 1\nerr 1\nout 2\nerr 2\n")
        smoke = {"script": "analyst/a-mix.py", "compare": [{"got": "stdout+stderr", "want": "analyst/mix.txt"}]}
        r = fx.run(meta=self.meta_with(smoke))
        self.assertOk(r)
        self.assertIn("smoke check 1/1 OK", r.stdout)
        # one entry cannot compare both the merged stream and stdout alone: a meta error
        both = dict(smoke, compare=smoke["compare"] + [{"want": "analyst/mix.txt"}])
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with(both))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("stdout+stderr", r.stderr)
        self.assertEqual(fx.snapshot(), snap)

    def test_paths_outside_the_staged_copy_are_refused(self):
        # script / want must name files in the staged tools copy and got a file in {out}: an absolute path or "..".
        # would test (or compare with) something that is not what gets archived
        fx = self.fx
        live = self.an("a-cells.py")  # exists on disk, but is not the staged copy
        cmp_ = SMOKE["compare"][0]
        bad = [
            dict(SMOKE, script=live),
            dict(SMOKE, script="../ozarchy-tc-tools/analyst/a-cells.py"),
            dict(SMOKE, script="analyst/../../x/a-cells.py"),
            dict(SMOKE, compare=[dict(cmp_, want=self.an("cells.txt"))]),
            dict(SMOKE, compare=[dict(cmp_, want="analyst/../../cells.txt")]),
            dict(SMOKE, compare=[dict(cmp_, got=self.an("cells.txt"))]),
            dict(SMOKE, compare=[dict(cmp_, got="{analyst}/cells.txt")]),
            dict(SMOKE, compare=[dict(cmp_, got="../tools/analyst/cells.txt")]),
        ]
        snap = fx.snapshot()
        for b_ in bad:
            r = fx.run(meta=self.meta_with(b_))
            self.assertEqual(r.returncode, 1, "%r\n%s" % (b_, r.stderr))
            self.assertIn("relative path", r.stderr)
            self.assertNotIn("smoke check", r.stdout)
            self.assertEqual(fx.snapshot(), snap)

    def test_analyst_overwriting_the_staged_want_is_refused(self):
        # the want is compared with the bytes that get archived (snapshotted before any analyst runs), so a script
        # that rewrites the staged copy of its own expected output cannot make a mismatch pass
        fx = self.fx
        write(
            self.an("a-cheat.py"),
            "open('../tools/analyst/cells.txt', 'w').write('bad\\n')\nprint('bad')\n",
        )
        smoke = {
            "script": "analyst/a-cheat.py",
            "compare": [{"want": "analyst/cells.txt"}],
        }
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with(smoke))
        self.assertRefused(r, snap, "analyst/cells.txt", "+bad")
        r = fx.run("--dry-run", meta=self.meta_with(smoke))
        self.assertRefused(r, snap, "+bad")

    def test_relative_bench_root_works(self):
        # {bench} is substituted as an absolute path: the analyst commands run with cwd = the scratch dir
        # (run from the temp dir with --bench-root bench: a relative path that does not resolve from the scratch dir)
        fx = self.fx
        fx.bench = "bench"
        snap = fx.snapshot()
        r = fx.run("--dry-run", cwd=fx.tmp)
        self.assertOk(r)
        self.assertIn("smoke check 1/1 OK", r.stdout)
        self.assertEqual(fx.snapshot(), snap)

    def test_bad_tolerance_is_an_error(self):
        # nan would approve every numeric mismatch (any comparison with nan is False); inf likewise; < 0 never
        # matches: all are meta errors (exit 1, nothing written, no smoke run)
        fx = self.fx
        cmp_ = SMOKE["compare"][0]
        snap = fx.snapshot()
        for tol in (float("nan"), float("inf"), -1e-4, "0.1", True):
            r = fx.run(
                meta=self.meta_with(dict(SMOKE, compare=[dict(cmp_, tolerance=tol)]))
            )
            self.assertEqual(r.returncode, 1, "%r\n%s" % (tol, r.stderr))
            self.assertIn("tolerance", r.stderr)
            self.assertNotIn("smoke check", r.stdout)
            self.assertEqual(fx.snapshot(), snap)
        m = load_module()
        for tol in (float("nan"), float("inf")):
            with self.assertRaises(m.Fatal):
                m.smoke_specs(
                    {
                        "smoke": [
                            {
                                "script": "a.py",
                                "compare": [{"want": "w.txt", "tolerance": tol}],
                            }
                        ]
                    }
                )
        m.smoke_specs(
            {
                "smoke": [
                    {"script": "a.py", "compare": [{"want": "w.txt", "tolerance": 0}]}
                ]
            }
        )

    def test_overflowing_numbers_refused_under_tolerance(self):
        # 1e400 and 9e400 both parse to inf: a tolerance must not let two different overflowing values match
        fx = self.fx
        write(self.an("a-big.py"), "print('r1-b b ms=9e400')\n")
        write(self.an("big.txt"), "r1-b b ms=1e400\n")
        cmp_ = {"want": "analyst/big.txt", "tolerance": 1e-4}
        snap = fx.snapshot()
        r = fx.run(
            meta=self.meta_with({"script": "analyst/a-big.py", "compare": [cmp_]})
        )
        self.assertRefused(r, snap, "-r1-b b ms=1e400", "+r1-b b ms=9e400")

    def test_malformed_declaration_is_an_error(self):
        fx = self.fx
        snap = fx.snapshot()
        r = fx.run(meta=self.meta_with({"args": []}))
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("smoke", r.stderr)
        self.assertEqual(fx.snapshot(), snap)


class SmokeSame(unittest.TestCase):
    """smoke_same(a, b, tol): a non-finite number (inf, -inf, nan, or a literal that overflows to inf) only matches
    the identical string; finite numbers keep the relative tolerance."""

    @classmethod
    def setUpClass(cls):
        cls.same = staticmethod(load_module().smoke_same)

    def test_overflow_to_inf_is_a_mismatch(self):
        for tol in (0, 1e-4, 0.5):
            self.assertFalse(self.same("ms=1e400", "ms=9e400", tol), tol)
            self.assertFalse(self.same("ms=-1e400", "ms=-9e400", tol), tol)
            self.assertFalse(self.same("a 1e400 b", "a 2E+400 b", tol), tol)

    def test_overflow_vs_finite_is_a_mismatch(self):
        # inf - 1e308 = inf, inf > tol * inf is False: the old check passed these
        for tol in (1e-4, 0.5):
            self.assertFalse(self.same("ms=1e308", "ms=1e400", tol), tol)
            self.assertFalse(self.same("ms=1e400", "ms=1.7e308", tol), tol)

    def test_overflow_vs_inf_literal_is_a_mismatch(self):
        for tol in (0, 1e-4):
            self.assertFalse(self.same("ms=1e400", "ms=inf", tol), tol)
            self.assertFalse(self.same("ms=inf", "ms=1e400", tol), tol)

    def test_identical_non_finite_strings_match(self):
        for tol in (0, 1e-4):
            self.assertTrue(self.same("ms=inf", "ms=inf", tol), tol)
            self.assertTrue(self.same("ms=-inf", "ms=-inf", tol), tol)
            self.assertTrue(self.same("ms=nan", "ms=nan", tol), tol)
            self.assertTrue(self.same("ms=1e400", "ms=1e400", tol), tol)
        # the identical non-finite part matches while the finite numbers on the line use the tolerance
        self.assertTrue(self.same("x=1e400 y=1000.0", "x=1e400 y=1000.04", 1e-4))
        self.assertTrue(self.same("x=inf y=1000.0", "x=inf y=1000.04", 1e-4))
        self.assertFalse(self.same("x=1e400 y=1000.0", "x=1e400 y=1001.0", 1e-4))

    def test_non_finite_spelled_differently_is_a_mismatch(self):
        self.assertFalse(self.same("ms=inf", "ms=Infinity", 1e-4))
        self.assertFalse(self.same("ms=nan", "ms=NaN", 1e-4))

    def test_finite_tolerance_unchanged(self):
        self.assertTrue(self.same("ms=1000.0 n=3", "ms=1000.0 n=3", 0))
        self.assertFalse(self.same("ms=1000.0", "ms=1000.04", 0))
        self.assertTrue(self.same("ms=1000.0", "ms=1000.04", 1e-4))
        self.assertFalse(self.same("ms=1000.0", "ms=1000.2", 1e-4))
        self.assertTrue(self.same("ms=1.0e300", "ms=1.00001e300", 1e-4))
        self.assertTrue(self.same("ms=0", "ms=0.0", 1e-4))
        self.assertFalse(self.same("ms=1.0 a", "ms=1.0 b", 1e-4))
        self.assertFalse(self.same("ms=1.0", "ms=1.0 2.0", 1e-4))


class Units(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.m = load_module()

    def test_h(self):
        for n in (0, 511, 513, 1536, 280082, 50135, 1 << 20, 1061358832, 6628992253):
            self.assertEqual(self.m.h(n), h(n))

    def test_dir_size_matches_du(self):
        with tempfile.TemporaryDirectory() as t:
            write(os.path.join(t, "a", "b", "c.bin"), b"x" * 12345)
            write(os.path.join(t, "a", "d.txt"), "hello")
            os.link(os.path.join(t, "a", "d.txt"), os.path.join(t, "a", "e.txt"))
            os.symlink("d.txt", os.path.join(t, "a", "s"))
            self.assertEqual(self.m.dir_size(t), du(t))

    def test_summary_fields_normalised(self):
        f = self.m.summary_fields(
            {
                "a": {"val0": {"x": 1}, "val12": {"x": 2}},
                "cpu": {"p": {"torus-node:123": 1.0}},
                "l": [1, 2],
                "e": {},
            }
        )
        self.assertEqual(f, {"a.val*.x", "cpu.p.torus-node:*", "l", "e"})

    def test_never_deletes(self):
        with open(SCRIPT) as f:
            src = f.read()
        for bad in (
            "os.remove",
            "os.unlink",
            "rmtree",
            "os.rmdir",
            ".unlink(",
            "os.removedirs",
            "shell=True",
        ):
            self.assertNotIn(bad, src)


@unittest.skipUnless(
    os.path.isdir(os.path.join(BENCH, "ozarchy-p3cloid-run")),
    "p3cloid raw data not on this host",
)
class GoldenP3cloid(unittest.TestCase):
    SHA = "6b643edb"
    SUMMARY = "docs/perf/ozarchy-summary-2026-10-13.md"

    def git(self, *a, text=True):
        return subprocess.check_output(["git", "-C", REPO] + list(a), text=text)

    def setUp(self):
        try:
            self.git("cat-file", "-e", self.SHA + "^{commit}")
        except subprocess.CalledProcessError:
            self.skipTest("commit %s not in this repo" % self.SHA)
        self._tmp = tempfile.TemporaryDirectory()
        self.arch = os.path.join(self._tmp.name, "archive")
        os.makedirs(os.path.join(self.arch, "tools"))
        for f in ("INDEX.md", "cells.md", "DELETABLE.md"):
            write(
                os.path.join(self.arch, f),
                self.git("show", "%s^:docs/perf/bench-archive/%s" % (self.SHA, f)),
            )
        self.summary = os.path.join(self._tmp.name, "ozarchy-summary-2026-10-13.md")
        write(self.summary, self.git("show", "%s^:%s" % (self.SHA, self.SUMMARY)))
        # the field baseline as it was before p3cloid: drop the fields p3cloid introduced
        self.fields = os.path.join(self._tmp.name, "summary-fields.txt")
        with open(os.path.join(HERE, "summary-fields.txt")) as f:
            write(
                self.fields, "".join(l for l in f if not l.startswith("ingest.cloids."))
            )

    def tearDown(self):
        self._tmp.cleanup()

    def test_reproduces_6b643edb(self):
        r = subprocess.run(
            [
                sys.executable,
                "-I",
                SCRIPT,
                "p3cloid",
                "--meta",
                os.path.join(HERE, "meta", "p3cloid.toml"),
                "--archive-dir",
                self.arch,
                "--repo",
                REPO,
                "--fields-file",
                self.fields,
                "--summary-file",
                self.summary,
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("fence check 119/119 OK", r.stdout)
        # meta/p3cloid.toml's [[smoke]]: the archived a-cells / a-arms / a-paired reproduce cells.txt, arms.txt, paired.txt
        self.assertIn("smoke check 4/4 OK", r.stdout)
        # summary table: rows and note bullet are generated; the "Sources:" sentence (lines 4-5) is hand-edited
        # prose (it names the branch of the write-up), the one documented diff
        exp = self.git("show", "%s:%s" % (self.SHA, self.SUMMARY)).split("\n")
        with open(self.summary) as fh:
            got = fh.read().split("\n")
        self.assertEqual(len(got), len(exp))
        diff = [i + 1 for i, (a, b) in enumerate(zip(got, exp, strict=True)) if a != b]
        self.assertEqual(
            diff,
            [4, 5],
            "\n".join("%d: %s\n   %s" % (i, got[i - 1], exp[i - 1]) for i in diff),
        )
        for f in ("INDEX.md", "cells.md", "DELETABLE.md"):
            exp = self.git("show", "%s:docs/perf/bench-archive/%s" % (self.SHA, f))
            with open(os.path.join(self.arch, f)) as fh:
                got = fh.read()
            if got != exp:
                import difflib

                d = "".join(
                    list(
                        difflib.unified_diff(
                            exp.splitlines(True),
                            got.splitlines(True),
                            "committed",
                            "generated",
                        )
                    )[:80]
                )
                self.fail("%s differs from %s:\n%s" % (f, self.SHA, d))
        # tools tree: same files, bytes and modes as committed
        pre = "docs/perf/bench-archive/tools/ozarchy-p3cloid-tools/"
        tree = {}
        for line in self.git("ls-tree", "-r", self.SHA, pre).splitlines():
            mode, _, _, path = line.split(None, 3)
            tree[path[len(pre) :]] = mode
        t = os.path.join(self.arch, "tools", "ozarchy-p3cloid-tools")
        got = {
            os.path.relpath(os.path.join(a, f), t)
            for a, _, fs in os.walk(t)
            for f in fs
        }
        self.assertEqual(got, set(tree))
        for rel, mode in tree.items():
            p = os.path.join(t, rel)
            with open(p, "rb") as fh:
                self.assertEqual(
                    fh.read(),
                    self.git("show", "%s:%s%s" % (self.SHA, pre, rel), text=False),
                    rel,
                )
            self.assertEqual(
                oct(os.stat(p).st_mode & 0o777), oct(int(mode, 8) & 0o777), rel
            )
        # the checks fire exactly where the p3cloid write-up needed hand work
        a = [l for l in r.stderr.splitlines() if l.startswith("WARNING (a)")]
        b = [l for l in r.stderr.splitlines() if l.startswith("WARNING (b)")]
        self.assertEqual(len(a), 1, r.stderr)
        for fld in (
            "ingest.cloids.placed",
            "ingest.cloids.share_pct",
            "ingest.cloids.stamped",
        ):
            self.assertIn(fld, a[0])
        self.assertEqual(len(b), 1, r.stderr)
        self.assertIn("phases.txt", b[0])
        self.assertIn("cloids.txt", b[0])
        for other in ("WARNING (c)", "WARNING (d)", "WARNING (x)", "WARNING (s)"):
            self.assertNotIn(other, r.stderr)


if __name__ == "__main__":
    unittest.main()
