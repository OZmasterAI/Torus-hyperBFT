#!/usr/bin/env python3
"""Small counterexample models for the cea1254 chain audit.

These execute Python models, NOT the Rust node. Source guards make the
relationship to the audited implementation explicit; they are not a Rust
parser, a consensus simulator, or end-to-end regression tests.
Run from any directory: python docs/audits/chain-cea1254-models.py
"""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def source_guard(relative, *snippets):
    source = (ROOT / relative).read_text()
    for snippet in snippets:
        if snippet not in source:
            raise RuntimeError(f"Audited source changed: {relative}: {snippet!r}")


def session_expiry():
    source_guard("crates/torus-consensus/src/app.rs",
                 "&torus_block.native_actions,\n                    torus_block.header.timestamp,")
    source_guard("crates/torus-types/src/eip712.rs", "timestamp <= session.expiry")
    source_guard("crates/torus-bridge/src/native_executor.rs",
                 "let now_ms = ctx.timestamp.saturating_mul(1000);")
    created_seconds = 1_791_100_000
    expiry_ms = (created_seconds + 60) * 1000
    block_seconds = created_seconds + 120
    observed = block_seconds <= expiry_ms
    required = block_seconds * 1000 <= expiry_ms
    assert observed and not required
    return {"block_seconds": block_seconds, "expiry_ms": expiry_ms,
            "current_execution_accepts": observed, "correct_units_accept": required}


def fee_conservation():
    source_guard("crates/torus-bridge/src/proposer.rs",
                 "r.gas_used as u128 * r.effective_gas_price as u128")
    source_guard("crates/torus-economics/src/types.rs",
                 "FEE_START_BURN_BPS: u16 = 1000",
                 "FEE_START_TREASURY_BPS: u16 = 4500",
                 "FEE_START_DEV_POOL_BPS: u16 = 4500")
    source_guard("crates/torus-consensus/src/app.rs",
                 "overlay.seed_from_bundle(&bundle)",
                 "NativeExecutor::distribute_fees(&mut ctx, computed_fee_revenue)")
    # revm-handler 17.0.0 post_execution.rs:69-88 pays this priority fee.
    gas, base_fee, effective_price = 21_000, 1_000_000_000, 2_000_000_000
    charged = gas * effective_price
    tip = gas * (effective_price - base_fee)
    burn = charged * 1000 // 10_000
    redistributed = charged - burn
    net_supply_delta = -charged + tip + redistributed
    assert net_supply_delta > 0
    assert net_supply_delta - (-burn) == tip
    return {"charged_wei": charged, "revm_tip_wei": tip,
            "torus_credits_wei": redistributed,
            "net_supply_delta_wei": net_supply_delta,
            "scheduled_supply_delta_wei": -burn}


def adl_scan_starvation():
    source_guard("crates/torus-core/src/liquidation.rs",
                 "ADL_MAX_SCAN_ROWS: usize = 65_536",
                 "let mut start: Vec<u8> = Vec::new();",
                 "while rows.len() < max_rows",
                 "k[20..28] != m.to_be_bytes()")
    cap = 65_536
    # Rows are in trader/market order. All eligible market-2 counterparties
    # follow a stable prefix of market-1 positions at lower trader addresses.
    rows = [(i, 1) for i in range(cap)] + [(cap, 2)]
    reached_per_retry = []
    for _ in range(3):
        candidates = [r for r in rows[:cap] if r[1] == 2]
        reached_per_retry.append(len(candidates))
    assert reached_per_retry == [0, 0, 0]
    assert any(market == 2 for _, market in rows)
    return {"prefix_rows": cap, "eligible_counterparties": 1,
            "counterparties_found_on_retries": reached_per_retry}


def inherited_epoch():
    source_guard("crates/torus-consensus/src/app.rs", "epoch: parent_header.epoch")
    source_guard("crates/torus-bridge/src/native_executor.rs",
                 "total_fees,\n            ctx.epoch,")
    source_guard("crates/torus-bridge/src/proposer.rs", "epoch: 0,")
    epoch_length, parent_epoch = 4, 0
    headers = []
    for height in range(1, 13):
        child_epoch = parent_epoch
        headers.append((height, child_epoch))
        parent_epoch = child_epoch
    assert all(epoch == 0 for _, epoch in headers)
    assert headers[-1][1] != headers[-1][0] // epoch_length
    return {"height": 12, "epoch_length": epoch_length,
            "header_fee_epoch": headers[-1][1], "height_epoch": 3}


if __name__ == "__main__":
    results = {f.__name__: f() for f in
               [session_expiry, fee_conservation, adl_scan_starvation, inherited_epoch]}
    print(json.dumps({"kind": "counterexample models, not Rust runtime tests",
                      "violated_invariants": results}, indent=2))
