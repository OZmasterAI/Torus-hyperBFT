#!/usr/bin/env python3
"""t12-fork-check.py — cross-node committed-hash fork detector (T1.2, n=4/f=1).

Consensus-safety cross-checker for the think-dev grandparent-lock fix. Given the
JSON-RPC endpoints of every validator in a devnet, it pulls the COMMITTED block
hash at every height from every node and proves that no height is ever committed
with two different hashes across nodes. Any disagreement is a SAFETY VIOLATION
(a fork) and is reported loudly with per-node hashes; the process exits non-zero.

AUTHORITATIVE HASH SOURCE (verified in the think-dev worktree):
  * torus-consensus/src/app.rs:207  block_hash = keccak256(header.canonical_header_bytes())
    persisted as block_hash(32) || header_json in CF_BLOCK_HEADERS at commit time.
  * torus-rpc/src/eth.rs:102-114    get_header_with_hash reads bytes[..32]; build_rpc_block
    returns  hash: hex_b256(hash).
  => eth_getBlockByNumber(hex(h), false)["hash"] IS the consensus commit hash at height h.
  => an uncommitted height returns JSON null (Option<RpcBlock> == None).

Because the hash is keccak over the canonical header (which chains parent_hash and
commits the block content), identical hashes at height h across nodes imply identical
ancestry — so per-height hash equality across the whole common range is a COMPLETE
fork check, not a heuristic.
  (parent_hash is actually chained into canonical_header_bytes as of the header-identity
  hardening; before that this ancestry assumption was aspirational, not enforced.)

SAFETY-FIRST POSTURE (a false PASS is worse than a crash):
  * A node that is supposed to be reachable but errors / is unparseable => FAIL,
    never silently skipped.
  * A height at or below a node's own head that returns null (a gap in its own
    committed prefix) => FAIL.
  * Any two nodes disagreeing on a height's hash => FAIL (FORK DETECTED).
  * Fewer than --min-heights heights agreed by all nodes => FAIL (not enough proof).

Exit codes: 0 = PASS, 2 = FORK / gap / internal-chain break, 3 = unreachable / insufficient data.
"""

import argparse
import json
import sys
import time
import urllib.request

TIMEOUT = 5.0


