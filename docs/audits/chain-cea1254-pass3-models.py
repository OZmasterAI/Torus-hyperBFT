#!/usr/bin/env python3
"""Source-guarded counterexamples for the project-wide audit.

These are Python models, not executions of Torus/revm/RocksDB. No network,
transactions, or node changes. The bytecode example follows the pinned
revm-bytecode 9.0.0 analysis rule for a complete program ending in RETURN.
"""
from pathlib import Path
import re
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
SHA = "cea1254e34625e6b09c58f794de8793b5c12713c"


def read(path):
    return (ROOT / path).read_text()


def guards():
    assert subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip() == SHA
    decode = read("crates/torus-bridge/src/decode.rs")
    assert "gas_priority_fee: Some(tx.max_priority_fee_per_gas)" in decode
    assert "tx_type:" not in decode and "derive_tx_type" not in decode
    assert "let raw = bytecode.bytes();" in read("crates/torus-state/src/incremental.rs")
    assert "Bytecode::new_raw(Bytes::from(bytes))" in read("crates/torus-state/src/db.rs")
    lock = tomllib.loads(read("Cargo.lock"))
    versions = {p["name"]: p["version"] for p in lock["package"]}
    assert versions["revm-context"] == "15.0.0"
    assert versions["revm-context-interface"] == "16.0.0"
    assert versions["revm-bytecode"] == "9.0.0"
    snapshot = read("crates/torus-state/src/snapshot.rs").split("pub fn verify_snapshot", 1)[1]
    snapshot = snapshot.split("pub fn restore_from_snapshot", 1)[0]
    assert "native_root_full(&snapshot_db)" in snapshot
    assert "computed_root == metadata.state_root" in snapshot
    assert "running_hash" not in snapshot
    node = read("crates/torus-node/src/main.rs")
    assert "no genesis file provided, using default chain config" in node
    assert "treasury_address: Address::ZERO" in node
    assert "dev_pool_address: Address::ZERO" in node


def typed_fee():
    cap, base, tip, gas = 100, 10, 2, 21_000
    actual_price = cap  # Default TxEnv type 0 selects legacy effective price.
    advertised_price = min(cap, base + tip)
    assert actual_price == 100 and advertised_price == 12
    assert gas * (actual_price - advertised_price) == 1_848_000
    print(f"EVM type: charged {gas * actual_price} vs receipt {gas * advertised_price} fee units")


def padded_code():
    # CODESIZE; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN.
    # Complete PUSH data, last opcode RETURN: revm analysis appends one STOP.
    original = bytes.fromhex("3860005260206000f3")
    analyzed = original + b"\0"
    persisted = analyzed  # Bytecode::bytes()
    reloaded_original_length = len(persisted)  # Bytecode::new_raw -> new_legacy
    assert len(original) == 9 and reloaded_original_length == 10
    assert persisted != original
    # Control: saving original_bytes preserves the deployed length.
    assert len(original) == 9
    print("EVM code: deployed length 9 -> persisted/reloaded original length 10")


def snapshot_projection():
    source = read("crates/torus-state/src/native_trie.rs")
    literal = source.split("pub const NATIVE_ROOT_CFS:", 1)[1].split("];", 1)[0]
    committed = set(re.findall(r"\((CF_[A-Z_]+),\s*\d+\)", literal))
    assert len(committed) == 7
    excluded = {"CF_SESSIONS", "CF_NATIVE_NONCES", "CF_CORE_WRITER_QUEUE",
                "CF_GOVERNANCE_PROPOSALS", "CF_NATIVE_MARKETS"}
    assert committed.isdisjoint(excluded)
    before = {cf: [(b"key", b"same")] for cf in committed}
    before["CF_NATIVE_NONCES"] = [(b"sender_nonce", b"consumed")]
    after = {**before, "CF_NATIVE_NONCES": []}
    def native_inputs(state):
        return tuple((cf, tuple(state[cf])) for cf in sorted(committed))
    assert before != after and native_inputs(before) == native_inputs(after)
    print("Snapshot: removing a consumed nonce leaves every verified native-root input unchanged")


def restart_config():
    original = {"epoch_length": 4000, "treasury": "T", "dev": "D"}
    defaults = {"epoch_length": 100000, "treasury": "ZERO", "dev": "ZERO"}
    restart_without_genesis = defaults
    assert original != restart_without_genesis
    def recipient_credits(config):
        credits = {}
        for field in ("treasury", "dev"):
            recipient = config[field]
            credits[recipient] = credits.get(recipient, 0) + 45
        return credits
    assert recipient_credits(original) == {"T": 45, "D": 45}
    assert recipient_credits(restart_without_genesis) == {"ZERO": 90}
    print("Restart: identical 100-unit epoch-0 fee credits T/D before, ZERO after omitted genesis")


if __name__ == "__main__":
    guards()
    typed_fee()
    padded_code()
    snapshot_projection()
    restart_config()
