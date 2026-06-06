#!/usr/bin/env python3
"""native-transfer-probe.py — PROVE duplicate native execution (correctness bug).

Submits ONE signed TransferToPerp native action from a fresh hardhat account and
measures:
  (a) how many times it EXECUTED  -> native availableBalance delta / amount
  (b) how many blocks INCLUDED it -> blocks with nativeActionCount>0 in the window
      (on an otherwise-quiescent devnet, every such block is OUR single action).

A correct chain shows 1x (executed once). The duplicate-inclusion bug shows ~3x.

The EIP-712 signer is SELF-TESTED against a pinned known vector
(crates/torus-types/tests/fixtures/eip712_vectors.json, label "TransferToPerp")
before anything is submitted, so a wrong signer can never produce a false result.

Usage:
  python3 native-transfer-probe.py selftest                  # just validate the signer
  python3 native-transfer-probe.py [rpc_url] [acct_idx] [amount_raw]
"""

import json
import sys
import time

import requests
from Crypto.Hash import keccak as keccak_mod
from eth_keys import KeyAPI

# ---------------------------------------------------------------------------
# EIP-712 signing for TransferToPerp(uint256 amount,uint64 nonce)
# ---------------------------------------------------------------------------
DOMAIN_SEPARATOR = bytes.fromhex(
    "c17bc08f8d2e5d76651f1d3a5c156a7cfc34b56bb732b24997bfc22da5f4a257"
)


def keccak256(data: bytes) -> bytes:
    h = keccak_mod.new(digest_bits=256)
    h.update(data)
    return h.digest()


def uint256(v: int) -> bytes:
    return v.to_bytes(32, "big")


TTP_TYPEHASH = keccak256(b"TransferToPerp(uint256 amount,uint64 nonce)")


def ttp_struct_hash(amount: int, nonce: int) -> bytes:
    # mirrors crates/torus-types/src/eip712.rs hash_transfer_to_perp:
    #   keccak256(typehash || encode_u256(amount) || encode_u64(nonce))
    # encode_u256 and encode_u64 both emit 32-byte big-endian.
    return keccak256(TTP_TYPEHASH + uint256(amount) + uint256(nonce))


def signing_hash(struct_hash: bytes) -> bytes:
    return keccak256(b"\x19\x01" + DOMAIN_SEPARATOR + struct_hash)


def sign_ttp(pk: KeyAPI.PrivateKey, amount: int, nonce: int):
    sh = ttp_struct_hash(amount, nonce)
    mh = signing_hash(sh)
    sig = pk.sign_msg_hash(mh)
    v = sig.v + 27
    r = sig.r.to_bytes(32, "big")
    s = sig.s.to_bytes(32, "big")
    envelope = {
        "action": {"TransferToPerp": {"amount": hex(amount)}},
        "nonce": nonce,
        "signature": {"Eip712": {"v": v, "r": list(r), "s": list(s)}},
    }
    payload = "0x" + json.dumps(envelope, separators=(",", ":")).encode().hex()
    return payload, sh, mh


# Pinned known-answer vector (eip712_vectors.json, label "TransferToPerp").
_VEC_AMOUNT = 0xE8D4A51000
_VEC_NONCE = 1700000000000
_VEC_STRUCT = "36ba8cd3f5605c354e5db164c283ae7b767b23ff55792346a5a307cbfa2a1e6f"
_VEC_SIGNING = "2d9a3a02dc90a084973cb5d23919bac4ef38e690151612420a74ca99cb5ca6d9"
_VEC_SIGNER = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
_VEC_R = [
    214,
    243,
    173,
    11,
    45,
    55,
    37,
    111,
    195,
    192,
    80,
    210,
    126,
    120,
    17,
    223,
    50,
    76,
    93,
    48,
    140,
    127,
    225,
    58,
    233,
    71,
    255,
    200,
    102,
    64,
    244,
    23,
]
_VEC_S = [
    1,
    113,
    31,
    134,
    176,
    73,
    161,
    40,
    121,
    55,
    4,
    67,
    213,
    85,
    105,
    68,
    46,
    127,
    144,
    145,
    70,
    232,
    145,
    100,
    188,
    186,
    209,
    191,
    77,
    254,
    239,
    157,
]


def selftest() -> None:
    pk = KeyAPI().PrivateKey((1).to_bytes(32, "big"))
    payload, sh, mh = sign_ttp(pk, _VEC_AMOUNT, _VEC_NONCE)
    assert pk.public_key.to_address() == _VEC_SIGNER, pk.public_key.to_address()
    assert sh.hex() == _VEC_STRUCT, f"struct_hash {sh.hex()}"
    assert mh.hex() == _VEC_SIGNING, f"signing_hash {mh.hex()}"
    sig = pk.sign_msg_hash(mh)
    assert sig.v + 27 == 28, sig.v
    assert list(sig.r.to_bytes(32, "big")) == _VEC_R, "r mismatch"
    assert list(sig.s.to_bytes(32, "big")) == _VEC_S, "s mismatch"
    log("[selftest] PASS — TransferToPerp signer reproduces the pinned vector")
    log(f"           struct_hash  = 0x{sh.hex()}")
    log(f"           signing_hash = 0x{mh.hex()}")
    log(f"           signer       = {_VEC_SIGNER}")


