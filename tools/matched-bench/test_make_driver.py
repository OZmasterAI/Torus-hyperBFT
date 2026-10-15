#!/usr/bin/env python3
"""test_make_driver.py — tests for campaign/make-driver.py (spec -> campaign driver) and its driver body.

    python3 tools/matched-bench/test_make_driver.py

Offline, no nodes, no cargo: drivers are generated into a tempdir, checked with `bash -n`, and their
functions are run in "library mode" (DRIVER_LIB_ONLY=1 source DRIVER) against fixture cell dirs and a
fake /proc (DRIVER_PROC). Pinned:

1. golden: the Job 4 horizon and decay specs (testdata/make-driver/p4j4-{h,d}.json) give the same arms,
   node env, harness env, cell order, replicates, markets, md5 and env checks as the hand-derived drivers
   ozarchy-p4j4-{h,d}-campaign.sh (copied verbatim into testdata/make-driver/), compared as tables.
2. the env check reads TORUS_* from node-env-all.txt and every other process env (MALLOC_CONF) from
   node-preload.txt (Job 4's false STOP read MALLOC_CONF from node-env-all.txt).
3. the md5 check hashes the running exe (/proc/PID/exe), never the staged file, which may be a wrapper
   script (g3's 4 needless reruns hashed the g-je wrapper).
4. a bad spec (unknown key, missing baseline arm, duplicate arm, ...) is a hard error.
"""

import importlib.util
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GEN = os.path.join(HERE, "campaign", "make-driver.py")
EXAMPLE = os.path.join(HERE, "campaign", "driver-spec.example.json")
FIX = os.path.join(HERE, "testdata", "make-driver")
CELLFIX = os.path.join(HERE, "testdata", "cell-step")
NODE_MD5_H = "03a2b75727727f972cf5e80865646f0d"


def load_gen():
    spec = importlib.util.spec_from_file_location("make_driver", GEN)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def md5_of(path):
    import hashlib

    return hashlib.md5(open(path, "rb").read()).hexdigest()


