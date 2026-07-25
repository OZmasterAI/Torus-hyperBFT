# Torus-hyperBFT

High-performance EVM-compatible blockchain with HotStuff BFT consensus. Sub-100ms block times, Cancun EVM, native actions, and QUIC-based p2p networking.

## Quick Start

### Prerequisites

- Rust 1.82+
- Linux (Ubuntu 22.04+ recommended)
- A C/C++ toolchain (`build-essential` / `clang`) and **`perl`** — the workspace
  enables `alloy-primitives/asm-keccak`, whose `sha3-asm` build script generates
  the Keccak assembly with perl (cryptogams perlasm) and compiles it with `cc`.
  Both are preinstalled on `ubuntu-latest` CI runners.
  Supported build architectures: `x86_64`, `aarch64`, `arm`, `x86` (needs MMX),
  `mips`, `powerpc`, `riscv`, `s390x`. Other targets (notably `wasm32`) fail at
  build time with `unsupported target arch`; a wasm build would first have to
  make that feature target-conditional in the root `Cargo.toml`.

### Build

```bash
cargo build --release -p torus-node
```

### Run a Testnet Node

```bash
# 1. Generate your validator key
./target/release/torus-node keygen --output my-key.json

# 2. Build the genesis (expands the tracked base into the full file nodes boot)
./testnet/gen-weighted-genesis.sh

# 3. Start the node (auto-connects to seed nodes)
./target/release/torus-node --keystore my-key.json \
  --genesis testnet/genesis-weighted-full.json --rpc-only
```

> `testnet/genesis-weighted-full.json` is **generated**, not tracked — it carries
> ~100k bulk bench accounts and is too large for git. The tracked source is
> `testnet/genesis-weighted-base.json`. Every node must run the generator and end
> up with the **same sha256**, or it builds a different state root and cannot join.

The node will automatically discover the network via hardcoded bootstrap peers.

### Config File (Optional)

Instead of CLI flags, create a `torus.toml`:

```toml
genesis = "testnet/genesis-weighted-full.json"
keystore = "my-key.json"
data-dir = "./data"
log-level = "info"
rpc-addr = "0.0.0.0:8545"
```

Then run:

```bash
./target/release/torus-node --config torus.toml
```

CLI flags always override config file values.

## Local Devnet

Spin up a 4-validator local network with Docker Compose:

```bash
cargo build --release -p torus-node
cd devnet
./start.sh          # build and start
./start.sh logs     # follow logs
./start.sh down     # stop and remove
```

RPC endpoints: `localhost:8545` through `localhost:8549`

## Architecture

```
torus-node          CLI entrypoint, wires all crates together
torus-consensus     HotStuff BFT application layer (block production/validation)
hotstuff_rs         Consensus protocol engine (MonadBFT extensions)
torus-evm           EVM execution via revm (Cancun spec)
torus-network       libp2p networking (QUIC, GossipSub, Kademlia)
torus-mempool       Transaction pool with gossip
torus-rpc           JSON-RPC server (eth_*, net_*, web3_*)
torus-state         RocksDB state storage with pruning
torus-bridge        Proposer logic bridging mempool to consensus
torus-genesis       Genesis file parsing and chain initialization
torus-economics     Fee distribution and staking economics
torus-telemetry     Prometheus metrics and tracing
torus-types         Shared types (blocks, transactions, addresses)
torus-core          Core utilities
```

## Key Features

- **HotStuff BFT consensus** with pipelined execution
- **EVM (Cancun)** — full Ethereum compatibility
- **Native actions** — on-chain operations outside EVM (staking, governance)
- **QUIC transport** — low-latency p2p via libp2p
- **Encrypted keystore** — AES-256-GCM with Argon2id key derivation
- **State pruning** — optional `--retention-blocks N` to cap disk usage
- **Snapshot restore** — `--restore-from-snapshot` for fast sync

## CLI Reference

```
torus-node [OPTIONS] [COMMAND]

Commands:
  keygen    Generate a new validator keypair

Options:
  --config <PATH>              TOML config file
  --genesis <PATH>             Genesis JSON file (required on first run)
  --keystore <PATH>            Encrypted keystore file
  --data-dir <PATH>            Data directory [default: ./data]
  --p2p-listen <MULTIADDR>     Listen address [default: /ip4/0.0.0.0/udp/30333/quic-v1]
  --p2p-peers <ADDRS>          Bootstrap peers (comma-separated multiaddrs)
  --rpc-addr <ADDR>            JSON-RPC address [default: 0.0.0.0:8545]
  --rpc-only                   Run as non-validator sync node
  --log-level <LEVEL>          Log level [default: info]
  --metrics-addr <ADDR>        Prometheus metrics [default: 0.0.0.0:9090]
  --retention-blocks <N>       Enable pruning (keep last N blocks)
  --archive                    Archive mode (keep all data, default)
```

## Network

| Network | Chain ID | RPC | Status |
|---------|----------|-----|--------|
| Testnet | 7778 | `http://95.111.231.121:8545` | Active |

## License

Apache-2.0
