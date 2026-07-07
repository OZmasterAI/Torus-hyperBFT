#!/usr/bin/env python3
"""state-grow.py — grow REAL EVM state fast by creating NEW accounts.

Sends legacy (EIP-155) value transfers from the genesis-funded hardhat accounts
to FRESH, never-before-seen recipient addresses via raw eth_sendRawTransaction
over plain HTTP (ThreadPoolExecutor — no per-tx `cast` process spawn, so this is
~20-50x faster than the cast loop in state-grow.sh). Every fresh recipient is a
brand-new account in CF_ACCOUNTS + the incremental trie, so the account count /
data-dir / state-root full-scan cost grows MONOTONICALLY — the "large pre-grown
state" the A1.6 flatness proof needs, not churn on a fixed sender set.

Signing is hand-rolled minimal RLP + eth_keys secp256k1 (eth-account/rlp are not
installed on this host). A `selftest` mode signs a deterministic tx and compares
the raw bytes to `cast mktx`, so a wrong encoder can never silently flood
rejected txs. Targets ONLY the devnet RPCs; 8545 (live testnet) is refused.

Usage / env:
  python3 state-grow.py selftest
  DURATION=180 OFFSET=0 SENDERS=60 WORKERS=64 \
  RPCS="http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548" \
    python3 state-grow.py
Prints periodic progress and, at the end, `NEXT_OFFSET=<n>` (feed as OFFSET into
the next stage so recipients never repeat).
"""

import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

import requests
from Crypto.Hash import keccak as keccak_mod
from eth_keys import KeyAPI

CHAIN_ID = int(os.environ.get("CHAIN_ID", "7778"))
GAS_PRICE = int(os.environ.get("GAS_PRICE", str(100_000_000_000_000)))
GAS_LIMIT = int(os.environ.get("GAS_LIMIT", "21000"))
VALUE = int(os.environ.get("VALUE", str(1_000_000_000)))  # 1 gwei — trivial
RECIP_PREFIX = os.environ.get("RECIP_PREFIX", "cafe")  # 4 hex + 36 hex counter
ACCOUNTS_FILE = os.environ.get("ACCOUNTS_FILE", "/tmp/hardhat_accounts.txt")


def keccak256(data: bytes) -> bytes:
    h = keccak_mod.new(digest_bits=256)
    h.update(data)
    return h.digest()


# --- minimal RLP -----------------------------------------------------------
def _rlp_len(n: int, offset: int) -> bytes:
    if n < 56:
        return bytes([offset + n])
    lb = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([offset + 55 + len(lb)]) + lb


def rlp(item) -> bytes:
    if isinstance(item, list):
        payload = b"".join(rlp(x) for x in item)
        return _rlp_len(len(payload), 0xC0) + payload
    b = item
    if len(b) == 1 and b[0] < 0x80:
        return b
    return _rlp_len(len(b), 0x80) + b