def minimal_spec(tmp, **over):
    """a valid one-binary two-arm spec whose paths all live in tmp (the stage dir holds a fake node)"""
    st = os.path.join(tmp, "stage", "b-run", "release")
    os.makedirs(st, exist_ok=True)
    node = os.path.join(st, "torus-node")
    if not os.path.exists(node):
        with open(node, "wb") as f:
            f.write(b"\x7fELF fake node binary\x00\x01")
        os.chmod(node, 0o755)
        with open(os.path.join(st, "bench-throughput"), "wb") as f:
            f.write(b"\x7fELF fake bench\x00")
        os.chmod(os.path.join(st, "bench-throughput"), 0o755)
    wt = os.path.join(tmp, "wt")
    os.makedirs(wt, exist_ok=True)
    s = {
        "campaign": "test-camp",
        "results_root": os.path.join(tmp, "res"),
        "run_dir": os.path.join(tmp, "res", "test-camp-run"),
        "label_prefix": "test-camp-10m",
        "harness_worktree": wt,
        "tools_dir": HERE,
        "devnet_dir": os.path.join(tmp, "devnet"),
        "binaries": {"b": {"stage_dir": os.path.dirname(st), "node_md5": md5_of(node)}},
        "markets": 10,
        "duration_s": 120,
        "replicates": 2,
        "baseline": "A",
        "arms": [
            {"label": "A"},
            {
                "label": "M1",
                "env": {"MALLOC_CONF": "dirty_decay_ms:1000", "TORUS_X_KNOB": "7"},
            },
        ],
    }
    s.update(over)
    return s


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="make-driver-test-")
        self.gen = load_gen()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def write_spec(self, spec, name="spec.json"):
        p = os.path.join(self.tmp, name)
        with open(p, "w") as f:
            json.dump(spec, f)
        return p

    def generate(self, spec_path, name="driver.sh"):
        out = os.path.join(self.tmp, name)
        r = subprocess.run(
            [sys.executable, "-I", GEN, spec_path, "-o", out],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return out

    def lib(self, driver, script, env=None):
        """run `script` after sourcing the driver in library mode (functions + config only)"""
        e = dict(os.environ)
        e.update(env or {})
        src = "DRIVER_LIB_ONLY=1; source %s || exit 99\n%s" % (
            shlex.quote(driver),
            script,
        )
        return subprocess.run(
            ["bash", "-c", src], capture_output=True, text=True, env=e, timeout=120
        )

    def plan(self, driver, smoke=False):
        r = self.lib(driver, "show_plan%s" % (" smoke" if smoke else ""))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r.stdout.splitlines()


# ---------------------------------------------------------------- golden (Job 4)


def kv_dict(s):
    d = {}
    for kv in s.split():
        k, _, v = kv.partition("=")
        d[k] = v
    return d


def ref_tables(path):
    """the arm / env / order tables of a hand-derived Job 4 driver (campaign-template lineage)"""
    t = open(path).read()
    aenv = dict(re.findall(r'^AENV\[(\w+)\]="([^"]*)"$', t, re.M))
    arms = {}
    for m in re.finditer(
        r"^arm (\S+) (\S+) (\S+) (\S+) (\S+) (\S+) (\S+) (\S+)$", t, re.M
    ):
        a, b, tail, _knob, _prof, cloid, orc, mk = m.groups()
        arms[a] = {"bin": b, "tail": tail, "cloid": cloid, "orc": orc, "mk": mk}
    knobs = re.search(r'^KNOBS="([^"]*)"$', t, re.M).group(1)
    cells_txt = re.search(r"^CELLS=\( (.*) \)$", t, re.M).group(1)
    cells = [tuple(c.split(":")[:4]) for c in re.findall(r'"([^"]+)"', cells_txt)]
    run, prefix = re.search(r"^RUN=\$R/(\S+); P=(\S+)$", t, re.M).groups()
    budget = re.search(r"^BUDGET=(\d+)$", t, re.M).group(1)
    depth = re.search(r"^DEPTH=(\d+)", t, re.M).group(1)
    sha_b = re.search(r"^SHA_B=(\w+)$", t, re.M).group(1)
    md5_row = re.search(
        r'BMD5\[b\]=\$\(awk -v s="\$\{SHA_B:0:8\}" \'\$1=="(\w+)"', t
    ).group(1)
    # the harness env of cell(): E=(...) with the driver's per-arm values resolved (N = the launch arg 4)
    e_txt = re.search(r"^    E=\((.*?)\)\n", t, re.M | re.S).group(1)
    henv_tpl = dict(kv.split("=", 1) for kv in shlex.split(e_txt))
    # env checks: armenvchk's absent list (node-env-all.txt) and heapchk's MALLOC_CONF
    ae = re.search(r"^armenvchk\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    absent_torus = re.search(r"for k in ([A-Z_ ]+); do", ae).group(1).split()
    i = t.index('armenvchk "$label"')
    assert "node-env-all.txt" in t[i : i + 120]
    hc = re.search(r"^heapchk\(\) \{.*?^\}\n", t, re.M | re.S).group(0)
    # the file heapchk reads MALLOC_CONF from: node-preload.txt in the fixed d driver; node-env-all.txt
    # (TORUS_* only, so the check is vacuous) in the h driver, made before Job 4's false STOP fixed d
    hc_files = set(re.findall(r'MALLOC_CONF=[^"]*" "\$o/(node-[a-z-]+\.txt)"', hc))
    assert len(hc_files) == 1, hc
    return {
        "aenv": aenv,
        "arms": arms,
        "knobs": knobs,
        "cells": cells,
        "run": run,
        "prefix": prefix,
        "budget": budget,
        "depth": depth,
        "sha_b": sha_b,
        "md5_row": md5_row,
        "henv_tpl": henv_tpl,
        "absent_torus": absent_torus,
        "malloc_file": hc_files.pop(),
    }


def ref_harness_env(ref, arm):
    a = ref["arms"][arm]
    sub = {
        "$mif": "4",
        "$bud": ref["budget"],
        "${AORC[$arm]}": a["orc"],
        "$tail": a["tail"],
        "$DEPTH": ref["depth"],
        "${ACLOID[$arm]}": a["cloid"],
    }
    out = {}
    for k, v in ref["henv_tpl"].items():
        if k in ("TARGET_DIR", "TOOLS_DIR", "DATA_ROOT"):
            continue  # paths: compared separately
        out[k] = sub.get(v, v)
    return out


def ref_checks(ref, arm):
    """(present, absent) env checks of the reference driver for one arm: name -> (value, file)"""
    present = {}
    for k, v in kv_dict(ref["knobs"]).items():
        present[k] = (v, "node-env-all.txt")  # envok / knobok / statschk
    for k, v in kv_dict(ref["aenv"][arm]).items():
        present[k] = (v, ref["malloc_file"] if k == "MALLOC_CONF" else "node-env-all.txt")
    absent = {k: "node-env-all.txt" for k in ref["absent_torus"] if k not in present}
    if "MALLOC_CONF" not in present:
        absent["MALLOC_CONF"] = ref["malloc_file"]
    return present, absent


def gen_tables(lines):
    arms, henv, present, absent, cells = {}, {}, {}, {}, []
    paths = {}
    for ln in lines:
        f = ln.split(" ")
        if f[0] == "ARM":
            arms[f[1]] = {
                "bin": f[2],
                "md5": f[3],
                "xe": ln.split(" xe=", 1)[1] if " xe=" in ln else "",
            }
        elif f[0] == "HENV":
            henv.setdefault(f[1], {})
            if len(f) > 2:
                k, _, v = f[2].partition("=")
                henv[f[1]][k] = v
        elif f[0] == "PRESENT":
            k, _, v = f[2].partition("=")
            present.setdefault(f[1], {})[k] = (v, f[3])
        elif f[0] == "ABSENT":
            absent.setdefault(f[1], {})[f[2]] = f[3]
        elif f[0] == "CELL":
            cells.append((f[1], f[2], f[3], f[4]))
        elif f[0] == "PATH":
            paths[f[1]] = f[2]
    return arms, henv, present, absent, cells, paths


class GoldenTest(Base):
    def golden(self, kind):
        ref = ref_tables(os.path.join(FIX, "ozarchy-p4j4-%s-campaign.sh" % kind))
        # the one intended difference: MALLOC_CONF is read from node-preload.txt (the h driver read node-env-all.txt)
        self.assertEqual(ref["malloc_file"], {"h": "node-env-all.txt", "d": "node-preload.txt"}[kind])
        ref["malloc_file"] = "node-preload.txt"
        drv = self.generate(
            os.path.join(FIX, "p4j4-%s.json" % kind), "p4j4-%s.sh" % kind
        )
        r = subprocess.run(["bash", "-n", drv], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        arms, henv, present, absent, cells, paths = gen_tables(self.plan(drv))
        # arms, binary, md5 (the reference reads md5s.txt row N bdd59888 -> the Job 4 node md5)
        self.assertEqual(sorted(arms), sorted(ref["aenv"]))
        self.assertEqual(ref["md5_row"], "N")
        for a in arms:
            self.assertEqual(arms[a]["md5"], NODE_MD5_H, a)
            # node env = KNOBS + the arm's AENV (the reference's xe), as a dict
            want = kv_dict(ref["knobs"])
            want.update(kv_dict(ref["aenv"][a]))
            self.assertEqual(kv_dict(arms[a]["xe"]), want, a)
            self.assertEqual(henv[a], ref_harness_env(ref, a), a)
            rp, ra = ref_checks(ref, a)
            self.assertEqual(present.get(a, {}), rp, a)
            self.assertEqual(absent.get(a, {}), ra, a)
        # order, replicates, durations, kinds, warm-up first
        self.assertEqual(cells, ref["cells"])
        self.assertEqual(paths["RUN"], "/home/oz/bench-results-matched/" + ref["run"])
        self.assertEqual(paths["P"], ref["prefix"])
        self.assertEqual(paths["MARKETS"], ref["arms"][cells[0][1]]["mk"])
        self.assertEqual(paths["RATE"], "76000")
        return ref, cells

    def test_golden_horizon(self):
        _, cells = self.golden("h")
        self.assertEqual(len(cells), 16)

    def test_golden_decay(self):
        _, cells = self.golden("d")
        self.assertEqual(len(cells), 10)

    def test_example_spec_generates(self):
        drv = self.generate(EXAMPLE, "example.sh")
        r = subprocess.run(["bash", "-n", drv], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_rotation_and_fixed_order(self):
        rot = self.gen.cell_list(
            ["A", "B", "C"], 4, "rotate", 120, ("w1", "A", 60)
        )
        self.assertEqual(
            [c[0] for c in rot],
            [
                "w1",
                "r1-A",
                "r1-B",
                "r1-C",
                "r2-B",
                "r2-C",
                "r2-A",
                "r3-C",
                "r3-A",
                "r3-B",
                "r4-A",
                "r4-B",
                "r4-C",
            ],
        )
        fx = self.gen.cell_list(["A", "B"], 2, "fixed", 120, None)
        self.assertEqual([c[0] for c in fx], ["r1-A", "r1-B", "r2-A", "r2-B"])
        ex = self.gen.cell_list(
            ["A", "B"], 2, ["r1:A", "r1:B", "r2:B", "r2:A"], 120, None
        )
        self.assertEqual(
            [c[:2] for c in ex],
            [("r1-A", "A"), ("r1-B", "B"), ("r2-B", "B"), ("r2-A", "A")],
        )


# ---------------------------------------------------------------- env check


def node_files(cell, torus_lines, preload_lines):
    os.makedirs(cell, exist_ok=True)
    with open(os.path.join(cell, "node-env-all.txt"), "w") as f:
        f.write(
            "".join("pid=%d %s\n" % (100 + i, ln) for i, ln in enumerate(torus_lines))
        )
    with open(os.path.join(cell, "node-preload.txt"), "w") as f:
        f.write(
            "".join("pid=%d %s\n" % (100 + i, ln) for i, ln in enumerate(preload_lines))
        )


class EnvCheckTest(Base):
    def setUp(self):
        super().setUp()
        self.drv = self.generate(self.write_spec(minimal_spec(self.tmp)))
        self.cell = os.path.join(self.tmp, "cell")

    def chk(self, arm):
        return self.lib(self.drv, "envchk %s %s" % (shlex.quote(self.cell), arm))

    def test_malloc_conf_missing_from_preload_fails(self):
        """M1's MALLOC_CONF is in node-env-all.txt (TORUS_* only in real cells) but not in node-preload.txt: FAIL"""
        node_files(self.cell, ["TORUS_X_KNOB=7 "] * 3, ["", "", ""])
        with open(os.path.join(self.cell, "node-env-all.txt"), "a") as f:
            f.write(
                ""
            )  # node-env-all.txt never carries MALLOC_CONF; the check must not look there
        r = self.chk("M1")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("CHECK FAIL", r.stdout)
        self.assertIn("MALLOC_CONF", r.stdout)
        self.assertIn("node-preload.txt", r.stdout)

    def test_malloc_conf_present_in_preload_passes(self):
        node_files(
            self.cell, ["TORUS_X_KNOB=7 "] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3
        )
        r = self.chk("M1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("CHECK OK", r.stdout)

    def test_malloc_conf_on_two_of_three_nodes_fails(self):
        node_files(
            self.cell,
            ["TORUS_X_KNOB=7 "] * 3,
            ["MALLOC_CONF=dirty_decay_ms:1000 "] * 2 + [""],
        )
        self.assertNotEqual(self.chk("M1").returncode, 0)

    def test_baseline_with_malloc_conf_fails(self):
        """an arm that does not set MALLOC_CONF must run without it (jemalloc defaults)"""
        node_files(self.cell, [""] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3)
        r = self.chk("A")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        node_files(self.cell, [""] * 3, [""] * 3)
        r = self.chk("A")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_torus_knob_checked_in_node_env_all(self):
        node_files(
            self.cell,
            ["TORUS_X_KNOB=7 "] * 2 + [""],
            ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3,
        )
        r = self.chk("M1")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("TORUS_X_KNOB=7", r.stdout)
        self.assertIn("node-env-all.txt", r.stdout)
        # another arm's knob on the baseline: FAIL
        node_files(self.cell, ["TORUS_X_KNOB=7 "] * 3, [""] * 3)
        self.assertNotEqual(self.chk("A").returncode, 0)

    def test_wrong_value_fails(self):
        node_files(
            self.cell, ["TORUS_X_KNOB=8 "] * 3, ["MALLOC_CONF=dirty_decay_ms:1000 "] * 3
        )
        self.assertNotEqual(self.chk("M1").returncode, 0)

    def test_record_writes_preload_from_process_env(self):
        """envrec_now: MALLOC_CONF from each node's environ goes to node-preload.txt, TORUS_* to node-env-all.txt"""
        proc, dev = fake_proc(
            self.tmp,
            os.path.join(self.tmp, "stage", "b-run", "release", "torus-node"),
            ["TORUS_X_KNOB=7", "MALLOC_CONF=dirty_decay_ms:1000", "HOME=/x"],
        )
        os.makedirs(self.cell, exist_ok=True)
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && envchk %s M1"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        pre = open(os.path.join(self.cell, "node-preload.txt")).read()
        allv = open(os.path.join(self.cell, "node-env-all.txt")).read()
        self.assertEqual(pre.count("MALLOC_CONF=dirty_decay_ms:1000"), 3, pre)
        self.assertNotIn("MALLOC_CONF", allv)
        self.assertEqual(allv.count("TORUS_X_KNOB=7"), 3)


# ---------------------------------------------------------------- md5 check


def fake_proc(tmp, exe, environ):
    """a fake /proc with 3 node pids whose exe links to `exe`, and a devnet pids file"""
    proc = os.path.join(tmp, "proc")
    dev = os.path.join(tmp, "devnet")
    os.makedirs(os.path.join(dev, "run"), exist_ok=True)
    pids = [4101, 4102, 4103]
    for p in pids:
        d = os.path.join(proc, str(p))
        os.makedirs(d, exist_ok=True)
        if os.path.lexists(os.path.join(d, "exe")):
            os.remove(os.path.join(d, "exe"))
        os.symlink(exe, os.path.join(d, "exe"))
        with open(os.path.join(d, "environ"), "wb") as f:
            f.write(b"\0".join(x.encode() for x in environ) + b"\0")
    with open(os.path.join(dev, "run", "pids"), "w") as f:
        f.write("\n".join(map(str, pids)) + "\n")
    return proc, dev


class Md5Test(Base):
    def setUp(self):
        super().setUp()
        rel = os.path.join(self.tmp, "real", "release")
        os.makedirs(rel)
        self.real = os.path.join(rel, "torus-node")
        with open(self.real, "wb") as f:
            f.write(b"\x7fELF the real node\x00")
        os.chmod(self.real, 0o755)
        wst = os.path.join(self.tmp, "stage", "w-run", "release")
        os.makedirs(wst)
        self.wrapper = os.path.join(wst, "torus-node")
        with open(self.wrapper, "w") as f:
            f.write(
                '#!/bin/sh\nexport LD_PRELOAD=/usr/lib/libjemalloc.so.2\nexec %s "$@"\n'
                % self.real
            )
        os.chmod(self.wrapper, 0o755)
        self.assertNotEqual(md5_of(self.real), md5_of(self.wrapper))
        spec = minimal_spec(self.tmp)
        spec["binaries"]["w"] = {
            "stage_dir": os.path.dirname(wst),
            "node_md5": md5_of(self.real),
            "wrapper": True,
        }
        for a in spec["arms"]:
            a["binary"] = "b"
        spec["arms"].append({"label": "GJ", "binary": "w"})
        self.spec = spec
        self.drv = self.generate(self.write_spec(spec))
        self.cell = os.path.join(self.tmp, "cell")
        os.makedirs(self.cell)

    def test_md5_hashes_running_exe_not_wrapper(self):
        """the wrapper arm's node runs the real exe (/proc/PID/exe): md5 = real node md5 -> OK"""
        proc, dev = fake_proc(
            self.tmp, self.real, ["LD_PRELOAD=/usr/lib/libjemalloc.so.2"]
        )
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && md5chk %s GJ"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        rec = open(os.path.join(self.cell, "node-environ-trie.txt")).read()
        self.assertEqual(rec.count("exe_md5=" + md5_of(self.real)), 3, rec)
        self.assertNotIn(md5_of(self.wrapper), rec)

    def test_md5_mismatch_fails(self):
        """a node that runs the wrapper file itself (not the real exe) does not match the arm md5"""
        proc, dev = fake_proc(self.tmp, self.wrapper, [])
        r = self.lib(
            self.drv,
            "DEVNET=%s; envrec_now %s && md5chk %s GJ"
            % (shlex.quote(dev), shlex.quote(self.cell), shlex.quote(self.cell)),
            env={"DRIVER_PROC": proc},
        )
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("CHECK FAIL", r.stdout)

    def test_preflight_resolves_wrapper_target(self):
        """dry preflight: a wrapper binary's staged md5 is the md5 of the node it execs"""
        r = subprocess.run(
            ["bash", self.drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("PREFLIGHT OK", r.stdout)
        # the same wrapper declared as a plain binary: its own md5 is not the node md5 -> PREFLIGHT FAIL
        self.spec["binaries"]["w"]["wrapper"] = False
        drv2 = self.generate(self.write_spec(self.spec, "spec2.json"), "driver2.sh")
        r = subprocess.run(
            ["bash", drv2, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("PREFLIGHT FAIL", r.stdout)

    def test_wrapper_arm_skips_summary_md5_in_step(self):
        """the step's summary.json md5 is run-cell.sh's hash of the staged file (the wrapper): off for wrapper arms"""
        r = self.lib(self.drv, "step_summary_md5 GJ; step_summary_md5 A")
        self.assertEqual(r.stdout.split(), ["0", "1"], r.stdout + r.stderr)


class CellStepSummaryMd5Test(unittest.TestCase):
    """cell_step.py --no-summary-md5: only the running-process records count"""

    def test_flag(self):
        tmp = tempfile.mkdtemp(prefix="make-driver-step-")
        try:
            lb = "ozarchy-p3gate-300m-r1-m"
            cell = os.path.join(tmp, lb)
            shutil.copytree(os.path.join(CELLFIX, lb), cell)
            shutil.copy(os.path.join(CELLFIX, lb + ".cell.rc"), cell + ".cell.rc")
            sp = os.path.join(cell, "summary.json")
            s = json.load(open(sp))
            s["binaries"]["torus_node_md5"] = (
                "0123456789abcdef0123456789abcdef"  # a wrapper's md5
            )
            json.dump(s, open(sp, "w"))
            base = [
                sys.executable,
                "-I",
                os.path.join(HERE, "cell_step.py"),
                "record",
                "--cell-dir",
                cell,
                "--label",
                lb,
                "--tag",
                "r1-m",
                "--arm",
                "m",
                "--rep",
                "r1",
                "--staged-md5",
                "f10815747be79f22f502b51a8cd466eb",
            ]
            run1 = os.path.join(tmp, "run1")
            run2 = os.path.join(tmp, "run2")
            os.makedirs(run1)
            os.makedirs(run2)
            r = subprocess.run(
                base + ["--run-dir", run1], capture_output=True, text=True
            )
            self.assertIn(
                "bad:md5",
                open(os.path.join(run1, "cells.tsv")).read(),
                r.stdout + r.stderr,
            )
            r = subprocess.run(
                base + ["--run-dir", run2, "--no-summary-md5"],
                capture_output=True,
                text=True,
            )
            row = (
                open(os.path.join(run2, "cells.tsv")).read().splitlines()[1].split("\t")
            )
            self.assertEqual(row[5], "yes", r.stdout + r.stderr)
            self.assertEqual(row[7], "ok", row)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


# ---------------------------------------------------------------- cells.tsv, CELL-FLAG, smoke, done marker


class DriverFlowTest(Base):
    def setUp(self):
        super().setUp()
        self.spec = minimal_spec(self.tmp)
        self.drv = self.generate(self.write_spec(self.spec))
        self.R = self.spec["results_root"]
        self.RUN = self.spec["run_dir"]
        os.makedirs(self.RUN, exist_ok=True)

    def test_run_spec_rows_and_cell_flag(self):
        """run_spec (generated body) with a stub cell(): clean cell -> ok row; failed check -> bad:check row,
        CELL-FLAG line in campaign.log, queued rerun"""
        lb = "ozarchy-p3gate-300m-r1-m"
        stub = r"""
source "$TOOLS/cell-step.sh"
export CELL_STEP_LOG=$RUN/campaign.log
BMD5[b]=f10815747be79f22f502b51a8cd466eb
cell() { local label=$P-$1
    cp -r %(fix)s "$R/$label"; echo rc=0 > "$R/$label.cell.rc"; STARTED_LABEL=$label
    [ "$1" = r1-M1 ] && { echo "CHECK FAIL $label: x"; CELL_FAIL=check; return 1; }
    return 0; }
run_spec "r1-A:A:120:none:"; echo "a=$?"
run_spec "r1-M1:M1:120:none:"; echo "m=$?"
""" % {"fix": shlex.quote(os.path.join(CELLFIX, lb))}
        r = self.lib(self.drv, stub)
        self.assertIn("a=0", r.stdout, r.stdout + r.stderr)
        self.assertIn("m=1", r.stdout, r.stdout + r.stderr)
        rows = [
            ln.split("\t")
            for ln in open(os.path.join(self.RUN, "cells.tsv")).read().splitlines()
        ]
        self.assertEqual(rows[0], "label arm rep rc agree md5_ok metric flag".split())
        self.assertEqual(
            [(x[0], x[1], x[2], x[7]) for x in rows[1:]],
            [
                ("test-camp-10m-r1-A", "A", "r1", "ok"),
                ("test-camp-10m-r1-M1", "M1", "r1", "bad:check"),
            ],
        )
        log = open(os.path.join(self.RUN, "campaign.log")).read()
        self.assertIn("CELL-FLAG test-camp-10m-r1-M1 check", log)
        self.assertIn(
            "test-camp-10m-r1-M1-rerun",
            open(os.path.join(self.RUN, "rerun-queue.tsv")).read(),
        )

    def test_dry_lists_warmup_first_and_smoke_alone(self):
        r = subprocess.run(
            ["bash", self.drv, "dry"], capture_output=True, text=True, timeout=60
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        cells = self.plan(self.drv)
        tags = [ln.split()[1] for ln in cells if ln.startswith("CELL ")]
        self.assertEqual(tags, ["w1", "r1-A", "r1-M1", "r2-M1", "r2-A"])
        sm = [ln for ln in self.plan(self.drv, smoke=True) if ln.startswith("CELL ")]
        self.assertEqual(len(sm), 1)
        self.assertEqual(sm[0].split()[1:4], ["s0", "A", "60"])
        paths = gen_tables(self.plan(self.drv, smoke=True))[5]
        self.assertEqual(paths["RUN"], self.RUN + "/smoke")
        self.assertEqual(paths["P"], "test-camp-10m-smoke")

    def test_run_outside_unit_refuses_and_writes_done(self):
        """`run` outside a bench-*.service unit: PREFLIGHT FAIL (launch goes through detach.sh, which holds the
        build lock), and campaign.done carries the exit code so the waiting session never hangs"""
        r = subprocess.run(
            ["bash", self.drv, "run"], capture_output=True, text=True, timeout=60
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("PREFLIGHT FAIL", r.stdout)
        done = open(os.path.join(self.RUN, "campaign.done")).read()
        self.assertTrue(done.startswith("exit=1"), done)

    def test_launch_goes_through_detach(self):
        """`launch` execs DETACH NAME RUN/campaign.log DRIVER run (detach.sh takes the build lock)"""
        fake = os.path.join(self.tmp, "fake-detach.sh")
        with open(fake, "w") as f:
            f.write('#!/bin/bash\necho "DETACH $*"\n')
        os.chmod(fake, 0o755)
        r = subprocess.run(
            ["bash", self.drv, "launch"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DRIVER_DETACH=fake),
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(
            "DETACH test-camp %s/campaign.log %s run"
            % (self.RUN, os.path.realpath(self.drv)),
            r.stdout,
        )
        r = subprocess.run(
            ["bash", self.drv, "launch", "smoke"],
            capture_output=True,
            text=True,
            timeout=60,
            env=dict(os.environ, DRIVER_DETACH=fake),
        )
        self.assertIn(
            "DETACH test-camp-smoke %s/smoke/campaign.log %s run smoke"
            % (self.RUN, os.path.realpath(self.drv)),
            r.stdout,
        )

    def test_body_wiring(self):
        t = open(self.drv).read()
        self.assertIn("cell_step_rerun_pass", t)
        self.assertNotRegex(t, r"cell_step [^\n]*&\s*$")
        self.assertIn("campaign.done", t)
        self.assertIn('"$DETACH" "$label"', t)  # every cell in its own bench unit

    def test_expect_and_market_checks(self):
        spec = minimal_spec(
            self.tmp,
            expect=[{"file": "run.log", "regex": "oracle feed pid [0-9]+;", "min": 1}],
        )
        spec["arms"][1]["expect"] = [
            {"file": "run.log", "regex": "admission horizon", "min": 0, "max": 0}
        ]
        drv = self.generate(self.write_spec(spec, "s3.json"), "d3.sh")
        cell = os.path.join(self.tmp, "c3")
        os.makedirs(cell)
        with open(os.path.join(cell, "run.log"), "w") as f:
            f.write("[x] oracle feed pid 123; ok\n")
        with open(os.path.join(cell, "summary.json"), "w") as f:
            json.dump({"cell": {"markets": 10}}, f)
        q = shlex.quote(cell)
        self.assertEqual(
            self.lib(drv, "expectchk %s M1 && marketchk %s" % (q, q)).returncode, 0
        )
        with open(os.path.join(cell, "run.log"), "a") as f:
            f.write("admission horizon 5000\n")
        self.assertNotEqual(self.lib(drv, "expectchk %s M1" % q).returncode, 0)
        self.assertEqual(self.lib(drv, "expectchk %s A" % q).returncode, 0)
        with open(os.path.join(cell, "summary.json"), "w") as f:
            json.dump({"cell": {"markets": 300}}, f)
        self.assertNotEqual(self.lib(drv, "marketchk %s" % q).returncode, 0)


# ---------------------------------------------------------------- bad specs


class SpecValidationTest(Base):
    def bad(self, spec, needle):
        with self.assertRaises(self.gen.SpecError) as cm:
            self.gen.validate(spec)
        self.assertIn(needle, str(cm.exception))

    def test_minimal_ok(self):
        self.gen.validate(minimal_spec(self.tmp))

    def test_unknown_top_key(self):
        self.bad(minimal_spec(self.tmp, cell_secs=60), "cell_secs")

    def test_unknown_arm_key(self):
        s = minimal_spec(self.tmp)
        s["arms"][0]["knob"] = "x"
        self.bad(s, "knob")

    def test_unknown_binary_key(self):
        s = minimal_spec(self.tmp)
        s["binaries"]["b"]["md5"] = "x"
        self.bad(s, "md5")

    def test_missing_baseline_arm(self):
        self.bad(minimal_spec(self.tmp, baseline="Z"), "baseline")

    def test_duplicate_arm(self):
        s = minimal_spec(self.tmp)
        s["arms"].append({"label": "A"})
        self.bad(s, "duplicate")

    def test_missing_required(self):
        s = minimal_spec(self.tmp)
        del s["replicates"]
        self.bad(s, "replicates")

    def test_unknown_binary_ref(self):
        s = minimal_spec(self.tmp)
        s["arms"][1]["binary"] = "nope"
        self.bad(s, "nope")

    def test_env_value_with_space(self):
        s = minimal_spec(self.tmp)
        s["arms"][1]["env"]["MALLOC_CONF"] = "a b"
        self.bad(s, "MALLOC_CONF")

    def test_reserved_harness_env(self):
        self.bad(minimal_spec(self.tmp, harness_env={"TARGET_DIR": "/x"}), "TARGET_DIR")
        self.bad(
            minimal_spec(self.tmp, harness_env={"CRASH_KILL_AT_S": "40"}),
            "CRASH_KILL_AT_S",
        )

    def test_bad_md5(self):
        s = minimal_spec(self.tmp)
        s["binaries"]["b"]["node_md5"] = "xyz"
        self.bad(s, "node_md5")

    def test_explicit_order_unknown_arm(self):
        s = minimal_spec(self.tmp, order=["r1:A", "r1:Q"])
        del s["replicates"]
        self.bad(s, "Q")
        s["order"] = ["r1:A", "r1:M1"]
        self.gen.validate(s)
        self.bad(minimal_spec(self.tmp, order=["r1:A", "r1:M1"]), "replicates")

    def test_generator_cli_rejects_bad_spec(self):
        p = self.write_spec(minimal_spec(self.tmp, baseline="Z"))
        r = subprocess.run(
            [sys.executable, "-I", GEN, p, "-o", os.path.join(self.tmp, "x.sh")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("SPEC ERROR", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "x.sh")))



class Md5SourceTest(Base):
    def test_md5_file_rows(self):
        """node_row / bench_row read md5s.txt (last line whose first two columns match), as the Job 4 driver's awk"""
        mf = os.path.join(self.tmp, "md5s.txt")
        with open(mf, "w") as f:
            f.write("# comment\nN bdd59888 %s\nbench-F ff822b95 %s\nN bdd59888 %s\n" % ("1" * 32, "2" * 32, "3" * 32))
        s = minimal_spec(self.tmp)
        st = s["binaries"]["b"]["stage_dir"]
        s["binaries"]["b"] = {"stage_dir": st, "md5_file": mf, "node_row": "N bdd59888", "bench_row": "bench-F ff822b95"}
        p = self.gen.validate(s)
        self.assertEqual(p["binaries"]["b"]["node_md5"], "3" * 32)
        self.assertEqual(p["binaries"]["b"]["bench_md5"], "2" * 32)
        s["binaries"]["b"]["node_row"] = "N 00000000"
        with self.assertRaises(self.gen.SpecError):
            self.gen.validate(s)
        s["binaries"]["b"] = {"stage_dir": st, "node_md5": "3" * 32, "node_row": "N bdd59888", "md5_file": mf}
        with self.assertRaises(self.gen.SpecError):
            self.gen.validate(s)

    def test_md5chk_accepts_older_8_char_records(self):
        """records of older drivers (exe_md5=<first 8>) are matched as a prefix; another md5 still fails"""
        spec = minimal_spec(self.tmp)
        drv = self.generate(self.write_spec(spec))
        want = spec["binaries"]["b"]["node_md5"]
        cell = os.path.join(self.tmp, "c8")
        os.makedirs(cell)
        with open(os.path.join(cell, "node-environ-trie.txt"), "w") as f:
            f.write("".join("pid=%d exe_md5=%s TRIE_VAR_MISSING\n" % (i, want[:8]) for i in range(3)))
        self.assertEqual(self.lib(drv, "md5chk %s A" % shlex.quote(cell)).returncode, 0)
        with open(os.path.join(cell, "node-environ-trie.txt"), "w") as f:
            f.write("".join("pid=%d exe_md5=%s\n" % (i, "0" * 8) for i in range(3)))
        self.assertNotEqual(self.lib(drv, "md5chk %s A" % shlex.quote(cell)).returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
