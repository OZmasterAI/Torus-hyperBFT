#!/usr/bin/env python3
"""test_genesis_reuse_lines.py — a reused-genesis cell's run.log carries every genesis line a built cell's does.

    python3 tools/matched-bench/test_genesis_reuse_lines.py

Before genesis reuse (0f2ca85b) every cell ran devnet/wsl/gen-3val-genesis.sh, and campaign checks and spec
expectations read its run.log output: the "generating N-market 3-val genesis -> G" line, the generator's
"reusing existing ..." / "wrote G" lines and its summary ("  state_hash_activation = 1", "  markets    = N", ...).
A reused cell must print the same lines in the same form (ozarchy 8p: a spec expectation on the
state_hash_activation line stopped the campaign at its first reused cell).

run-cell.sh runs for real in a sandbox (test_genesis_cache.RunCellReuseTest's, up to the launch) with the REAL
generator (a wrapper counts its calls) over a small weighted-full genesis, for several genesis configurations
(default, STATE_HASH_ACTIVATION, EXTRA_ENV TIMEOUT_BASE_MS / EVM_BLOCK_INTERVAL, LIQ_THIN, FORCE=1). For each:
the pre-0f2ca85b run-cell.sh (from git; skipped without the object), a building cell and a reusing cell. Pinned:
the genesis section of run.log (after the binaries' md5 lines up to "genesis markets=...", timestamps stripped,
the cache's own lines dropped) is the same in all three; every run.log expectation of the archived and testdata
campaign specs (plus ozarchy 8p's) and every genesis check of the archived campaign scripts gives the same
result on the reused cell as on the built one; the archived scripts hold no genesis check this file does not run.
"""

import glob
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
PRE_REUSE = "0f2ca85b^"  # the last run-cell.sh without genesis reuse


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


TGC = load("test_genesis_cache", os.path.join(HERE, "test_genesis_cache.py"))

GEN_WRAPPER = """#!/bin/bash
# counts the calls, then runs the real generator (copied beside it)
echo call >> "$GEN_CALLS"
exec "$(dirname "$0")/gen-3val-genesis.real.sh" "$@"
"""
# bench-throughput: gen-accounts for gen-weighted-genesis.sh (a FORCE=1 cell rebuilds the weighted-full genesis)
FAKE_BENCH = """#!/bin/bash
if [ "$1" = gen-accounts ]; then
    shift; off=0; n=0
    while [ $# -gt 0 ]; do case $1 in --offset) off=$2; shift 2 ;; --count) n=$2; shift 2 ;; *) shift ;; esac; done
    for ((i = off; i < off + n; i++)); do printf '%d 0x%040x\\n' "$i" $((0xc0000 + i)); done
    exit 0
fi
echo fake
"""
FAKE_SUMMARIZE = "import sys\nprint('usage: summarize.py [--liq-thin N] [--liq-thin-avail A]')\n"
FAKE_DIGEST = """#!/bin/sh
[ "$1" = --capabilities ] && { echo position-accounts; exit 0; }
exit 0
"""
BULK = 5
# (name, cell env): each a genesis configuration; FORCE=1 last (it rewrites the weighted-full genesis)
CONFIGS = (
    ("default", {}),
    ("state-hash", {"STATE_HASH_ACTIVATION": "1"}),
    ("extra-env", {"EXTRA_ENV": "TIMEOUT_BASE_MS=700 EVM_BLOCK_INTERVAL=5"}),
    ("liq-thin", {"LIQ_THIN": "2"}),
    # owner s136: unset = the hash on (written explicitly); 'off' = the pre-s136 genesis
    ("hash-off", {"STATE_HASH_ACTIVATION": "off"}),
    ("force", {"FORCE": "1"}),
)
TS = re.compile(r"^\[\d\d:\d\d:\d\d\] ")
# the genesis cache's own lines (0f2ca85b on), the one difference a reused section may have
CACHE_LINE = re.compile(
    r"^(genesis cache: |WARNING: genesis cache: |genesis reused \(|genesis built \()"
)


def section(log):
    """run.log's genesis section: after the binaries' md5 lines up to "genesis markets=...", no timestamps,
    no cache lines"""
    lines = log.splitlines()
    start = max(i for i, l in enumerate(lines) if "] bench-throughput md5=" in l) + 1
    end = (
        next(i for i, l in enumerate(lines) if i >= start and "] genesis markets=" in l)
        + 1
    )
    out = [TS.sub("", l) for l in lines[start:end]]
    return [l for l in out if not CACHE_LINE.match(l)]


