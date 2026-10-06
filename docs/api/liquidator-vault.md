# `torus_getLiquidatorVault`

The liquidator vault (`LIQUIDATOR_VAULT`,
`0x746f7275732d6c697175696461746f722d766c74`, the ASCII bytes of
`torus-liquidator-vlt`) is a fixed protocol account with no known key. The
backstop moves a liquidated account's marked positions and remaining
collateral to it, and a flat account's negative collateral moves to it (D9),
so its cash can go negative. This method reads its committed
state (no params).

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getLiquidatorVault","params":[]}
```

Reply:

```json
{
  "address": "0x746f7275732d6c697175696461746f722d766c74",
  "availableBalance": "-200.00000000",
  "deficit": "200.00000000",
  "openPositions": 3
}
```

| Field | Type | Meaning |
|---|---|---|
| `address` | hex string | The vault address. |
| `availableBalance` | decimal string | Native available balance, **signed**, same format as `torus_getBalances` `availableBalance` (a negative value has a leading `-`). |
| `deficit` | decimal string | `-availableBalance` when it is negative, else `0.00000000`. Same definition as the `torus_liquidator_vault_deficit` gauge, but read from committed state, so it is also correct right after a restart. |
| `openPositions` | number | Positions with a non-zero size the vault holds (all markets). |

Rate-limit weight 2 (a cheap read, like `torus_getBalances`).
