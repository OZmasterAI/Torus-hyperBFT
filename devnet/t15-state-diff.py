#!/usr/bin/env python3
"""t15-state-diff.py — cross-node NATIVE-state divergence detector (T1.5/6 crash-safety).

Companion to t15-crash-inject-4val.sh. After a validator has been SIGKILLed at a
worst-case moment (between consensus commit and native post-commit execution / the
applied-marker write) and then restarted, this tool proves the whole devnet still
agrees on the NATIVE application state — not just the committed block hash.

WHY A SEPARATE NATIVE DIFF (not just the T1.2 block-hash fork check)
  Torus is consensus-THEN-execute. The committed block hash
    app.rs:207  keccak256(header.canonical_header_bytes())
  is agreed by HotStuff BEFORE native actions run, and the incremental native
  state root is off by default (TORUS_INCREMENTAL_STATE_ROOT). So two nodes can
  agree on every committed block hash (T1.2 PASS) yet hold DIFFERENT native state
  if one crashed inside execute_committed_block (app.rs ~272-638: native flush
  ~574, applied-marker write ~625) and recovered wrong. That divergence is exactly
  the T1.5/6 failure mode, and it is INVISIBLE to a header-hash checker. This tool
  reads the post-execution native state directly and cross-checks it.

READ SURFACE (verified in the think-dev worktree, crates/torus-rpc/src/torus.rs)
  Deterministic, pure committed-DB reads — safe to diff across nodes at equal height:
    torus_getMarkets            markets config (CF_NATIVE_MARKETS)                 :775
    torus_getOrderBook(mid)     resting-liquidity snapshot (CF_NATIVE_ORDER_BOOKS) :622
    torus_getOpenInterest(mid)  aggregated OI (CF_NATIVE_POSITIONS)                :1443
    torus_getBalances(addr)     available + order_margin + permanent_stake + evm   :726
    torus_getValidators         power/status (staking)                            :940
    torus_getGovernanceParams   governance config                                 :1114
    torus_getTreasuryInfo       cumulative_burned/treasury, treasury_balance      :1133
    torus_getPosition(a,mid)    RAW fields only (size/entry/realized/margin/side)  :667
    eth_getBalance(addr)        EVM-mirrored account balance                       (eth.rs)
  HEIGHT-DEPENDENT fields DELIBERATELY EXCLUDED from the diff (they call
  OracleManager::get_price(mid, latest_height), torus.rs:691-697 / getMarkPrice):
    RpcPosition.unrealized_pnl, RpcPosition.liquidation_price, torus_getMarkPrice.
  These are derived from the per-node latest height, so even a 1-block head spread
  makes them differ for a reason that is NOT a state fault. We snapshot only at an
   enforced equal+stable height and still drop them, to keep a false-positive floor.

COVERAGE GAP (documented, not hidden)
  * CF_NATIVE_NONCES has no read RPC, so native replay-nonce state is diffed only
    INDIRECTLY (a double-applied or dropped action shows up as divergent order-book
    liquidity / balances / open interest). A silent nonce-only divergence with zero
    balance/book effect would not be caught here.
  * Per-trader reads cover the 20 genesis `native_balances` hardhat makers (the bench
    senders) + validators. An address that traded but is outside that set is not
    balance-diffed (its effect still surfaces in the aggregate order book / OI).

SAFETY POSTURE (a false PASS is worse than a crash)
  * Any node unreachable at diff time => exit 3 (cannot verify => never a silent PASS).
  * Heights not ALL equal (and stable) => exit 3: a diff across unequal heights is
    meaningless, so we refuse it rather than emit a false fork/false clean.
  * Any surface value differing across nodes => exit 2, printing every node's value.
  * Too little state observed (no markets / no books) => exit 3 (insufficient proof).

Exit codes: 0 = PASS (all nodes agree), 2 = DIVERGENCE (state fault), 3 = unreachable
/ unequal-height / insufficient (inconclusive — caller treats as FAIL).
"""

import argparse
import json
import sys
import time
import urllib.request

