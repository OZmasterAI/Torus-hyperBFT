#!/usr/bin/env python3
"""native-order-flood.py — Submit PlaceOrder native actions to Torus devnet.

Usage: python3 native-order-flood.py [rpc_url] [num_senders] [orders_per_sec]

Generates random limit orders signed with EIP-712, submits via
torus_submitNativeAction RPC. Reports throughput.

Requires: pip install eth-keys pycryptodome requests
"""

import json
import os
import random
import struct
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests
from Crypto.Hash import keccak as keccak_mod
from eth_keys import KeyAPI

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
RPC_URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8545"
NUM_SENDERS = int(sys.argv[2]) if len(sys.argv) > 2 else 100
TARGET_OPS = int(sys.argv[3]) if len(sys.argv) > 3 else 200  # target orders/sec
CHAIN_ID = 7778
MARKET_ID = 1
MAX_WORKERS = 32
POLL_INTERVAL = 2.0

HARDHAT_MNEMONIC_KEYS = []  # populated below

# ---------------------------------------------------------------------------
# Crypto helpers
# ---------------------------------------------------------------------------


def keccak256(data: bytes) -> bytes:
    h = keccak_mod.new(digest_bits=256)
    h.update(data)
    return h.digest()


def uint256(v: int) -> bytes:
    """ABI-encode an integer as uint256 (32 bytes big-endian)."""
    return v.to_bytes(32, "big")


def int128_to_uint256(v: int) -> bytes:
    """ABI-encode an i128 as 32-byte two's complement big-endian."""
    if v < 0:
        v = (1 << 256) + v
    return v.to_bytes(32, "big")


def bool_to_uint256(v: bool) -> bytes:
    return uint256(1 if v else 0)


def address_to_uint256(addr_hex: str) -> bytes:
    return bytes(12) + bytes.fromhex(addr_hex.replace("0x", ""))


# ---------------------------------------------------------------------------
# EIP-712 domain (pinned for Torus chain 7778)
# ---------------------------------------------------------------------------
DOMAIN_SEPARATOR = bytes.fromhex(
    "c17bc08f8d2e5d76651f1d3a5c156a7cfc34b56bb732b24997bfc22da5f4a257"
)

PLACE_ORDER_TYPEHASH = keccak256(
    b"PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,"
    b"uint8 orderType,uint8 timeInForce,bool reduceOnly,"
    b"uint64 clientOrderId,bool hasClientOrderId,uint64 nonce)"
)

ORDER_TYPE_LIMIT = 0
ORDER_TYPE_MARKET = 1
TIF_GTC = 0
TIF_IOC = 1

# ---------------------------------------------------------------------------
# EIP-712 struct hash for PlaceOrder
# ---------------------------------------------------------------------------


def place_order_struct_hash(
    market_id: int,
    is_buy: bool,
    price: int,
    quantity: int,
    order_type: int,
    time_in_force: int,
    reduce_only: bool,
    client_order_id: int | None,
    nonce: int,
) -> bytes:
    has_coid = client_order_id is not None
    coid = client_order_id if has_coid else 0
    data = (
        PLACE_ORDER_TYPEHASH
        + uint256(market_id)
        + bool_to_uint256(is_buy)
        + int128_to_uint256(price)
        + int128_to_uint256(quantity)
        + uint256(order_type)
        + uint256(time_in_force)
        + bool_to_uint256(reduce_only)
        + uint256(coid)
        + bool_to_uint256(has_coid)
        + uint256(nonce)
    )
    return keccak256(data)


def eip712_signing_hash(struct_hash: bytes) -> bytes:
    return keccak256(b"\x19\x01" + DOMAIN_SEPARATOR + struct_hash)


# ---------------------------------------------------------------------------
# Key derivation (hardhat mnemonic accounts 0-99)
# ---------------------------------------------------------------------------


def load_keys(n: int) -> list[tuple[KeyAPI.PrivateKey, str]]:
    """Derive n private keys from the Hardhat mnemonic using cast."""
    keys = []
    for i in range(n):
        raw = (
            os.popen(
                f"cast wallet derive-private-key "
                f'"test test test test test test test test test test test junk" {i} 2>/dev/null'
            )
            .read()
            .strip()
        )
        if not raw:
            print(f"ERROR: cast failed for account #{i}")
            sys.exit(1)
        pk_bytes = bytes.fromhex(raw.replace("0x", ""))
        pk = KeyAPI().PrivateKey(pk_bytes)
        addr = pk.public_key.to_checksum_address()
        keys.append((pk, addr))
    return keys


