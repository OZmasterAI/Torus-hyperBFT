# Implementation Plan: Sprint 5 Item 1 — zstd Body Paths (Option A)

## Design Decision
docs/plans/sprint5-zstd-bodies.md Option A: sibling `2.0` protocols with
zstd-framed borsh + per-peer multistream fallback; gossip dual-subscribe with
publish flag. Mixed-binary safe; deployable to the seed before the friend
upgrade.

## Success Criteria
- Round-trip + bomb-guard + corrupt-frame tests green in torus-network.
- Mixed-version devnet (2.0 node ↔ 1.0 node): pull, push, sync all succeed
  via fallback; no MissingData regression; zero bans.
- Telemetry proves ratio: `wire_bytes_pre_compress` / `wire_bytes_on_wire`
  per path ≥ 3x on order-JSON bodies.
- Existing torus-network + torus-consensus suites stay green.

## Tasks

### Task 1: zstd codec frame + tests (RED → GREEN)
- **Test first** (codec.rs tests): `zstd_roundtrip_preserves_borsh`,
  `zstd_read_rejects_decompressed_over_cap` (bomb guard),
  `zstd_read_rejects_corrupt_frame`, plus a level-1/3/6 ratio probe on a
  representative 500-order PlaceOrderBatch body (prints ratio; asserts ≥2x).
- **Implementation**: `write_length_prefixed_borsh_zstd` /
  `read_length_prefixed_borsh_zstd` beside codec.rs:90, bounded by the same
  per-path caps (decompressed size checked streamingly). `zstd` crate dep in
  torus-network/Cargo.toml. Level from `const ZSTD_LEVEL: i32 = 3` pending T1
  probe.
- **Verify**: `nice -n19 cargo test -p torus-network --lib zstd -j2`

### Task 2: register 2.0 protocols (request/response negotiation)
- **Test first**: behaviour test asserting both `/torus/native-da/1.0` and
  `/torus/native-da/2.0` are offered, and codec picks zstd read/write when
  the negotiated protocol is 2.0.
- **Implementation**: behaviour.rs:62/71/81/90 — add 2.0 StreamProtocol
  alongside each 1.0; codec dispatch on negotiated protocol name
  (codec.rs:318, swarm.rs:2033 same pattern).
- **Verify**: `nice -n19 cargo test -p torus-network --lib -j2`
- **Depends on**: Task 1

### Task 3: wire push/pull/sync senders to prefer 2.0
- **Implementation**: bridge.rs push + pull request senders and sync client
  use the negotiated protocol transparently (request_response handles it);
  audit any hardcoded `/1.0` strings (bridge.rs:78,242,264).
- **Verify**: full torus-network suite.
- **Depends on**: Task 2

### Task 4: gossip dual-subscribe + `--gossip-zstd` publish flag
- **Test first**: mempool/bridge test — v2-topic receive path decompresses
  and admits; publish honors flag (v1 default).
- **Implementation**: behaviour.rs:14 add `NATIVE_ACTION_TOPIC_V2 =
  "/torus/native-actions/2.0"`; subscribe both; publish v2 iff flag;
  torus-node main.rs flag plumbing (default off).
- **Verify**: `nice -n19 cargo test -p torus-network --lib -j2 && nice -n19 cargo test -p torus-mempool --lib -j2`
- **Depends on**: Task 1

### Task 5: telemetry — per-path pre/post byte counters
- **Implementation**: torus-telemetry counters
  `wire_bytes_pre_compress{path}` / `wire_bytes_on_wire{path}`; observe in
  the zstd write fn. Registration test (mirrors exec-phase metric tests).
- **Verify**: `nice -n19 cargo test -p torus-telemetry --lib -j2`
- **Depends on**: Task 1

### Task 6: mixed-version devnet proof
- **Verify**: 4-node local devnet, one node built at HEAD~1 (1.0-only):
  bench flood; assert all nodes commit in lockstep, pulls succeed on both
  protocol versions (logs), dup-factor ≤1.05, zero bans. Capture ratio
  numbers from T5 counters into the design doc results section.
- **Depends on**: Tasks 2–5
- NOTE (s332 lesson, mem 59db6ad3): devnet bench must run OFF the live
  validator box or at nice -n19 with the seed watched.

### Task 7: commit + docs results
- Commit code; append measured ratios to sprint5-zstd-bodies.md.
- **Depends on**: Task 6

## Verification (end-to-end)
Devnet mixed-version proof (T6) is the gate. Live testnet rollout per the
design doc Rollout section — seed deploy any time after T6; gossip flip only
after friend deploy (standing prerequisite, unchanged).

## Rollback
2.0 protocols are additive; reverting the commit returns to 1.0-only. The
gossip flag defaults off, so no live behavior changes until ops flips it.
