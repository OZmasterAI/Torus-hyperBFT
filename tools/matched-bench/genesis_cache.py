#!/usr/bin/env python3
"""genesis_cache.py — reuse a cell's generated genesis when none of its inputs changed (python3 stdlib only).

run-cell.sh calls this when GENESIS_CACHE_DIR is set (the campaign driver sets it to $RUN/genesis-cache):

    genesis_cache.py key   --root MAINREPO --run-cell RUN_CELL_SH --param NAME=VALUE ...
        prints the genesis key: sha256 (first 20 hex) over every genesis input (KEY_FILES under MAINREPO, the
        run-cell.sh that post-processes the genesis, PARAMS, AMBIENT_ENV, jq's version). --explain prints the
        inputs as JSON instead.
    genesis_cache.py fetch --dir D --key K --dest GENESIS [--forced] [--root MAINREPO]
        a hit (D/K.genesis + D/K.meta.json, whose md5 the copy re-hashes): GENESIS = the cached file, prints the
        meta (one JSON line), rc 0; with --root the meta's "replay" holds the generator's run.log lines for this
        cell (replay()). rc 1 = miss (no entry, or --forced and the entry was not built under FORCE=1), rc 2 = the
        cached file's md5 is not its meta's, or the meta has no generator output (corrupt or an older entry:
        rebuild, the store overwrites it). The reason goes to stderr. GENESIS is replaced atomically, only on a hit.
    genesis_cache.py store --dir D --genesis GENESIS --md5 MD5 --gen-out FILE [--forced] [--meta NAME=VALUE ...]
                           + key's args
        keys the inputs as they are now (after a build, which may have written the weighted-full genesis), copies
        GENESIS to D/K.genesis (its md5 must be MD5), writes D/K.meta.json (md5, forced, the --meta values, the
        generator's output FILE as generator_output(), the key inputs) last, so an entry without one is never a
        hit, and prints K.

Generator output: before reuse (0f2ca85b) every cell's run.log held devnet/wsl/gen-3val-genesis.sh's output, and
campaign checks and spec expectations read it ("  state_hash_activation = off", "wrote G", ...). A reused cell
prints the same lines (test_genesis_reuse_lines.py): the generator's summary after its "wrote GENESIS" line is a
function of the genesis bytes and the generator script, both key inputs, so the entry keeps it verbatim; the lines
before it name paths and depend on FORCE (replay()).

The inputs (test_genesis_cache.py checks that every env name the generator scripts read is a key input or
EXEMPT_ENV): the generator scripts and their data (KEY_FILES, incl. the weighted-full base genesis and its params
sidecar), the cell's MARKETS / BULK_COUNT / TIMEOUT_BASE_MS / EVM_BLOCK_INTERVAL / BOOK_MODE / VALIDATOR_STAKES /
N_VALIDATORS / DL_K / DL_JCAP (the generator's knobs) and
LIQ_THIN / LIQ_THIN_AVAIL (run-cell.sh patches the genesis after the generator), the ambient env the generator
reads (STATE_HASH_ACTIVATION, BOOK_WINDOW_COMMIT, gen-weighted-genesis.sh's BASE / BULK_OFFSET / NATIVE_AVAIL / EVM_WEI) and the
md5 of the BASE file. FUNDED_BULK_END is a bench-throughput (load) argument, not a genesis input.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time

KEY_VERSION = 2  # 2: an entry holds the generator output (generator_output)
KEY_LEN = 20
# genesis inputs under run-cell.sh's MAINREPO (absent = "absent"; the weighted-full genesis and its sidecar may
# not exist before the first build, which then creates them: the store key is computed after the build)
KEY_FILES = (
    "devnet/wsl/gen-3val-genesis.sh",
    "devnet/wsl/bench-validator-keys.json",
    "devnet/wsl/keys-4val/bench-validator-keys.json",
    "devnet/wsl/keys-5val/bench-validator-keys.json",
    "devnet/wsl/keys-6val/bench-validator-keys.json",
    "devnet/wsl/keys-7val/bench-validator-keys.json",
    "devnet/genesis.json",
    "testnet/lib/cargo-bin.sh",
    "testnet/gen-weighted-genesis.sh",
    "testnet/genesis-weighted-full.json",
    "testnet/genesis-weighted-full.params.json",
)
# run-cell.sh's values for the cell (each --param is required: a missing one is an error, never a silent "")
PARAMS = (
    "MARKETS",
    "BULK_COUNT",
    "TIMEOUT_BASE_MS",
    "EVM_BLOCK_INTERVAL",
    "LIQ_THIN",
    "LIQ_THIN_AVAIL",
    "BOOK_MODE",
    "VALIDATOR_STAKES",
    "N_VALIDATORS",
    "DL_K",
    "DL_JCAP",
)
# env the generator scripts read from the environment run-cell.sh hands them (unset and empty are the same:
# the scripts read them as ${NAME:-default})
AMBIENT_ENV = (
    "STATE_HASH_ACTIVATION",
    "BOOK_WINDOW_COMMIT",
    "BASE",
    "BULK_OFFSET",
    "NATIVE_AVAIL",
    "EVM_WEI",
)
# env the generator reads that is not a key input, and why
EXEMPT_ENV = {
    "OUT": "the output path, not its content",
    "FORCE": "recorded in the entry (meta forced): with FORCE=1 only an entry built under FORCE=1 is a hit",
    "BENCH_BIN": "resolves bench-throughput, used only to build the weighted-full genesis (its bytes are a key input)",
    "BIN": "as BENCH_BIN (gen-weighted-genesis.sh, cargo-bin.sh)",
    "CARGO_TARGET_DIR": "as BENCH_BIN (cargo-bin.sh)",
}
DEFAULT_BASE = "testnet/genesis-weighted-base.json"
WEIGHTED_FULL = "testnet/genesis-weighted-full.json"
CHUNK = 1 << 20


def md5_file(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                return h.hexdigest()
            h.update(b)


def file_id(path):
    return md5_file(path) if os.path.isfile(path) else "absent"


def jq_version():
    try:
        r = subprocess.run(
            ["jq", "--version"], capture_output=True, text=True, timeout=10
        )
        return r.stdout.strip() or "unknown"
    except (OSError, subprocess.SubprocessError):
        return "absent"


def key_inputs(root, run_cell, params, env=None):
    """the genesis inputs as a dict (what the key hashes)"""
    env = os.environ if env is None else env
    missing = [p for p in PARAMS if p not in params]
    if missing:
        raise ValueError("missing --param %s" % ", ".join(missing))
    extra = sorted(set(params) - set(PARAMS))
    if extra:
        raise ValueError(
            "unknown --param %s (known: %s)" % (", ".join(extra), ", ".join(PARAMS))
        )
    files = {rel: file_id(os.path.join(root, rel)) for rel in KEY_FILES}
    amb = {k: env.get(k, "") for k in AMBIENT_ENV}
    base = amb["BASE"] or DEFAULT_BASE
    files["BASE:" + base] = file_id(
        base if os.path.isabs(base) else os.path.join(root, base)
    )
    files["run-cell.sh"] = file_id(run_cell)
    return {
        "version": KEY_VERSION,
        "files": files,
        "params": {k: params[k] for k in PARAMS},
        "env": amb,
        "jq": jq_version(),
    }


def key_of(inputs):
    raw = json.dumps(inputs, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()[:KEY_LEN]


def entry(d, key):
    return os.path.join(d, key + ".genesis"), os.path.join(d, key + ".meta.json")


def copy_hashed(src, dst):
    """copy src to dst (a temp name beside it), returning the md5 of the bytes copied"""
    h = hashlib.md5()
    with open(src, "rb") as fi, open(dst, "wb") as fo:
        while True:
            b = fi.read(CHUNK)
            if not b:
                break
            h.update(b)
            fo.write(b)
    return h.hexdigest()


def generator_output(lines, genesis, root):
    """the generator's run.log lines of a build, split at its last "wrote GENESIS" line: head (the weighted-full
    genesis step: "reusing existing FULL (BULK_COUNT=N)", or under FORCE=1 its rebuild's lines), summary (after
    it: the genesis' own fields, no paths). root: the build's MAINREPO (the generator's REPO)"""
    wrote = "wrote " + genesis
    at = [i for i, line in enumerate(lines) if line == wrote]
    if not at:
        raise ValueError("generator output has no %r line" % wrote)
    return {"root": root, "head": lines[: at[-1]], "summary": lines[at[-1] + 1 :]}


def generator_output_ok(go):
    return (
        isinstance(go, dict)
        and isinstance(go.get("root"), str)
        and all(isinstance(go.get(k), list) and all(isinstance(x, str) for x in go[k]) for k in ("head", "summary"))
    )


def replay(meta, dest, root, forced):
    """the lines gen-3val-genesis.sh prints when run now for this cell (OUT=dest, REPO=root) over the entry's inputs:
    the weighted-full genesis (a key input) is on disk, so without FORCE=1 it prints its "reusing existing" line
    (its BULK_COUNT = the entry's bulk_count, else it fails); under FORCE=1 it rebuilds that genesis from the same
    inputs: the entry's head (a FORCE=1 entry's, the only kind a FORCE=1 cell reuses) with its MAINREPO this one's.
    Then "wrote dest" and the entry's summary."""
    go = meta["generator_output"]
    if forced:
        old = go["root"].rstrip("/") + "/"
        head = [line.replace(old, root.rstrip("/") + "/") for line in go["head"]]
    else:
        head = [
            "reusing existing %s/%s (BULK_COUNT=%s)" % (root.rstrip("/"), WEIGHTED_FULL, meta["bulk_count"])
        ]
    return head + ["wrote " + dest] + go["summary"]


def fetch(d, key, dest, forced):
    """(rc, meta or reason): 0 hit (dest replaced), 1 miss, 2 md5 mismatch"""
    gen, meta_p = entry(d, key)
    if not (os.path.isfile(gen) and os.path.isfile(meta_p)):
        return 1, "no cache entry for key %s in %s" % (key, d)
    try:
        meta = json.load(open(meta_p))
    except (OSError, ValueError) as ex:
        return 2, "cache entry %s: unreadable meta %s: %s" % (key, meta_p, ex)
    if meta.get("key") != key or not isinstance(meta.get("md5"), str):
        return 2, "cache entry %s: meta %s names key %r / md5 %r" % (
            key,
            meta_p,
            meta.get("key"),
            meta.get("md5"),
        )
    if not generator_output_ok(meta.get("generator_output")) or "bulk_count" not in meta:
        return 2, "cache entry %s: meta %s has no generator output or bulk_count (an older genesis_cache.py's?)" % (
            key,
            meta_p,
        )
    if forced and meta.get("forced") is not True:
        return (
            1,
            "FORCE=1 and cache entry %s was not built under FORCE=1 (the first FORCE=1 cell builds)"
            % key,
        )
    tmp = "%s.cache-tmp.%d" % (dest, os.getpid())
    try:
        got = copy_hashed(gen, tmp)
        if got != meta["md5"]:
            return 2, "cache entry %s: %s has md5 %s, its meta says %s" % (
                key,
                gen,
                got,
                meta["md5"],
            )
        os.replace(tmp, dest)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    return 0, meta


def store(d, key, genesis, md5, forced, extra, inputs, gen_out):
    if not generator_output_ok(gen_out):
        raise ValueError("store: no generator output (generator_output())")
    os.makedirs(d, exist_ok=True)
    gen, meta_p = entry(d, key)
    tmp = gen + ".tmp.%d" % os.getpid()
    try:
        got = copy_hashed(genesis, tmp)
        if got != md5:
            raise ValueError(
                "%s has md5 %s, the cell logged %s (changed while caching?)"
                % (genesis, got, md5)
            )
        os.replace(tmp, gen)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
    meta = {
        "key": key,
        "md5": md5,
        "forced": bool(forced),
        "built_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    meta.update(extra)
    meta["generator_output"] = gen_out
    meta["inputs"] = inputs
    mtmp = meta_p + ".tmp.%d" % os.getpid()
    with open(mtmp, "w") as f:
        json.dump(meta, f, indent=1, sort_keys=True)
        f.write("\n")
    os.replace(mtmp, meta_p)
    return meta


def kv_list(items, what):
    out = {}
    for it in items or []:
        k, sep, v = it.partition("=")
        if not sep or not k:
            raise ValueError("%s %r is not NAME=VALUE" % (what, it))
        out[k] = v
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    k = sub.add_parser("key")
    k.add_argument("--root", required=True)
    k.add_argument("--run-cell", required=True)
    k.add_argument("--param", action="append")
    k.add_argument("--explain", action="store_true")
    f = sub.add_parser("fetch")
    f.add_argument("--dir", required=True)
    f.add_argument("--key", required=True)
    f.add_argument("--dest", required=True)
    f.add_argument("--forced", action="store_true")
    f.add_argument("--root", help="MAINREPO: print the generator lines for this cell as the meta's replay")
    s = sub.add_parser("store")
    s.add_argument("--dir", required=True)
    s.add_argument("--key", help="if given, must be the key of the inputs now")
    s.add_argument("--genesis", required=True)
    s.add_argument("--md5", required=True)
    s.add_argument("--forced", action="store_true")
    s.add_argument("--meta", action="append")
    s.add_argument("--gen-out", required=True, help="the generator's run.log output of the build")
    s.add_argument("--root", required=True)
    s.add_argument("--run-cell", required=True)
    s.add_argument("--param", action="append")
    a = ap.parse_args(argv)
    try:
        if a.cmd == "key":
            inp = key_inputs(a.root, a.run_cell, kv_list(a.param, "--param"))
            print(
                json.dumps(inp, indent=1, sort_keys=True) if a.explain else key_of(inp)
            )
            return 0
        if a.cmd == "fetch":
            rc, res = fetch(a.dir, a.key, a.dest, a.forced)
            if rc == 0:
                out = {x: y for x, y in res.items() if x not in ("inputs", "generator_output")}
                if a.root:
                    out["replay"] = replay(res, a.dest, a.root, a.forced)
                print(json.dumps(out, sort_keys=True))
            else:
                print(res, file=sys.stderr)
            return rc
        inp = key_inputs(a.root, a.run_cell, kv_list(a.param, "--param"))
        key = key_of(inp)
        if a.key and a.key != key:
            raise ValueError("store: --key %s is not the key of the inputs now (%s)" % (a.key, key))
        with open(a.gen_out, errors="replace") as f:
            gen_out = generator_output(f.read().splitlines(), a.genesis, a.root)
        store(a.dir, key, a.genesis, a.md5, a.forced, kv_list(a.meta, "--meta"), inp, gen_out)
        print(key)
        return 0
    except (OSError, ValueError) as ex:
        print("genesis_cache.py %s: %s" % (a.cmd, ex), file=sys.stderr)
        return 3


if __name__ == "__main__":
    sys.exit(main())