def _rpc(url, method, params, retries=3):
    """Single JSON-RPC call. Raises on transport error or a JSON-RPC error object."""
    last = None
    for attempt in range(retries):
        try:
            body = json.dumps(
                {"jsonrpc": "2.0", "method": method, "params": params, "id": 1}
            ).encode()
            req = urllib.request.Request(
                url, data=body, headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
                obj = json.load(r)
            if obj.get("error"):
                raise RuntimeError(f"rpc error {method}: {obj['error']}")
            return obj["result"]
        except Exception as e:  # noqa: BLE001 — deliberate: retry any transient failure
            last = e
            time.sleep(0.4 * (attempt + 1))
    raise RuntimeError(f"{method}({params}) failed after {retries} tries: {last}")


def head(url):
    return int(_rpc(url, "eth_blockNumber", []), 16)


def get_block(url, h):
    """Return (hash, parent_hash) for height h, or None if the height is uncommitted."""
    b = _rpc(url, "eth_getBlockByNumber", [hex(h), False])
    if b is None:
        return None
    hsh = b.get("hash")
    if not hsh:
        # A committed height MUST carry a hash; a missing field is corruption, not "absent".
        raise RuntimeError(f"height {h}: committed block has no 'hash' field: {b!r}")
    ph = b.get("parentHash") or b.get("parent_hash")
    return (hsh, ph)


def main():
    ap = argparse.ArgumentParser(description="n=4/f=1 committed-hash fork detector")
    ap.add_argument(
        "--rpc-urls",
        required=True,
        help="comma-separated validator JSON-RPC URLs (label optional as label=url)",
    )
    ap.add_argument(
        "--from", dest="frm", type=int, default=1, help="first height to check"
    )
    ap.add_argument(
        "--to",
        dest="to",
        type=int,
        default=0,
        help="last height to check (0 = each node's own head)",
    )
    ap.add_argument(
        "--min-heights",
        type=int,
        default=10,
        help="minimum heights that ALL nodes must agree on for a PASS",
    )
    ap.add_argument("--out", default="", help="optional path to also write the report")
    args = ap.parse_args()

    # Parse endpoints, allowing "label=url" or bare "url".
    nodes = []
    for i, tok in enumerate(args.rpc_urls.split(",")):
        tok = tok.strip()
        if not tok:
            continue
        if "=" in tok and not tok.split("=", 1)[1].strip().startswith("//"):
            label, url = tok.split("=", 1)
        else:
            label, url = f"v{i}", tok
        nodes.append((label.strip(), url.strip()))
    if len(nodes) < 2:
        print("FATAL: need >= 2 RPC URLs to cross-check", file=sys.stderr)
        sys.exit(3)

    out_lines = []

    def emit(s=""):
        print(s)
        out_lines.append(s)

    emit("=" * 72)
    emit("T1.2 FORK CHECK — committed block-hash cross-validation (n=%d)" % len(nodes))
    emit("=" * 72)

    # ---- 1. heads (an unreachable node here is a hard FAIL: we cannot verify it) ----
    heads = {}
    for label, url in nodes:
        try:
            heads[label] = head(url)
        except Exception as e:  # noqa: BLE001
            emit("FATAL: node %s (%s) unreachable at check time: %s" % (label, url, e))
            _flush(args.out, out_lines)
            sys.exit(3)
    max_head = max(heads.values())
    min_head = min(heads.values())
    scan_to = args.to if args.to > 0 else max_head
    emit("heads: " + ", ".join("%s=%d" % (l, heads[l]) for l, _ in nodes))
    emit(
        "min_head=%d  max_head=%d  head_spread=%d"
        % (min_head, max_head, max_head - min_head)
    )
    emit("scanning heights [%d .. %d]" % (args.frm, scan_to))
    emit("")

    # ---- 2. pull every committed hash from every node ----
    # hashes[label][h] = block hash ; a null below a node's own head is a gap (FAIL).
    hashes = {label: {} for label, _ in nodes}
    parents = {label: {} for label, _ in nodes}
    gaps = {label: [] for label, _ in nodes}
    for label, url in nodes:
        node_top = min(heads[label], scan_to)
        for h in range(args.frm, node_top + 1):
            try:
                res = get_block(url, h)
            except Exception as e:  # noqa: BLE001 — unparseable => FAIL, never skip
                emit("FATAL: node %s height %d could not be read: %s" % (label, h, e))
                _flush(args.out, out_lines)
                sys.exit(2)
            if res is None:
                gaps[label].append(
                    h
                )  # <= own head yet absent => hole in committed prefix
            else:
                hashes[label][h], parents[label][h] = res

    # ---- 3. per-node internal chain continuity (parent_hash of h == hash of h-1) ----
    chain_breaks = []
    for label, _ in nodes:
        for h in sorted(hashes[label]):
            if h - 1 in hashes[label]:
                ph = parents[label].get(h)
                if ph is not None and ph.lower() != hashes[label][h - 1].lower():
                    chain_breaks.append((label, h, ph, hashes[label][h - 1]))

    # ---- 4. cross-node agreement at every height ----
    forks = []
    common = []
    all_labels = [l for l, _ in nodes]
    for h in range(args.frm, scan_to + 1):
        present = {l: hashes[l][h] for l in all_labels if h in hashes[l]}
        if not present:
            continue
        distinct = set(v.lower() for v in present.values())
        if len(distinct) > 1:
            forks.append((h, dict(present)))
        if len(present) == len(all_labels):
            common.append(h)
    max_common = max(common) if common else None

    # ---- 5. verdict ----
    fail = False

    if forks:
        fail = True
        emit("#" * 72)
        emit(
            "# !!! FORK DETECTED — %d height(s) committed with divergent hashes !!!"
            % len(forks)
        )
        emit("#" * 72)
        for h, present in forks[:50]:
            emit("  height %d:" % h)
            for l in all_labels:
                emit("    %-10s %s" % (l, present.get(l, "<absent>")))
        if len(forks) > 50:
            emit("  ... and %d more forked heights" % (len(forks) - 50))
        emit("")

    bad_gaps = {l: g for l, g in gaps.items() if g}
    if bad_gaps:
        fail = True
        emit(
            "!!! COMMITTED-PREFIX GAPS (height <= node head but returned null) — divergence/corruption:"
        )
        for l, g in bad_gaps.items():
            emit("    %-10s missing %d heights, e.g. %s" % (l, len(g), g[:15]))
        emit("")

    if chain_breaks:
        fail = True
        emit("!!! INTERNAL CHAIN BREAK (parent_hash mismatch within a single node):")
        for l, h, ph, prev in chain_breaks[:20]:
            emit(
                "    %-10s height %d parent=%s but %d.hash=%s" % (l, h, ph, h - 1, prev)
            )
        emit("")

    n_common = len(common)
    if n_common < args.min_heights:
        fail = True
        emit(
            "!!! INSUFFICIENT PROOF: only %d heights agreed by all %d nodes (need >= %d)"
            % (n_common, len(all_labels), args.min_heights)
        )
        emit("")

    # Head-spread advisory: a large spread AFTER catch-up is worth flagging, but on its
    # own it is lag/leadership, not a safety break, so it never forces FAIL by itself.
    if max_head - min_head > 20:
        emit(
            "WARN: head spread %d > 20 — a node is far behind; if paired with any gap/fork above this is a divergent chain"
            % (max_head - min_head)
        )

    emit("-" * 72)
    emit("per-node commit lag (max_head - node_head):")
    for l, _ in nodes:
        emit("    %-10s head=%d lag=%d" % (l, heads[l], max_head - heads[l]))
    emit("-" * 72)
    emit("heights checked (union range): %d" % (scan_to - args.frm + 1))
    emit("heights agreed by ALL nodes:   %d" % n_common)
    emit(
        "max common committed height:   %s"
        % (max_common if max_common is not None else "none")
    )
    emit("forked heights:                %d" % len(forks))
    emit("committed-prefix gaps:         %d" % sum(len(g) for g in gaps.values()))
    emit("internal chain breaks:         %d" % len(chain_breaks))
    emit("=" * 72)
    if fail:
        emit("RESULT: FAIL — consensus safety violated (see above)")
    else:
        emit("RESULT: PASS — all nodes agree on every committed height in range")
    emit("=" * 72)

    _flush(args.out, out_lines)
    if fail:
        sys.exit(2 if (forks or bad_gaps or chain_breaks) else 3)
    sys.exit(0)


def _flush(path, lines):
    if path:
        try:
            with open(path, "w") as f:
                f.write("\n".join(lines) + "\n")
        except OSError as e:
            print("WARN: could not write report to %s: %s" % (path, e), file=sys.stderr)


if __name__ == "__main__":
    main()
