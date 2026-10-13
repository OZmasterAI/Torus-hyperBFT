# `torus_getPosition`

One account's position in one market, read from committed state
(params: the account address, the market id as hex).

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getPosition","params":["0x5555555555555555555555555555555555555555","0x1"]}
```

Reply (cash 1000, a cross long of 10 at 1000 marked at 1010, default 20x),
or `null` when the account holds no position in the market:

```json
{
  "marketId": "0x1",
  "side": "long",
  "size": "10.00000000",
  "entryPrice": "1000.00000000",
  "unrealizedPnl": "100.00000000",
  "realizedPnl": "0.00000000",
  "margin": "0.00000000",
  "marginMode": "cross",
  "liquidationPrice": "923.07692308"
}
```

Decimal strings have 8 decimals and are signed.

| Field | Type | Meaning |
|---|---|---|
| `marketId` | hex string | The market id as requested. |
| `side` | `"long"` / `"short"` | Position direction. |
| `size` | decimal string | Absolute size. |
| `entryPrice` | decimal string | Average entry price. |
| `unrealizedPnl` | decimal string | PnL at the usable mark (oracle aggregate fresh within 60 s of the executed head's block time), 0 without one. |
| `realizedPnl` | decimal string | Realized PnL of the position. |
| `margin` | decimal string | Isolated margin (0 for cross). |
| `marginMode` | `"cross"` / `"isolated"` | Margin mode. |
| `liquidationPrice` | decimal string or `null` | See below. |

## `liquidationPrice`

**Cross** (every production position): the mark price of this market at
which the account's equity (`accountValue` in `torus_getBalances`) equals its
maintenance margin (`maintenanceMarginUsed`), **every other market's mark
held at its current value**. It is the account-wide threshold the
liquidation step checks (`equity < maintenance` liquidates), computed from
the engine's own account valuation (`AccountView` + `liquidation::classify`)
at the executed head's block time, not an approximation:

- long: the lowest mark at which the account is still healthy; at one raw
  unit (1e-8) below it the account is liquidatable;
- short: the highest mark at which it is still healthy; one raw unit above,
  liquidatable.

Other markets are valued as the liquidation step values them: listed markets
at their usable mark, otherwise at entry. The price therefore moves when
other positions' marks, cash, resting-order margin or other positions
change; it is one market's shock, not a portfolio scenario. With several
positions, each position's price assumes only its own market moves.

`null` when no mark of this market crosses maintenance:
- the account stays healthy even at mark 0 (e.g. a long whose cost is fully
  covered by cash);
- the account is liquidatable at every mark of this market (e.g. a short
  whose equity is negative even at mark 0, because of other positions or
  negative cash);
- the market is not listed (the liquidation step never marks it, so its
  price cannot trigger liquidation);
- the account also holds an isolated position (the liquidation step does
  not value such an account);
- no crossing inside the i128 price range `[0, i128::MAX]` raw;
- the crossing cannot be bracketed by marks the engine can value: a mark
  whose valuation overflows i128 is skipped by the liquidation step, so it
  counts as neither healthy nor liquidatable. `null` when mark 0 overflows,
  or when every mark the engine can value is on one side (e.g. liquidatable)
  and the marks beyond overflow. A returned price is always a mark the
  engine values as healthy next to one (1e-8 away) it values as
  liquidatable.

`null` never means "no risk" by itself when other positions exist: a shock
in another market can still liquidate the account. Read
`torus_getBalances` for the account-level figures.

**Isolated**: the simplified estimate `entry ∓ margin / size` (0 without
isolated margin), unchanged. Isolated positions are not created in
production.

Cost: the cross price reads the account's balance, positions, one market row
and one oracle row per market it holds and the head header (the
`torus_getBalances` reads), then values the account at up to ~250 candidate
marks in memory (bisection on raw units; O(positions) each). RocksDB reads
only; no executor lock or structure. Rate-limit weight 2.
