#!/usr/bin/env python3
"""Fill the placeholders of a testnet genesis draft with DUMMY devnet values.

    fill-dummy.py <draft.json> <out.json> [--leave KEY]... [--list]

The draft (testnet/testnet-genesis-draft-{3,4,5}val.json) marks every value the
owner decides right before launch as the string "FILL:<key>". This script
replaces each one with an INSECURE devnet value (the deterministic devnet
consensus keys 01..05, the bench validator account keys, made-up treasury /
dev-pool / governance / faucet addresses, one BTC-USD market), so the file can
be loaded and booted by tools/genesis-draft/check.sh. Never use its output for
a real network.

"FILL:fee_split" (the open burn / validator / treasury shares; the draft writes
dev_pool 0 at start and end) is filled with FEE_SPLIT_DEV_ZERO, a DUMMY split
with the dev pool at 0 (burn / validator / treasury / dev_pool: start
1000/0/9000/0 -> end 2500/2500/5000/0). The loader reads economics.fee_split
since main 4c445f94, so the boot runs the real genesis fee-split path with the
owner's dev share of 0. The numbers are NOT an owner decision.

--leave KEY   keep "FILL:KEY" unfilled (check.sh proves the node refuses it).
--list        print the placeholder keys of the draft, one per line, and exit.

Exits 1 when the draft has a placeholder this script has no value for, or when
the output still contains a "FILL:" string that was not --leave'd.
"""

import json
import sys

PREFIX = "FILL:"

# Devnet ed25519 consensus pubkeys of the seeds 01.., 02.., ... (devnet/wsl/env.sh
# KEY0..; the 5th is seed 05..), and validator ACCOUNT addresses whose secp256k1
# keys check.sh holds (devnet/wsl/keys-4val/bench-validator-keys.json indices
# 0..3; the 5th is the private key 0x05 * 32). INSECURE: devnet only.
CONSENSUS_PUBKEYS = [
    "0xcecc1507dc1ddd7295951c290888f095adb9044d1b73d696e6df065d683bd4fc",
    "0x6b79c57e6a095239282c04818e96112f3f03a4001ba97a564c23852a3f1ea5fc",
    "0xdadbd184a2d526f1ebdd5c06fdad9359b228759b4d7f79d66689fa254aad8546",
    "0x9be3287795907809407e14439ff198d5bfc7dce6f9bc743cb369146f610b4801",
    "0xf4bd46521ce7b57899ae6f4ca09eddec689327a86a2232d4a3f2a4f39ac68a9e",
]
ACCOUNT_ADDRESSES = [
    "0x8c3229ec621644789d7f61faa82c6d0e5f97d43d",
    "0x9586a4833970847aef259ad5bfb7aa8901ddf746",
    "0x0e9971c0005d91336c1441b8f03c1c4fe5fb4584",
    "0x4548774216f19914493d051481feb56246bc13f0",
    "0xd09ad14080d4b257a819a4f579b8485be88f086c",
]
# 1M TRS each (devnet/genesis.json's stake): equal stakes.
STAKE = "1000000000000000000000000"

VALUES = {
    "chain_id": 7778,  # the devnet id: a dummy value, never the testnet's
    "timestamp": 1784678400,
    "treasury_address": "0xf000000000000000000000000000000000000001",
    "treasury_balance": "1000000000000000000000000000",
    "dev_pool_address": "0xf000000000000000000000000000000000000002",
    "dev_pool_balance": "500000000000000000000000000",
    "faucet_address": "0xf000000000000000000000000000000000000003",
    "faucet_balance": "1000000000000000000000000",
    # 100k TRS: enough to delegate the 1,000 TRS min_proposal_stake after launch.
    "gov_account_address": "0xf000000000000000000000000000000000000004",
    "gov_account_balance": "100000000000000000000000",
    "liquidator_vault_seed": "1000000.0",
}
for i, (pubkey, address) in enumerate(
    zip(CONSENSUS_PUBKEYS, ACCOUNT_ADDRESSES, strict=True)
):
    VALUES[f"validator{i}_address"] = address
    VALUES[f"validator{i}_pubkey"] = pubkey
    VALUES[f"validator{i}_stake"] = STAKE

# "FILL:markets" marks the whole markets list (every field of its one row).
MARKETS = [
    {
        "market_id": 1,
        "base_asset": "BTC",
        "quote_asset": "USD",
        "lot_size": "1.0",
        "tick_size": "1.0",
        "initial_margin": "5.0",
    }
]

# "FILL:fee_split" marks burn / validator / treasury of fee_split.start and .end.
# A dev-share-0 split, each side summing to 10000 bps as the loader requires
# (dummy numbers, NOT an owner decision: the destination split is open).
FEE_SPLIT_DEV_ZERO = {
    "start": {"burn": 1000, "validator": 0, "treasury": 9000, "dev_pool": 0},
    "end": {"burn": 2500, "validator": 2500, "treasury": 5000, "dev_pool": 0},
}
for side in FEE_SPLIT_DEV_ZERO.values():
    assert sum(side.values()) == 10000 and side["dev_pool"] == 0


def placeholders(value, found):
    if isinstance(value, str) and value.startswith(PREFIX):
        found.add(value[len(PREFIX) :])
    elif isinstance(value, dict):
        for v in value.values():
            placeholders(v, found)
    elif isinstance(value, list):
        for v in value:
            placeholders(v, found)
    return found


def fill(value, leave):
    if isinstance(value, str) and value.startswith(PREFIX):
        key = value[len(PREFIX) :]
        return value if key in leave else VALUES[key]
    if isinstance(value, dict):
        return {k: fill(v, leave) for k, v in value.items()}
    if isinstance(value, list):
        return [fill(v, leave) for v in value]
    return value


def main(argv):
    args = argv[1:]
    leave = set()
    listing = False
    rest = []
    while args:
        a = args.pop(0)
        if a == "--leave":
            leave.add(args.pop(0))
        elif a == "--list":
            listing = True
        else:
            rest.append(a)
    with open(rest[0], encoding="utf-8") as f:
        draft = json.load(f)
    keys = placeholders(draft, set())
    if listing:
        print("\n".join(sorted(keys)))
        return 0
    unknown = sorted(keys - set(VALUES) - {"markets", "fee_split"} - leave)
    if unknown:
        print(f"fill-dummy: no dummy value for {unknown}", file=sys.stderr)
        return 1
    if "markets" not in leave and any(
        isinstance(row.get("market_id"), str) for row in draft.get("markets", [])
    ):
        draft["markets"] = MARKETS
    if "fee_split" in keys and "fee_split" not in leave:
        for side, row in FEE_SPLIT_DEV_ZERO.items():
            draft["economics"]["fee_split"][side] = dict(row)
    out = fill(draft, leave)
    left = placeholders(out, set()) - leave
    if left:
        print(f"fill-dummy: still unfilled {sorted(left)}", file=sys.stderr)
        return 1
    with open(rest[1], "w", encoding="utf-8") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
