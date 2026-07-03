#!/usr/bin/env python3
"""D6 (S392): derive a single-validator, hardhat-funded genesis for the CI
1-node devnet from devnet/genesis.json.

Usage: make-ci-genesis.py <in-genesis.json> <out-genesis.json>
"""

import json
import sys


def main() -> None:
    src, dst = sys.argv[1], sys.argv[2]
    with open(src) as f:
        g = json.load(f)

    # Quorum of one: the CI devnet runs a single validator.
    g["validators"] = g["validators"][:1]

    # Fund the hardhat accounts deploy.sh / ci-swap-check.sh sign with.
    hardhat = [
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",  # deployer (account 0)
        "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",  # trader (account 1)
        "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC",  # spare (account 2)
    ]
    balance = str(10**24)  # 1M TRS each
    accounts = g.setdefault("accounts", [])
    existing = {a["address"].lower() for a in accounts}
    for addr in hardhat:
        if addr.lower() not in existing:
            accounts.append(
                {"address": addr, "balance": balance, "note": "CI hardhat account"}
            )

    with open(dst, "w") as f:
        json.dump(g, f, indent=1)
    print(f"wrote {dst}: 1 validator, {len(hardhat)} funded accounts")


if __name__ == "__main__":
    main()