class Sandbox:
    """one run-cell.sh sandbox for the module, every configuration's logs (built in setUpModule)"""

    def __init__(self):
        sb = TGC.RunCellReuseTest("test_no_cache_dir_builds_every_cell")
        sb.setUp()
        self.sb = sb
        try:
            self.setup()
        except BaseException:
            sb.doCleanups()
            raise

    def setup(self):
        sb = self.sb
        repo = sb.repo
        tools = os.path.join(repo, "tools", "matched-bench")
        # a LIQ_THIN cell asks the summarizer and the digest script for their liquidation support up front
        for path, body in (
            (os.path.join(tools, "summarize.py"), FAKE_SUMMARIZE),
            (os.path.join(tools, "digest-node.sh"), FAKE_DIGEST),
        ):
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        wsl = os.path.join(repo, "devnet", "wsl")
        shutil.copy2(
            os.path.join(REPO, "devnet/wsl/gen-3val-genesis.sh"),
            os.path.join(wsl, "gen-3val-genesis.real.sh"),
        )
        for path, body in (
            (os.path.join(wsl, "gen-3val-genesis.sh"), GEN_WRAPPER),
            (os.path.join(sb.tmp, "release", "bench-throughput"), FAKE_BENCH),
        ):
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        # the weighted-full genesis: the real base plus BULK bulk senders (a FORCE=1 cell rebuilds it the same way)
        full = json.load(open(os.path.join(repo, "testnet/genesis-weighted-base.json")))
        full["native_balances"] += [
            {
                "address": "0x%040x" % (0xB0000 + i),
                "available": "1.0",
                "note": "bulk-test %d" % (60 + i),
            }
            for i in range(BULK)
        ]
        with open(os.path.join(repo, "testnet/genesis-weighted-full.json"), "w") as f:
            json.dump(full, f)
        self.genesis = os.path.join(wsl, "genesis-3val.json")
        self.old = None
        r = subprocess.run(
            ["git", "-C", REPO, "show", PRE_REUSE + ":tools/matched-bench/run-cell.sh"],
            capture_output=True,
            text=True,
        )
        if r.returncode == 0:
            d = os.path.join(repo, "tools", "pre-reuse")
            os.makedirs(d)
            self.old = os.path.join(d, "run-cell.sh")
            with open(self.old, "w") as f:
                f.write(r.stdout)
            os.chmod(self.old, 0o755)
            for f in ("digest-node.sh", "summarize.py"):
                shutil.copy2(os.path.join(tools, f), d)
        self.new = sb.script
        self.logs = {}
        for name, env in CONFIGS:
            c = {"env": env}
            if self.old:
                sb.script = self.old
                c["old"] = self.cell(GENESIS_CACHE_DIR="", **env)
                sb.script = self.new
            n0 = sb.calls_n()
            c["build"] = self.cell(**env)
            n1 = sb.calls_n()
            c["reuse"] = self.cell(**env)
            c["calls"] = (n1 - n0, sb.calls_n() - n1)
            c["md5"] = hashlib.md5(open(self.genesis, "rb").read()).hexdigest()
            self.logs[name] = c

    def cell(self, **env):
        """one cell of sb.script; EXTRA_ENV is run-cell.sh's 6th argument (DUR and RATE: its defaults)"""
        sb = self.sb
        script = sb.script
        if "EXTRA_ENV" in env:
            wrapper = os.path.join(sb.tmp, "run-cell-extra.sh")
            with open(wrapper, "w") as f:
                f.write('#!/bin/sh\nexec "$CELL_SCRIPT" "$1" "$2" "$3" 120 76000 "$CELL_EXTRA_ENV"\n')
            os.chmod(wrapper, 0o755)
            env = dict(env, CELL_SCRIPT=script, CELL_EXTRA_ENV=env["EXTRA_ENV"])
            del env["EXTRA_ENV"]
            sb.script = wrapper
        try:
            sb.run_cell(**env)
        finally:
            sb.script = script
        path = os.path.join(sb.tmp, "results", "reuse-%d" % sb.n, "run.log")
        return {"path": path, "text": open(path).read()}

    def close(self):
        self.sb.doCleanups()


SB = None


def setUpModule():
    global SB
    SB = Sandbox()


def tearDownModule():
    if SB:
        SB.close()


