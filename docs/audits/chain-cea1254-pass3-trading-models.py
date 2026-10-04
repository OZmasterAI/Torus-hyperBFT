#!/usr/bin/env python3
"""Source-guarded arithmetic/interface models; these DO NOT execute Torus Rust."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
REV = "cea1254e34625e6b09c58f794de8793b5c12713c"
assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip() == REV

def guard(path, *snippets):
    source = (ROOT / path).read_text()
    for snippet in snippets:
        assert snippet in source, (path, snippet)

guard("crates/torus-core/src/order_book.rs",
      ".checked_add(price.checked_mul(q - closing).ok()?)",
      "Some(d) if closing == q || d <= a.free =>")
guard("crates/torus-core/src/liquidation.rs", "px.min(b)",
      "tb.available = tb.available.checked_add(c).map_err(of)?;")
guard("crates/torus-bridge/src/liquidation_step.rs",
      "Self::adl_account(ctx, &marks, &prev, &trader)?",
      "liq::move_collateral(&ctx.positions, u, &LIQUIDATOR_VAULT)?;",
      "|| !ps.iter().any(|p| marks.contains_key(&p.market_id))")
guard("crates/torus-core/src/precompiles.rs",
      "let order_id: u128 = ((current_block + 1) as u128) << 64 | seq as u128;",
      "// OrderType: 0=Limit, 1=Market, 2=StopMarket, 3=StopLimit")
guard("crates/torus-bridge/src/native_executor.rs",
      "book.set_next_order_id(forced_id.unwrap_or(ctx.next_global_order_id));",
      "Some(action) => Self::execute(ctx, &qa.trader, &action),",
      "0 => OrderType::Limit,\n        1 => OrderType::Market,\n        _ => OrderType::Limit,")

# Values are raw eight-decimal native units; size is one base-asset unit.
S = 100_000_000
cash_a = cash_b = 60 * S
mark = previous_mark = 100 * S
fill = 1_000 * S
quantity = S
lev = 20
mul = lambda a, b: a * b // S  # Every model multiplication is exact here.
reservation_a = mul(mark, quantity) // lev
reservation_b = mul(fill, quantity) // lev
assert reservation_a == 5 * S and reservation_b == 50 * S
assert reservation_b <= cash_b  # B placement need 50 fits collateral 60.
maker_delta = reservation_b - reservation_b
assert maker_delta == 0 and maker_delta <= cash_b - reservation_b
taker_need = mul(fill, quantity) // lev
taker_budget = reservation_a + (cash_a - reservation_a)
assert taker_need == 50 * S and taker_need <= taker_budget
equity_a = cash_a + mul(mark - fill, quantity)
equity_b = cash_b + mul(fill - mark, quantity)
assert equity_a == -840 * S and equity_b == 960 * S
bankruptcy = fill - cash_a * S // quantity
adl_price = min(previous_mark, bankruptcy)
assert bankruptcy == 940 * S and adl_price == 100 * S
realized_a = mul(adl_price - fill, quantity)
realized_b = -realized_a
cash_a += realized_a
cash_b += realized_b
assert cash_a == -840 * S and cash_b == 960 * S
vault = cash_a
cash_a = 0  # Move signed collateral to vault after closing both positions.
assert cash_a + cash_b + vault == 120 * S  # Signed accounting still conserves.
assert cash_b - cash_b >= 0  # Flat B withdrawal leaves required IM/notional = 0.
withdrawal_wei = cash_b * 10_000_000_000
initial_collateral_wei = 120 * 10**18
assert withdrawal_wei - initial_collateral_wei == 840 * 10**18
print("T01 MODEL: A=0, B=960, flat vault=-840 native; withdraw960 vs collateral120 (net840).")

returned_id = (21 << 64) | 0
actual_id = 1  # Empty books; first scalar placement allocates global id 1.
assert returned_id == 387381625547900583936 and returned_id != actual_id
print(f"T02 MODEL: CoreWriter return={returned_id}, actual resting order={actual_id}; cancel misses.")
decode = lambda code: "Market" if code == 1 else "Limit"
assert decode(2) == decode(3) == "Limit"
print("T03 MODEL: accepted StopMarket(2)/StopLimit(3) both drain as immediately active Limit.")