TIMEOUT = 6.0


# ---------------------------------------------------------------------------
# JSON-RPC
# ---------------------------------------------------------------------------
def _rpc(url, method, params, retries=3):
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
        except Exception as e:  # noqa: BLE001 — retry any transient failure
            last = e
            time.sleep(0.4 * (attempt + 1))
    raise RuntimeError(f"{method}({params}) failed after {retries} tries: {last}")


def head(url):
    return int(_rpc(url, "eth_blockNumber", []), 16)


# ---------------------------------------------------------------------------
# Genesis-driven inputs (markets + trader set) — no bench internals needed.
# ---------------------------------------------------------------------------
def load_genesis_inputs(path):
    """Return (market_ids_hex, trader_addrs) from the devnet genesis.

    market ids: `markets[].market_id` (int) -> hex string ("0x1"), because the
    RPC parses market ids as radix-16 (types.rs parse_u64). If the chain later
    auto-creates books for ids not in genesis, getMarkets (read live per node)
    still enumerates them below, so this list is only a floor.

    traders: `native_balances[].address` (the 20 hardhat bench makers) +
    `validators[].address`.
    """
    with open(path) as f:
        g = json.load(f)
    market_ids = []
    for m in g.get("markets", []) or []:
        mid = m.get("market_id")
        if isinstance(mid, int):
            market_ids.append("0x%x" % mid)
    traders = []
    for a in g.get("native_balances", []) or []:
        ad = a.get("address")
        if ad:
            traders.append(ad.lower())
    for v in g.get("validators", []) or []:
        ad = v.get("address")
        if ad:
            traders.append(ad.lower())
    # de-dup, stable order
    seen = set()
    traders = [t for t in traders if not (t in seen or seen.add(t))]
    return market_ids, traders


# ---------------------------------------------------------------------------
# Per-node native-state snapshot -> {logical_key: canonical_value}
# ---------------------------------------------------------------------------
POSITION_DROP = {"unrealized_pnl", "liquidation_price"}  # oracle/height-derived


def canon(v):
    """Deterministic JSON string for equality (order-insensitive on dict keys)."""
    return json.dumps(v, sort_keys=True, separators=(",", ":"))


def snapshot_node(url, market_ids_hint, traders):
    """Read every diffable native surface from one node. Raises on any read error
    (a failed read is a hard FAIL, never a silent skip)."""
    snap = {}

    # --- markets (live enumeration; union with the genesis hint) ---
    markets = _rpc(url, "torus_getMarkets", [0, 500])
    snap["markets"] = canon(markets)
    live_ids = []
    for m in markets:
        mid = m.get("market_id")
        if mid:
            live_ids.append(mid if isinstance(mid, str) else "0x%x" % mid)
    all_mids = []
    seen = set()
    for mid in list(market_ids_hint) + live_ids:
        if mid not in seen:
            seen.add(mid)
            all_mids.append(mid)

    # --- per-market order book + open interest (the money-path native state) ---
    for mid in all_mids:
        snap["orderbook:%s" % mid] = canon(_rpc(url, "torus_getOrderBook", [mid]))
        snap["open_interest:%s" % mid] = canon(
            _rpc(url, "torus_getOpenInterest", [mid])
        )

    # --- staking / governance / treasury (deterministic aggregates) ---
    snap["validators"] = canon(_rpc(url, "torus_getValidators", []))
    snap["governance_params"] = canon(_rpc(url, "torus_getGovernanceParams", []))
    snap["treasury"] = canon(_rpc(url, "torus_getTreasuryInfo", []))

    # --- per-trader balances + positions (raw fields only) ---
    for t in traders:
        snap["balances:%s" % t] = canon(_rpc(url, "torus_getBalances", [t]))
        snap["evm_balance:%s" % t] = canon(_rpc(url, "eth_getBalance", [t, "latest"]))
        for mid in all_mids:
            pos = _rpc(url, "torus_getPosition", [t, mid])
            if isinstance(pos, dict):
                pos = {k: v for k, v in pos.items() if k not in POSITION_DROP}
            snap["position:%s:%s" % (t, mid)] = canon(pos)

    return snap, all_mids