def _int(n: int) -> bytes:
    """RLP integer: minimal big-endian, 0 -> empty string."""
    if n == 0:
        return b""
    return n.to_bytes((n.bit_length() + 7) // 8, "big")


def sign_legacy_tx(pk, nonce, to_bytes, value, gas_price, gas_limit, chain_id):
    """Return the 0x raw signed legacy (EIP-155) transaction."""
    unsigned = rlp(
        [
            _int(nonce),
            _int(gas_price),
            _int(gas_limit),
            to_bytes,
            _int(value),
            b"",
            _int(chain_id),
            b"",
            b"",
        ]
    )
    sig = pk.sign_msg_hash(keccak256(unsigned))
    v = chain_id * 2 + 35 + sig.v
    signed = rlp(
        [
            _int(nonce),
            _int(gas_price),
            _int(gas_limit),
            to_bytes,
            _int(value),
            b"",
            _int(v),
            _int(sig.r),
            _int(sig.s),
        ]
    )
    return "0x" + signed.hex()


# --- self-test against cast mktx -------------------------------------------
def selftest():
    keys = load_keys(1)
    pk, _addr = keys[0]
    to = "0x" + RECIP_PREFIX + "0" * 34 + "07"
    nonce, val = 3, 12345
    mine = sign_legacy_tx(
        pk, nonce, bytes.fromhex(to[2:]), val, GAS_PRICE, GAS_LIMIT, CHAIN_ID
    )
    try:
        out = subprocess.run(
            [
                "cast",
                "mktx",
                "--private-key",
                "0x" + pk.to_bytes().hex(),
                "--nonce",
                str(nonce),
                "--gas-price",
                str(GAS_PRICE),
                "--priority-gas-price",
                str(GAS_PRICE),
                "--gas-limit",
                str(GAS_LIMIT),
                "--chain",
                str(CHAIN_ID),
                "--legacy",
                to,
                "--value",
                str(val),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout.strip()
    except Exception as e:  # noqa: BLE001
        print(f"selftest: cast unavailable ({e}); skipping byte-compare", flush=True)
        print(f"  mine={mine}", flush=True)
        return
    ok = out.lower() == mine.lower()
    print(
        f"selftest: {'PASS' if ok else 'FAIL'} raw-tx byte-match vs cast mktx",
        flush=True,
    )
    if not ok:
        print(f"  mine={mine}\n  cast={out}", flush=True)
        sys.exit(1)


def load_keys(n):
    keys = []
    with open(ACCOUNTS_FILE) as f:
        for line in f:
            p = line.strip().split("|")
            if len(p) == 3:
                pk = KeyAPI().PrivateKey(bytes.fromhex(p[1].replace("0x", "")))
                keys.append((pk, p[2]))
                if len(keys) >= n:
                    break
    if not keys:
        sys.exit(f"state-grow: no keys in {ACCOUNTS_FILE}")
    return keys


def rpc(url, method, params, timeout=5):
    body = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    return requests.post(url, json=body, timeout=timeout).json()


def recip(counter):
    return "0x" + RECIP_PREFIX + f"{counter:036x}"


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "selftest":
        selftest()
        return
    selftest()  # always verify the encoder before flooding

    duration = int(os.environ.get("DURATION", "180"))
    offset = int(os.environ.get("OFFSET", "0"))
    senders = int(os.environ.get("SENDERS", "60"))
    workers = int(os.environ.get("WORKERS", "64"))
    rpcs = os.environ.get(
        "RPCS",
        "http://localhost:8645,http://localhost:8546,http://localhost:8547,http://localhost:8548",
    ).split(",")
    for u in rpcs:
        if u.rstrip("/").endswith(":8545"):
            sys.exit(f"state-grow: REFUSING to target testnet RPC {u}")

    keys = load_keys(senders)
    nk = len(keys)
    # Seed per-sender nonces from chain (never rewind — mempool may lead).
    nonces = []
    for _pk, addr in keys:
        try:
            r = rpc(rpcs[0], "eth_getTransactionCount", [addr, "pending"])
            nonces.append(int(r["result"], 16))
        except Exception:  # noqa: BLE001
            nonces.append(0)

    def height():
        try:
            return int(rpc(rpcs[0], "eth_blockNumber", [])["result"], 16)
        except Exception:  # noqa: BLE001
            return -1

    def send(sidx, counter, ep):
        pk, _addr = keys[sidx]
        nonce = nonces[sidx]
        nonces[sidx] = nonce + 1
        raw = sign_legacy_tx(
            pk,
            nonce,
            bytes.fromhex(recip(counter)[2:]),
            VALUE,
            GAS_PRICE,
            GAS_LIMIT,
            CHAIN_ID,
        )
        try:
            r = rpc(ep, "eth_sendRawTransaction", [raw])
            return "result" in r and bool(r["result"])
        except Exception:  # noqa: BLE001
            return False

    print(
        f"=== state-grow.py: senders={nk} duration={duration}s offset={offset} "
        f"endpoints={len(rpcs)} recip_base={recip(offset)} ===",
        flush=True,
    )
    submitted = accepted = 0
    counter = offset
    start = time.time()
    last = start
    h0 = height()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        while time.time() - start < duration:
            futs = []
            for _ in range(workers):
                sidx = submitted % nk
                ep = rpcs[submitted % len(rpcs)]
                futs.append(pool.submit(send, sidx, counter, ep))
                counter += 1
                submitted += 1
            for fu in futs:
                if fu.result():
                    accepted += 1
            now = time.time()
            if now - last >= 5:
                el = now - start
                print(
                    f"state-grow {el:6.0f}s | submitted={submitted:<8d} "
                    f"accepted={accepted:<8d} rate={accepted / max(el, 0.01):5.0f}/s "
                    f"height={height()}",
                    flush=True,
                )
                last = now
    el = time.time() - start
    h1 = height()
    print(
        f"=== state-grow.py done: submitted={submitted} accepted={accepted} "
        f"({accepted / max(el, 0.01):.0f}/s) new-recipients={counter - offset} "
        f"height {h0}->{h1} ===",
        flush=True,
    )
    print(f"NEXT_OFFSET={counter}", flush=True)


if __name__ == "__main__":
    main()