class GenesisSectionTest(unittest.TestCase):
    def test_reused_cell_prints_the_built_cells_genesis_lines(self):
        for name, c in SB.logs.items():
            with self.subTest(config=name):
                self.assertEqual(
                    c["calls"], (1, 0), "built once, then reused (generator calls)"
                )
                b, r = c["build"]["text"], c["reuse"]["text"]
                self.assertRegex(
                    b, r"\] genesis built \(key [0-9a-f]{20}, md5 %s[,)]" % c["md5"]
                )
                self.assertRegex(
                    r, r"\] genesis reused \(key [0-9a-f]{20}, md5 %s\)" % c["md5"]
                )
                self.assertEqual(section(r), section(b))

    def test_section_holds_the_generator_lines(self):
        g = SB.genesis
        for name, c in SB.logs.items():
            with self.subTest(config=name):
                sec = section(c["reuse"]["text"])
                want = {
                    "generating 3-market 3-val genesis -> " + g,
                    "wrote " + g,
                    "  state_hash_activation = %s"
                    % ("off" if name == "hash-off" else "1"),
                    "  markets    = 3",
                    "  bulk_count = %d" % BULK,
                }
                if name != "force":
                    full = os.path.join(
                        SB.sb.repo, "testnet", "genesis-weighted-full.json"
                    )
                    want.add("reusing existing %s (BULK_COUNT=%d)" % (full, BULK))
                else:
                    want.add(
                        "building weighted-full genesis (BULK_COUNT=%d bulk senders)..."
                        % BULK
                    )
                self.assertLessEqual(want, set(sec), "\n".join(sec))
                self.assertIn(
                    "genesis timeout_base_ms=%s"
                    % ("700" if name == "extra-env" else "1200"),
                    sec,
                )

    def test_sections_equal_the_pre_reuse_run_cell(self):
        if not SB.old:
            self.skipTest(
                "git object %s:tools/matched-bench/run-cell.sh not available"
                % PRE_REUSE
            )
        for name, c in SB.logs.items():
            with self.subTest(config=name):
                self.assertNotIn(
                    "genesis built",
                    c["old"]["text"],
                    "the pre-reuse script has no cache",
                )
                self.assertEqual(section(c["build"]["text"]), section(c["old"]["text"]))
                self.assertEqual(section(c["reuse"]["text"]), section(c["old"]["text"]))


# ---------------------------------------------------------------- archived campaign expectations and checks

SPEC_GLOBS = (
    "docs/perf/bench-archive/**/*.json",
    "tools/matched-bench/testdata/**/*.json",
    "tools/matched-bench/campaign/*.json",
)
# ozarchy 8p (1489255f tools, 2026-10-19): the expectation that stopped the driver at its first reused cell
ARCHY_8P = {
    "file": "run.log",
    "regex": '^  state_hash_activation = "?off"?$',
    "min": 1,
    "max": None,
    "src": "ozarchy-8p",
}
SCRIPT_GLOBS = (
    "docs/perf/bench-archive/**/*.sh",
    "tools/matched-bench/testdata/**/*.sh",
    "tools/matched-bench/campaign-template.sh",
)
# The genesis checks of the archived campaign scripts on run.log: the text as it stands in the scripts, and the
# check as a shell snippet over L (run.log), G (genesis path), M / N (its markets / native balances), WANT.
SCRIPT_CHECKS = (
    (
        'grep -q -x "  state_hash_activation = $want"',
        'grep -q -x "  state_hash_activation = $WANT" "$L"',
    ),
    ('grep -q "genesis -> $g\\$"', 'grep -q "genesis -> $G\\$" "$L"'),
    (
        "grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+ md5=[0-9a-f]+'",
        "grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+ md5=[0-9a-f]+' \"$L\" | sed 's/.*md5=//'",
    ),
    (
        "grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+'",
        "grep -m1 -oE 'genesis markets=[0-9]+ native_balances=[0-9]+' \"$L\"",
    ),
    (
        "grep -q 'genesis markets=300 native_balances=100060 '",
        'grep -q "genesis markets=$M native_balances=$N " "$L"',
    ),
    (
        "grep -m1 'genesis markets=300 '",
        'grep -m1 "genesis markets=$M " "$L" | sed -n \'s/.* md5=//p\'',
    ),
)
# the checks the current run.log satisfies (True: its output is the genesis md5). The other two: the state check
# runs per WANT (asserted below), and "native_balances=N md5=" predates run.log's bulk_count= field (it matches on
# neither the pre-reuse script's cells nor today's: equal, nothing more)
MUST_PASS = {SCRIPT_CHECKS[1][0]: False, SCRIPT_CHECKS[3][0]: False, SCRIPT_CHECKS[4][0]: False, SCRIPT_CHECKS[5][0]: True}
GENESIS_WORDS = re.compile(
    r"genesis|state_hash|timeout_ms|native_bal|bulk_count|chain_id|perm_stakes|evm_block_interval|accounts +=|"
    r"validators +=|reusing existing|wrote "
)