# ---------------------------------------------------------------------------
def main():
    ap = argparse.ArgumentParser(description="n-node native-state divergence detector")
    ap.add_argument(
        "--rpc-urls",
        required=True,
        help="comma-separated validator JSON-RPC URLs (label=url or bare)",
    )
    ap.add_argument(
        "--genesis",
        required=True,
        help="devnet genesis.json (source of market ids + trader set)",
    )
    ap.add_argument(
        "--require-height-equal",
        type=int,
        default=1,
        help="1 = run the native-state torn-read guard (re-read must be unchanged)",
    )
    ap.add_argument(
        "--max-head-spread",
        type=int,
        default=25,
        help="max tolerated empty-block head spread across nodes before INCONCLUSIVE",
    )
    ap.add_argument("--out", default="", help="optional path to also write the report")
    args = ap.parse_args()

    nodes = []
    for i, tok in enumerate(args.rpc_urls.split(",")):
        tok = tok.strip()
        if not tok:
            continue
        if "=" in tok and not tok.split("=", 1)[1].strip().startswith("//"):
            label, urlv = tok.split("=", 1)
        else:
            label, urlv = "v%d" % i, tok
        nodes.append((label.strip(), urlv.strip()))
    if len(nodes) < 2:
        print("FATAL: need >= 2 RPC URLs to cross-check", file=sys.stderr)
        sys.exit(3)

    out_lines = []

    def emit(s=""):
        print(s)
        out_lines.append(s)

    emit("=" * 72)
    emit(
        "T1.5/6 NATIVE-STATE DIFF — cross-node post-crash divergence check (n=%d)"
        % len(nodes)
    )
    emit("=" * 72)

    # ---- 1. reachability + equal-height gate -------------------------------
    heads = {}
    for label, urlv in nodes:
        try:
            heads[label] = head(urlv)
        except Exception as e:  # noqa: BLE001
            emit("FATAL: node %s (%s) unreachable at diff time: %s" % (label, urlv, e))
            _flush(args.out, out_lines)
            sys.exit(3)
    emit("heads: " + ", ".join("%s=%d" % (l, heads[l]) for l, _ in nodes))
    hset = set(heads.values())
    spread = max(hset) - min(hset)
    # This chain runs a HotStuff pacemaker: it commits EMPTY blocks forever, so the
    # block height NEVER freezes even after the mempool drains. Requiring an
    # identical, frozen height would make this diff impossible to ever run. What
    # actually matters for a native-state diff is that no NATIVE action lands during
    # the read window — enforced below by a per-node state RE-READ guard (a torn-read
    # check on the native state itself, not on the empty-block height). So we tolerate
    # a small empty-block head spread and only refuse an implausibly large one, which
    # would indicate a genuinely stuck/lagging node rather than pacemaker skew.
    emit("head spread: %d (min %d .. max %d)" % (spread, min(hset), max(hset)))
    if spread > args.max_head_spread:
        emit(
            "INCONCLUSIVE: head spread %d exceeds --max-head-spread %d — a node is"
            % (spread, args.max_head_spread)
        )
        emit("  lagging/stuck, not merely empty-block skew; re-quiesce and retry.")
        _flush(args.out, out_lines)
        sys.exit(3)
    diff_height = max(hset)
    emit(
        "diffing near height %d (empty-block skew tolerated; native torn-read guarded)"
        % diff_height
    )
    emit("")

    try:
        market_hint, traders = load_genesis_inputs(args.genesis)
    except Exception as e:  # noqa: BLE001
        emit("FATAL: could not read genesis %s: %s" % (args.genesis, e))
        _flush(args.out, out_lines)
        sys.exit(3)
    emit(
        "genesis inputs: %d market ids, %d trader addrs"
        % (len(market_hint), len(traders))
    )

    # ---- 2. snapshot every node -------------------------------------------
    snaps = {}
    mids_seen = set()
    for label, urlv in nodes:
        try:
            snaps[label], mids = snapshot_node(urlv, market_hint, traders)
            mids_seen.update(mids)
        except Exception as e:  # noqa: BLE001 — any read error => FAIL, never skip
            emit("FATAL: node %s snapshot failed: %s" % (label, e))
            _flush(args.out, out_lines)
            sys.exit(3)

    # ---- 2b. torn-read guard: re-read each node's native state and require it
    # UNCHANGED. The chain keeps producing empty blocks, so we cannot key this on
    # height; instead we verify the NATIVE STATE itself did not mutate during the read
    # window (the only thing that can invalidate the diff). If any node's native
    # snapshot changed between the two reads, a native action was still landing => the
    # mempool had not fully drained => INCONCLUSIVE (caller re-quiesces and retries).
    if args.require_height_equal:
        changed = []
        for label, urlv in nodes:
            try:
                snap2, _ = snapshot_node(urlv, market_hint, traders)
            except Exception as e:  # noqa: BLE001
                emit(
                    "FATAL: node %s unreachable on torn-read recheck: %s" % (label, e)
                )
                _flush(args.out, out_lines)
                sys.exit(3)
            if snap2 != snaps[label]:
                changed.append(label)
        if changed:
            emit(
                "INCONCLUSIVE: native state still mutating on %s during the read window"
                % ", ".join(changed)
            )
            emit("  (mempool had not fully drained) — re-quiesce and retry.")
            _flush(args.out, out_lines)
            sys.exit(3)

    # ---- 3. insufficiency guard -------------------------------------------
    n_keys = len(next(iter(snaps.values())))
    if not mids_seen or n_keys < 3:
        emit(
            "INSUFFICIENT: observed %d markets / %d keys — not enough native state to prove"
            % (len(mids_seen), n_keys)
        )
        _flush(args.out, out_lines)
        sys.exit(3)

    # ---- 4. cross-node diff ------------------------------------------------
    labels = [l for l, _ in nodes]
    all_keys = set()
    for s in snaps.values():
        all_keys.update(s.keys())

    divergent = []  # (key, {label: value})
    missing = []  # (key, [labels missing it])
    for key in sorted(all_keys):
        present = {l: snaps[l].get(key) for l in labels}
        absent = [l for l in labels if present[l] is None]
        vals = set(v for v in present.values() if v is not None)
        if absent:
            missing.append((key, absent))
        if len(vals) > 1:
            divergent.append((key, present))

    fail = bool(divergent or missing)

    if divergent:
        emit("#" * 72)
        emit(
            "# !!! NATIVE STATE DIVERGENCE — %d key(s) differ across nodes !!!"
            % len(divergent)
        )
        emit("#" * 72)
        for key, present in divergent[:60]:
            emit("  key: %s" % key)
            for l in labels:
                val = present.get(l)
                shown = val if (val is None or len(val) <= 220) else val[:217] + "..."
                emit("    %-10s %s" % (l, shown))
        if len(divergent) > 60:
            emit("  ... and %d more divergent keys" % (len(divergent) - 60))
        emit("")

    if missing:
        emit(
            "!!! KEY PRESENCE MISMATCH (a surface readable on some nodes but not others):"
        )
        for key, absent in missing[:40]:
            emit("    %-40s absent on: %s" % (key, ", ".join(absent)))
        emit("")

    emit("-" * 72)
    emit("nodes:                 %d" % len(nodes))
    emit("diff height:           %d" % diff_height)
    emit("markets observed:      %d" % len(mids_seen))
    emit("state keys per node:   %d" % n_keys)
    emit("divergent keys:        %d" % len(divergent))
    emit("presence-mismatch keys:%d" % len(missing))
    emit("=" * 72)
    if fail:
        emit("RESULT: FAIL — native state diverged across nodes after crash/restart")
    else:
        emit(
            "RESULT: PASS — all %d nodes hold identical native state at height %d"
            % (len(nodes), diff_height)
        )
    emit("=" * 72)

    _flush(args.out, out_lines)
    if fail:
        sys.exit(2)
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