def load_keys_fast(n: int) -> list[tuple[KeyAPI.PrivateKey, str]]:
    """Load keys from the pre-generated file or derive them."""
    cache = "/tmp/hardhat_accounts.txt"
    if os.path.exists(cache):
        keys = []
        with open(cache) as f:
            for line in f:
                parts = line.strip().split("|")
                if len(parts) == 3:
                    idx, key_hex, addr = parts
                    pk_bytes = bytes.fromhex(key_hex.replace("0x", ""))
                    pk = KeyAPI().PrivateKey(pk_bytes)
                    keys.append((pk, addr))
                    if len(keys) >= n:
                        break
        if len(keys) >= n:
            return keys[:n]
    return load_keys(n)


# ---------------------------------------------------------------------------
# Sign and encode a PlaceOrder action
# ---------------------------------------------------------------------------


def sign_place_order(
    pk: KeyAPI.PrivateKey,
    market_id: int,
    is_buy: bool,
    price: int,
    quantity: int,
    nonce: int,
    order_type: int = ORDER_TYPE_LIMIT,
    time_in_force: int = TIF_IOC,
) -> str:
    """Build, sign, and hex-encode a PlaceOrder native action.

    Returns the 0x-prefixed hex payload for torus_submitNativeAction.
    """
    sh = place_order_struct_hash(
        market_id,
        is_buy,
        price,
        quantity,
        order_type,
        time_in_force,
        False,
        None,
        nonce,
    )
    msg_hash = eip712_signing_hash(sh)
    sig = pk.sign_msg_hash(msg_hash)

    v = sig.v + 27
    r_bytes = sig.r.to_bytes(32, "big")
    s_bytes = sig.s.to_bytes(32, "big")

    tif_str = ["GTC", "IOC", "FOK", "PostOnly"][time_in_force]
    ot_str = ["Limit", "Market", "StopMarket", "StopLimit"][order_type]

    envelope = {
        "action": {
            "PlaceOrder": {
                "market_id": market_id,
                "is_buy": is_buy,
                "price": price,
                "quantity": quantity,
                "order_type": ot_str,
                "time_in_force": tif_str,
                "reduce_only": False,
                "client_order_id": None,
            }
        },
        "nonce": nonce,
        "signature": {
            "Eip712": {
                "v": v,
                "r": list(r_bytes),
                "s": list(s_bytes),
            }
        },
    }

    json_bytes = json.dumps(envelope, separators=(",", ":")).encode()
    return "0x" + json_bytes.hex()


# ---------------------------------------------------------------------------
# RPC helpers
# ---------------------------------------------------------------------------

_session = requests.Session()
_session.headers.update({"Content-Type": "application/json"})
_req_id = 0


def rpc_call(method: str, params: list, url: str = RPC_URL) -> dict:
    global _req_id
    _req_id += 1
    body = {"jsonrpc": "2.0", "id": _req_id, "method": method, "params": params}
    try:
        r = _session.post(url, json=body, timeout=5)
        return r.json()
    except Exception as e:
        return {"error": str(e)}


def get_block_number() -> int:
    r = rpc_call("eth_blockNumber", [])
    return int(r.get("result", "0x0"), 16)


def get_block_native_count(height: int) -> int:
    """Get native action count from a block."""
    r = rpc_call("torus_getBlockBody", [hex(height)])
    if "result" in r and r["result"]:
        body = r["result"]
        return len(body.get("native_actions", []))
    return 0


def submit_action(payload: str, url: str = RPC_URL) -> dict:
    return rpc_call("torus_submitNativeAction", [payload], url)


# ---------------------------------------------------------------------------
# Main flood loop
# ---------------------------------------------------------------------------

# Price range for random orders (FixedPoint raw, 8 decimals)
# 60000.0 - 70000.0 → raw 6_000_000_000_000 - 7_000_000_000_000
PRICE_MIN = 6_000_000_000_000
PRICE_MAX = 7_000_000_000_000
QTY_MIN = 1_000_000  # 0.01
QTY_MAX = 100_000_000  # 1.0


