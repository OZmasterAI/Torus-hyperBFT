# `torus_getBalances`

One account's balances and its cross-margin figures: the numbers the engine
checks orders, withdrawals and liquidation against. Read from committed state
(param: the account address).

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getBalances","params":["0x5555555555555555555555555555555555555555"]}
```

Reply (cash 1000, a cross long of 10 at 1000 marked at 1010, default 20x):

```json
{
  "nativeBalance": "1000.00000000",
  "evmBalance": "0x0",
  "accountValue": "1100.00000000",
  "totalMarginUsed": "505.00000000",
  "maintenanceMarginUsed": "252.50000000",
  "cashBalance": "1000.00000000",
  "withdrawable": "90.00000000",
  "freeMargin": "595.00000000",
  "permanentStake": "0x0"
}
```

Decimal strings have 8 decimals and are **signed** (a negative value has a
leading `-`). `UPnL` is unrealized PnL, `IM` initial margin, `MM` maintenance
margin, each position at its position-size tier.

| Field | Type | Meaning | HL equivalent |
|---|---|---|---|
| `nativeBalance` | decimal string | Native collateral: cash + order margin (the resting orders' reservations). No UPnL. | – |
| `evmBalance` | hex string | EVM balance (wei). | – |
| `accountValue` | decimal string | Equity: cash + order margin + Σ UPnL. | `crossMarginSummary.accountValue` |
| `totalMarginUsed` | decimal string | Margin in use: order margin + Σ cross position IM + Σ isolated margin. | `crossMarginSummary.totalMarginUsed` |
| `maintenanceMarginUsed` | decimal string | Σ cross position MM, valued as the liquidation check values it. The account is liquidated when `accountValue < maintenanceMarginUsed`. | `crossMaintenanceMarginUsed` |
| `cashBalance` | decimal string | Ledger cash (the balance row's `available`). Can go negative: unrealized profit funds reservations, so cash can be below 0 while the account is healthy. Was `availableBalance`. | – |
| `withdrawable` | decimal string | The largest amount a withdrawal (`TransferToSpot`, `Withdraw`) is accepted for: `min(cash, cash + Σ UPnL − max(Σ position IM, 10% × Σ position notional))`, 0 if negative. | `withdrawable` |
| `freeMargin` | decimal string | The pool a new order's IM need is checked against: cash + Σ UPnL − Σ cross position IM (= `accountValue − totalMarginUsed` with no isolated position). An order is accepted iff its need is `<= 0` (it only reduces) or `<= freeMargin`. Can be negative. | none in `clearinghouseState`; HL's planned `availableBalance` (below) |
| `permanentStake` | hex string | Permanent stake. | – |

How the figures are valued (the executor's rules, on the same rows):
- A position is valued at the market's mark: the oracle aggregate while usable
  (fresh within 60 s of block time, positive), else at its entry price (UPnL 0,
  IM and MM still count). The block time is the executed head's, so a mark that
  goes stale at the next block still counts here.
- `maintenanceMarginUsed` marks only **listed** markets, as the liquidation
  check does; `accountValue`, `freeMargin` and `withdrawable` use every usable
  mark, as the placement and withdraw checks do. They differ only for a
  position in a delisted market whose aggregate is still fresh (up to 60 s).
- Each market's tiers come from its market row (no row or an undecodable row:
  the default 20x).
- `freeMargin` is the single-order pool. Inside one batch the engine also
  charges the sender's earlier orders of the same batch; a balance read sees
  only committed state.

Cost: the balance row, a prefix scan of the account's positions, and per
market it holds one market row and one oracle row, plus the head header. Only
RocksDB reads: no lock or structure the executor uses during a block (resident
books, sums cache, exec pipeline) is touched, so a balance call cannot delay a
block. Every native field comes from one read of the balance row; the
positions and oracle rows are separate point reads (no DB snapshot), as in
the other `torus_*` reads. Rate-limit weight 2.

## Planned upgrade

The full HL-shaped response is a likely future upgrade: `marginSummary` and
`crossMarginSummary` (each `{ accountValue, totalNtlPos, totalRawUsd,
totalMarginUsed }`), `crossMaintenanceMarginUsed`, `withdrawable`, and
`availableBalance` as free margin (today's `freeMargin`). It is the same
per-call cost class: it reuses the same mark-price reads.