def spec_expectations():
    """every run.log expectation of the campaign specs in the repo (spec-wide and per arm), and ozarchy 8p's"""
    out = []
    for g in SPEC_GLOBS:
        for p in sorted(glob.glob(os.path.join(REPO, g), recursive=True)):
            try:
                spec = json.load(open(p))
            except ValueError:
                continue
            if not isinstance(spec, dict):
                continue
            lists = [spec.get("expect")] + [
                a.get("expect") for a in spec.get("arms") or [] if isinstance(a, dict)
            ]
            for lst in lists:
                for e in lst or []:
                    if e.get("file") == "run.log":
                        x = {"min": 1, "max": None}
                        x.update(e)
                        x["src"] = os.path.relpath(p, REPO)
                        out.append(x)
    return out + [ARCHY_8P]


def grep_count(regex, path):
    """the driver's count (driver-body.sh: grep -E -c)"""
    r = subprocess.run(
        ["grep", "-E", "-c", "--", regex, path], capture_output=True, text=True
    )
    assert r.returncode in (0, 1), r.stderr
    return int(r.stdout.strip() or 0)


def script_check(snippet, log, want):
    m = re.search(r"\] genesis markets=(\d+) native_balances=(\d+) ", log["text"])
    env = dict(
        os.environ, L=log["path"], G=SB.genesis, M=m.group(1), N=m.group(2), WANT=want
    )
    r = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, env=env)
    return r.returncode, r.stdout


class ArchivedExpectationsTest(unittest.TestCase):
    def kinds(self, c):
        return ["build", "reuse"] + (["old"] if "old" in c else [])

    def test_spec_expectations_unchanged_on_a_reused_cell(self):
        exps = spec_expectations()
        self.assertGreater(
            len(exps), 3, "the testdata specs' run.log expectations were found"
        )
        for name, c in SB.logs.items():
            for e in exps:
                with self.subTest(config=name, regex=e["regex"], src=e["src"]):
                    n = {k: grep_count(e["regex"], c[k]["path"]) for k in self.kinds(c)}
                    self.assertEqual(len(set(n.values())), 1, n)
        # owner s136: an unset STATE_HASH_ACTIVATION now prints "= 1", so 8p's expectation
        # holds only for an explicit 'off' arm (archy's specs must set it)
        self.assertGreaterEqual(
            grep_count(ARCHY_8P["regex"], SB.logs["hash-off"]["reuse"]["path"]), 1
        )
        for name in ("default", "extra-env", "liq-thin", "force"):
            self.assertEqual(
                grep_count(ARCHY_8P["regex"], SB.logs[name]["reuse"]["path"]), 0, name
            )

    def test_archived_script_genesis_checks_pass_on_a_reused_cell(self):
        for name, c in SB.logs.items():
            for src, snippet in SCRIPT_CHECKS:
                for want in ('"off"', "off", "1"):
                    with self.subTest(config=name, check=src, want=want):
                        res = {
                            k: script_check(snippet, c[k], want) for k in self.kinds(c)
                        }
                        self.assertEqual(res["reuse"], res["build"])
                        if "old" in res:
                            self.assertEqual(res["reuse"], res["old"])
                        if src in MUST_PASS:
                            rc, out = res["reuse"]
                            self.assertEqual(rc, 0, src)
                            if MUST_PASS[src]:
                                self.assertEqual(out.strip(), c["md5"], src)
            state = "off" if name == "hash-off" else "1"
            self.assertEqual(
                script_check(SCRIPT_CHECKS[0][1], c["reuse"], state)[0], 0, name
            )

    def test_every_archived_genesis_check_is_covered(self):
        seen = {src: 0 for src, _ in SCRIPT_CHECKS}
        uncovered = []
        for g in SCRIPT_GLOBS:
            for p in sorted(glob.glob(os.path.join(REPO, g), recursive=True)):
                for i, line in enumerate(open(p, errors="replace"), 1):
                    if (
                        "grep" not in line
                        or "run.log" not in line
                        or not GENESIS_WORDS.search(line)
                    ):
                        continue
                    hits = [s for s in seen if s in line]
                    for s in hits:
                        seen[s] += 1
                    if not hits:
                        uncovered.append(
                            "%s:%d: %s" % (os.path.relpath(p, REPO), i, line.strip())
                        )
        self.assertEqual(
            uncovered, [], "archived genesis checks on run.log this test does not run"
        )
        self.assertEqual(
            [s for s, n in seen.items() if n == 0],
            [],
            "SCRIPT_CHECKS entries no archived script has",
        )


if __name__ == "__main__":
    unittest.main()