def main():
    print(f"=== Torus Native Order Flood ===")
    print(f"RPC:     {RPC_URL}")
    print(f"Senders: {NUM_SENDERS}")
    print(f"Target:  {TARGET_OPS} orders/sec")
    print()

    # Check RPC
    blk = get_block_number()
    if blk == 0:
        print("ERROR: cannot reach RPC or chain not started")
        sys.exit(1)
    print(f"Connected — block #{blk}")

    # Load keys
    print(f"Loading {NUM_SENDERS} sender keys...")
    keys = load_keys_fast(NUM_SENDERS)
    print(f"  loaded {len(keys)} accounts")
    print(f"  first: {keys[0][1]}")
    print(f"  last:  {keys[-1][1]}")
    print()

    # RPC endpoints — distribute across all 4 validators + rpc for throughput
    endpoints = []
    for port in [8545, 8546, 8547, 8548, 8549]:
        try:
            r = rpc_call("eth_blockNumber", [], f"http://localhost:{port}")
            if "result" in r:
                endpoints.append(f"http://localhost:{port}")
        except Exception:
            pass
    if not endpoints:
        endpoints = [RPC_URL]
    print(f"Submitting to {len(endpoints)} endpoints: {endpoints}")
    print()

    # Flood
    submitted = 0
    accepted = 0
    errors = 0
    start_time = time.time()
    start_block = get_block_number()
    nonce_counters = {}  # per-sender nonce (ms timestamp + offset)

    delay = 1.0 / max(TARGET_OPS, 1)
    batch_size = min(NUM_SENDERS, MAX_WORKERS)
    last_report = time.time()

    print(f"Flooding... (Ctrl+C to stop)")
    print(
        f"{'time':>8} | {'sub':>6} | {'ok':>6} | {'err':>5} | {'ops/s':>7} | {'blk':>8} | native/blk"
    )
    print("-" * 80)

    try:
        with ThreadPoolExecutor(max_workers=MAX_WORKERS) as pool:
            while True:
                futures = []
                batch_start = time.time()

                for _ in range(batch_size):
                    idx = random.randrange(len(keys))
                    pk, addr = keys[idx]
                    is_buy = random.random() > 0.5
                    price = random.randint(PRICE_MIN, PRICE_MAX)
                    qty = random.randint(QTY_MIN, QTY_MAX)
                    nonce_ms = int(time.time() * 1000)
                    # Add per-sender offset to avoid nonce collisions
                    offset = nonce_counters.get(idx, 0)
                    nonce_counters[idx] = offset + 1
                    nonce = nonce_ms + offset

                    payload = sign_place_order(pk, MARKET_ID, is_buy, price, qty, nonce)
                    ep = endpoints[submitted % len(endpoints)]
                    futures.append(pool.submit(submit_action, payload, ep))
                    submitted += 1

                for f in as_completed(futures):
                    result = f.result()
                    if "result" in result and result["result"]:
                        accepted += 1
                    else:
                        errors += 1

                elapsed = time.time() - start_time
                if time.time() - last_report >= POLL_INTERVAL:
                    cur_block = get_block_number()
                    blocks_delta = cur_block - start_block
                    native_rate = "?"
                    if blocks_delta > 0:
                        # Sample last block's native count
                        nc = get_block_native_count(cur_block - 1)
                        native_rate = str(nc)

                    ops_s = accepted / max(elapsed, 0.01)
                    print(
                        f"{elapsed:7.1f}s | {submitted:6d} | {accepted:6d} | {errors:5d} "
                        f"| {ops_s:6.0f}/s | #{cur_block:>6d} | {native_rate}"
                    )
                    last_report = time.time()

                # Pace to target rate
                batch_elapsed = time.time() - batch_start
                target_elapsed = batch_size * delay
                if batch_elapsed < target_elapsed:
                    time.sleep(target_elapsed - batch_elapsed)

    except KeyboardInterrupt:
        elapsed = time.time() - start_time
        cur_block = get_block_number()
        print()
        print(f"Stopped after {submitted} orders ({errors} errors) in {elapsed:.1f}s")
        print(f"  accepted: {accepted} ({accepted / max(elapsed, 0.01):.0f}/s)")
        print(
            f"  blocks:   {start_block} → {cur_block} ({cur_block - start_block} blocks)"
        )


if __name__ == "__main__":
    main()