# ---------------------------------------------------------------------------
# RPC
# ---------------------------------------------------------------------------
def log(*a):
    print(*a, flush=True)


def rpc(method, params, url):
    body = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    return requests.post(url, json=body, timeout=5).json()


def block_number(url):
    return int(rpc("eth_blockNumber", [], url)["result"], 16)


def get_balances(addr, url):
    r = rpc("torus_getBalances", [addr], url)["result"]
    return {k: int(v, 16) for k, v in r.items()}


def block_native(height, url):
    r = rpc("torus_getBlockBody", [height], url).get("result") or {}
    return int(r.get("nativeActionCount", 0)), (r.get("nativeActions") or [])


def load_key(idx):
    with open("/tmp/hardhat_accounts.txt") as f:
        for line in f:
            p = line.strip().split("|")
            if len(p) == 3 and int(p[0]) == idx:
                pk = KeyAPI().PrivateKey(bytes.fromhex(p[1].replace("0x", "")))
                return pk, p[2]
    raise SystemExit(f"account index {idx} not found in /tmp/hardhat_accounts.txt")


def settle(addr, url, base, deadline_s=20):
    """Poll availableBalance until it stops changing after first moving."""
    stable, last = 0, base
    deadline = time.time() + deadline_s
    while time.time() < deadline:
        time.sleep(0.5)
        cur = get_balances(addr, url)["availableBalance"]
        if cur != last:
            last, stable = cur, 0
        elif cur != base:
            stable += 1
            if stable >= 6:  # ~3s unchanged after first change
                break


def main():
    # selftest always runs first; abort before submitting if the signer is wrong.
    selftest()
    if len(sys.argv) > 1 and sys.argv[1] == "selftest":
        return

    url = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8645"
    acct_idx = int(sys.argv[2]) if len(sys.argv) > 2 else 80
    amount = int(sys.argv[3]) if len(sys.argv) > 3 else 100_000_000  # raw 8-dec = 1.0

    pk, addr = load_key(acct_idx)
    log(f"\nRPC={url}  account #{acct_idx}={addr}")
    log(f"amount(raw)={amount}  (= {amount / 1e8} in 8-decimal FixedPoint)")

    before = get_balances(addr, url)
    log(
        f"BEFORE: available={before['availableBalance']} "
        f"native={before['nativeBalance']} evm={before['evmBalance']}"
    )

    start_blk = block_number(url)
    nonce = int(time.time() * 1000)
    payload, sh, _ = sign_ttp(pk, amount, nonce)
    log(f"\nSubmitting ONE TransferToPerp  nonce={nonce}  struct_hash=0x{sh.hex()}")
    resp = rpc("torus_submitNativeAction", [payload], url)
    log(f"submit response: {json.dumps(resp)}")

    settle(addr, url, before["availableBalance"])

    end_blk = block_number(url)
    after = get_balances(addr, url)
    delta = after["availableBalance"] - before["availableBalance"]
    exec_count = delta / amount if amount else 0
    log(
        f"\nAFTER:  available={after['availableBalance']} "
        f"native={after['nativeBalance']} evm={after['evmBalance']}"
    )
    log(f"DELTA available = {delta}  => EXECUTED {exec_count:g}x")

    # Block-level inclusion: scan the window. On a quiescent chain every block with
    # native actions is our single submission.
    log(f"\nScanning blocks [{start_blk}..{end_blk}] for native inclusions:")
    incl = []
    for h in range(start_blk, end_blk + 1):
        cnt, acts = block_native(h, url)
        if cnt > 0:
            incl.append(h)
            nonces = [a.get("nonce") for a in acts if isinstance(a, dict)]
            log(f"  block {h}: nativeActionCount={cnt}  nonces={nonces}")
    log(f"\nINCLUDED in {len(incl)} block(s): {incl}")
    if incl:
        span = incl[-1] - incl[0] + 1
        log(f"  span={span} consecutive={span == len(incl)}")

    log("\n=== VERDICT ===")
    log(f"  1 submission -> EXECUTED {exec_count:g}x, INCLUDED in {len(incl)} block(s)")
    if exec_count >= 2:
        log("  >>> DUPLICATE EXECUTION CONFIRMED (correctness bug)")
    elif exec_count == 1:
        log("  >>> executed exactly once (correct / fixed)")
    else:
        log("  >>> action did not execute (check market/funding/nonce)")


if __name__ == "__main__":
    main()
