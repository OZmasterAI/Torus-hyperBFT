# EVM tx dissemination — direct-to-leader forward (Option B)

**Branch:** `feat/evm-tx-direct-to-leader`
**Decision:** mem `e68221898` (2026-06-13) — unicast forward to current leader, NOT a 2nd
gossipsub topic. All topics share one `gossipsub::Behaviour` + one swarm event-loop with no
cross-topic priority (mem `2d657035`, `58e4cd3`).

## Problem (proven live, mem `f8adaa0f`)
`eth_sendRawTransaction` → `mempool.add_evm_tx` is **local-pool only**. The EVM `TxGossipHandle`
is dropped as `_tx_gossip` (main.rs:433); `Mempool` has no EVM gossip field. So an EVM tx is
mined ONLY by the node it was submitted to, and only when that node leads — on an RPC-only /
non-proposer node it dead-ends forever.

## Approach — mirror the native forward path exactly
Native actions already reach the leader via direct-to-leader unicast over the `/torus/direct`
req-resp protocol, tagged with a marker byte. We add a parallel EVM path with a new marker.

| Stage | Native (exists) | EVM (new) |
|---|---|---|
| RPC send | `RpcState::forward_to_leader` torus.rs:517 | `RpcState::forward_evm_to_leader` |
| RPC channel | `forward_action_tx` lib.rs:165 | `forward_evm_tx` |
| node bridge | main.rs:617 → `forward_native_action` | new bridge → `forward_evm_tx` |
| net command | `ForwardNativeAction` + marker `0xFE` | `ForwardEvmTx` + marker `0xFB` |
| net receive | swarm.rs:889 → `native_action_inbound` | new branch → `evm_tx_inbound` |
| node ingest | main.rs:448 → `add_native_action_from_gossip` | new task → `add_evm_tx` |

### Key differences from native
- **Unconditional:** EVM forward is NOT gated on `forward_bodies` (that flag exists because
  native gossip can pre-spread bodies; EVM has no gossip, so we must always forward).
- **Payload = raw RLP only** (no 20-byte sender prefix): the leader recovers the sender inside
  `add_evm_tx` → `validate_evm_tx`. The leader independently full-validates (sig/nonce/balance/gas).
- **No loop / no re-forward:** only the RPC-receiving node forwards; the leader's ingest task
  calls `add_evm_tx` (not forward). Duplicates on the leader return `DuplicateTx` and are ignored.
- Leader rotation race is identical to native (forward targets the leader at submit time);
  acceptable per decision, re-forward is out of scope.

## Edit map
**torus-network/src/swarm.rs**
- add `const FORWARD_EVM_MARKER: u8 = 0xFB;`
- add `NetworkCommand::ForwardEvmTx { target, payload }`
- add `SharedState::evm_tx_inbound: Option<UnboundedSender<Vec<u8>>>`
- add receive branch (~889) for `FORWARD_EVM_MARKER` → `evm_tx_inbound`
- add command handler (~1305) → prepend marker, DirectRequest to leader
- add pure helpers `encode_forwarded_evm` / `parse_forwarded_evm_tx` (testable wire format)
- update test SharedState constructor (~1824)

**torus-network/src/bridge.rs**
- `LibP2PNetwork::evm_inbound_rx` field (+ Clone = None)
- create evm channel in `with_metrics`, wire into SharedState + struct
- `take_evm_tx_rx()`
- `forward_evm_tx(target, payload)`
- mock constructor (~395): `evm_tx_inbound: None`

**torus-rpc/src/lib.rs**
- `RpcState::forward_evm_tx` field (+ `None` in `new`)
- extend `set_leader_forwarding(...)` to also accept the EVM forward sender

**torus-rpc/src/torus.rs**
- `forward_evm_to_leader(&self, raw_rlp: &[u8])` (mirror of `forward_to_leader`, no `forward_bodies` gate)

**torus-rpc/src/eth.rs**
- `send_raw_transaction` (~629): after `add_evm_tx` Ok, call `self.forward_evm_to_leader(&bytes)`

**torus-node/src/main.rs**
- create `(evm_fwd_tx, evm_fwd_rx)`, pass `evm_fwd_tx` into `set_leader_forwarding`
- bridge task: `evm_fwd_rx.recv()` → `network.forward_evm_tx(vk, payload)`
- ingest task: `network.take_evm_tx_rx()` → `mempool.add_evm_tx(raw)` (ignore `DuplicateTx`)

## Tests
**Unit (torus-network, `cargo test -p torus-network --lib`):**
1. `forward_evm_marker_is_distinct` — `0xFB` ∉ {`0xFE`,`0xFD`,`0xFC`} (collision guard).
2. `forwarded_evm_tx_roundtrips` — `parse(encode(rlp)) == Some(rlp)`; empty body → `None`.
3. `parse_forwarded_evm_rejects_native_marker` — a `0xFE` envelope → `None` (no cross-routing
   of native bodies into `add_evm_tx`).

**Integration (live testnet — acceptance, mirrors the gap proof):**
- Submit an EVM value transfer to a NON-leader / RPC-only node; confirm it is mined in a block
  proposed by the *leader* (not the submitting node). This is the same method that proved the
  gap (`f8adaa0f`).

## Acceptance
- `cargo build` clean across workspace; `cargo test -p torus-network --lib` green.
- Live: EVM tx submitted to a non-proposer node gets included by the leader.

## Review findings — deferred follow-ups (high-effort code-review, 2026-06-14)
Applied during review: removed a redundant per-tx allocation — `forward_evm_to_leader` now
takes the RLP by value (the eth.rs clone moves straight into the channel; no second `to_vec`).

Deferred (all mirror the existing native forward path — parity, not regressions):
1. **Leader-rotation liveness edge** — the forward targets the leader at submit time; on a view
   change before that leader proposes, the tx can sit unincluded (EVM has no gossip fallback the
   way native does). A re-forward / leader-change retry would close it. Watch in the live test.
2. **No metrics** on the EVM forward/ingest path (native has `rpc_submit_admit_forward_seconds`
   et al.). Add forward/admit/reject counters for operability.
3. **Duplication** — the EVM pipeline copies the native one nearly line-for-line (bridge tasks,
   command handlers, forward methods, ingest tasks); already diverged (gate/metrics). A generic
   "forward typed payload to leader" mechanism parameterized by marker + optional gate would be
   the right altitude. Deferred to avoid touching the consensus-critical native path in this PR.
4. **RPC `tx_submit_limiter` bypass** on the leader's forwarded-tx ingest (mempool per-sender
   `is_evm_rate_limited` still applies; matches native gossip ingest).
5. **`pending_txs` notifier** not fired for forwarded txs on the leader (matches native gossip
   ingest) — `newPendingTransactions` subscribers on the leader miss forwarded txs.
6. **Unbounded** forward/ingest channels (matches native's unbounded forward/inbound channels).
