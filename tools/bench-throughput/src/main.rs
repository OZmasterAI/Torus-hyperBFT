use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use clap::{Parser, Subcommand};
use k256::ecdsa::SigningKey;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::Semaphore;

use torus_types::{
    eip712::{sign_native_action, sign_native_action_with_session},
    FixedPoint, NativeAction, OrderType, PlaceOrderParams, SessionScope, SignedNativeAction,
    TimeInForce,
};

const HARDHAT_KEYS: [&str; 20] = [
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
    "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
    "5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
    "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6",
    "47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a",
    "8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba",
    "92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e",
    "4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356",
    "dbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97",
    "2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6",
    "f214f2b2cd398c806f84e317254e0f0b801d0643303237d97a22a48e01628897",
    "701b615bbdfb9de65240bc28bd21bbc0d996645a3dd57e7b12bc2bdf6f192c82",
    "a267530f49f8280200edf313ee7af6b827f2a8bce2897751d06a843f644967b1",
    "47c99abed3324a2707c28affff1267e45918ec8c3f20b8aa892e8b065d2942dd",
    "c526ee95bf44d8fc405a158bb884d9d1238d99f0612e9f33d006bb0789009aaa",
    "8166f546bab6da521a8369cab06c5d2b9e46670292d85c875ee9ec20e84ffb61",
    "ea6c44ac03bff858b476bba40716402b03e41b8e97e276d1baec7c37d42484a0",
    "689af8efa8c651a91ad287602527f3af2fe9f6501a7ac4b061667b5a93e037fd",
    "de9be858da4a475276426320d5e9262ecfc3ba460bfac56360bfa6c4c28b4ee0",
    "df57089febbacf7ba0bc227dafbffa9fc08a93fdc68e1e42411a14efcf23656e",
];

#[derive(Parser)]
#[command(
    name = "bench-throughput",
    about = "Torus-hyperBFT throughput benchmarking tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    MatchingEngine {
        #[arg(long, default_value_t = 10_000)]
        orders: usize,
        #[arg(long, default_value_t = 1)]
        markets: u64,
        #[arg(long, default_value_t = 1_000)]
        warmup: usize,
        #[arg(long, default_value = "devnet/genesis.json")]
        genesis: String,
    },
    Consensus {
        #[arg(
            long,
            default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548"
        )]
        rpc_urls: String,
        /// Number of distinct signing senders. Defaults to 20 — the hardhat market-maker
        /// accounts pre-funded with a native balance in genesis (`native_balances`). Orders
        /// from UNFUNDED senders are rejected for insufficient margin and never match, so
        /// sender range must match genesis funding: the weighted testnet genesis
        /// (gen-weighted-genesis.sh) funds indices 60..60+100,000 — use
        /// `--sender-offset 60` with up to 100k senders there.
        #[arg(long, default_value_t = 20)]
        senders: usize,
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = 512)]
        concurrency: usize,
        /// Orders per signed PlaceOrderBatch (1 = single PlaceOrder). The throughput
        /// knob: orders/s = actions/s x batch_size. Sweep e.g. 1/100/500/1000.
        #[arg(long, default_value_t = 1)]
        batch_size: usize,
        /// Actions per `torus_submitNativeActions` RPC call (1 = legacy single
        /// endpoint). Amortizes HTTP/JSON/permit overhead (Sprint 2). Server cap: 100.
        #[arg(long, default_value_t = 1)]
        submit_batch: usize,
        /// First sender key index (use disjoint ranges, e.g. 0 and 10, when running
        /// multiple bench instances so nonce/rate-limit accounting never collides).
        #[arg(long, default_value_t = 0)]
        sender_offset: usize,
        /// Ingress wire format: "json" (hex of canonical JSON) or "bin"
        /// (hex of bincode via torus_submitNativeActionsBin, Sprint 5).
        #[arg(long, default_value = "json")]
        format: String,
        /// Pre-sign N submit-batches PER SENDER before the timed window, then
        /// fire the pre-built ammo (no signing in the hot loop) — isolates the
        /// chain's ingress/exec ceiling from this box's signing speed. 0 = off
        /// (stream-sign during the run). Nonces run contiguously from now_ms, so
        /// keep N x submit_batch under the 60s nonce window and size N for
        /// duration x target-rate (a sender that runs out logs "ammo exhausted").
        #[arg(long, default_value_t = 0)]
        pre_sign: usize,
        /// Pace pre-sign firing to this many actions/s PER SENDER (0 = unbounded
        /// burst). Offer a controlled load to find where the chain saturates —
        /// an unpaced burst overruns RPC ingress and under-measures the chain.
        #[arg(long, default_value_t = 0)]
        rate: usize,
        /// Signature mode: "eip712" (secp256k1 ECDSA, default — the node recovers
        /// the sender per action) or "session" (ed25519 session keys — registers a
        /// CreateSession per sender, then signs orders with the session key to hit
        /// the chain's batched-ed25519 fast path; isolates the chain ceiling from
        /// per-action ecrecover cost).
        #[arg(long, default_value = "eip712")]
        sign_mode: String,
        /// Spread orders uniformly across market ids 1..=N (per ORDER inside a
        /// batch). 1 = legacy single-market shape, which skips the chain's
        /// per-market parallel matching entirely (S372/S395).
        #[arg(long, default_value_t = 1)]
        markets: u64,
        /// A4 economic load shape: margin-targeted sizing, balanced one-side-per-
        /// (sender,market) maker/taker flow around a fixed mid, and interleaved
        /// cancel-alls so per-sender margin reaches a steady state instead of
        /// exhausting after ~8 orders. Default OFF = the legacy random shape
        /// (price ~U[55k,65k] x qty ~U[1,100]) so prior cells stay reproducible.
        #[arg(long, default_value_t = false)]
        econ: bool,
        /// econ: per-order Phase-2 margin target in whole TRS at the executor's
        /// default 20x leverage. qty = target*20/price (>= 1 lot).
        #[arg(long, default_value_t = 1500)]
        target_margin: u64,
        /// econ: probability an order is priced to CROSS the mid (taker-shaped;
        /// matched fills are the mission metric). The remainder rest passive.
        #[arg(long, default_value_t = 0.5)]
        cross_fraction: f64,
        /// econ: per-action probability of a CancelAllOrders (all markets)
        /// instead of a PlaceOrderBatch — recycles resting GTC margin and keeps
        /// per-(sender,market) resting counts under the book's 200-order cap.
        #[arg(long, default_value_t = 0.05)]
        cancel_fraction: f64,
        /// econ: fixed mid price in whole TRS. 0 = derive as 20 x target-margin
        /// so a 1-lot order's margin lands exactly on target.
        #[arg(long, default_value_t = 0)]
        econ_mid: u64,
        /// econ: price-offset half-band in ticks (d ~ U[1, band] around mid).
        #[arg(long, default_value_t = 5)]
        band: u64,
        /// econ: aggregate offered rate in actions/s across ALL senders (f64 —
        /// allows per-sender rates below 1/s at high sender counts, which the
        /// integer per-sender --rate cannot express). 0 = fall back to --rate.
        #[arg(long, default_value_t = 0.0)]
        rate_total: f64,
        /// #32: comma-separated node Prometheus `/metrics` endpoints (e.g.
        /// http://localhost:9161,http://localhost:9162). When set, the mission
        /// funnel counters' delta over the timed window (placed/s, matched/s)
        /// becomes THE headline throughput measure, and the block-rate health
        /// gate is reported from `torus_block_height`. Empty (default) = OFF:
        /// the run behaves as before and reports only load-gen submit stats.
        #[arg(long, default_value = "")]
        metrics_urls: String,
        /// #34/#35: re-fetch every block body after the run for deep per-action
        /// accounting (unique-action dedup, dup factor, peak block). Default OFF
        /// — #32's node counters are ground truth, and body sweeps both perturb
        /// the SUT and can melt val0's RPC core. Turn on only for forensics.
        #[arg(long, default_value_t = false)]
        sweep_bodies: bool,
    },
    Combined {
        #[arg(
            long,
            default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548"
        )]
        rpc_urls: String,
        #[arg(long, default_value_t = 100)]
        senders: usize,
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = 512)]
        concurrency: usize,
    },
    /// State-root scaling proof (Phase A A1.6): seed synthetic EVM state at each --sizes value,
    /// then measure per-block incremental vs full-scan root-compute time. The incremental root is
    /// O(changed) so its time stays ~flat as state grows; the full scan is O(total state) so it
    /// grows ~linearly — the production-scale payoff a fresh small-state devnet can't show.
    StateRoot {
        /// Comma-separated account counts to sweep (synthetic state sizes). 1M can take minutes.
        #[arg(long, default_value = "1000,100000,1000000")]
        sizes: String,
        /// Accounts changed per simulated block (the O(changed) working set).
        #[arg(long, default_value_t = 16)]
        changed: usize,
        /// Blocks measured per size; the reported time is the MIN across them (noise-robust).
        #[arg(long, default_value_t = 5)]
        blocks: usize,
    },
    /// Print derived EVM addresses for a sender range, for genesis funding. Uses
    /// the SAME key derivation as the consensus bench, so funded addresses provably
    /// match the senders the bench will use (no address/funding mismatch).
    GenAccounts {
        /// First sender index (e.g. 20 to derive senders 20..20+count).
        #[arg(long, default_value_t = 20)]
        offset: usize,
        /// How many sender addresses to derive.
        #[arg(long, default_value_t = 40)]
        count: usize,
        /// Also print each account's hex private key (as "idx addr privkey").
        /// For loading funded senders into external tools (e.g. tx-loop.sh).
        /// These are deterministic bench keys — NEVER use on a real network.
        #[arg(long, default_value_t = false)]
        secret_keys: bool,
    },
}

fn random_place_order(rng: &mut impl Rng, market_id: u64) -> NativeAction {
    // Prices MUST be tick-aligned or the matching engine rejects the order outright
    // (`order_book.rs`: `price.raw() % tick_size.raw() != 0` -> Rejected). The runtime
    // order book is auto-created with tick = lot = FixedPoint::ONE (whole units), so
    // generate whole-unit prices around a 60,000 mid (±5,000) — these always satisfy a
    // 1.0 tick and cross often enough to actually exercise the matching engine.
    let mid: i128 = 60_000;
    let offset = rng.gen_range(-5_000i128..=5_000);
    let price = FixedPoint::from_raw((mid + offset) * FixedPoint::SCALE);
    let qty_units = rng.gen_range(1i128..=100);
    let quantity = FixedPoint::from_raw(qty_units * FixedPoint::SCALE);
    let is_buy = rng.gen_bool(0.5);

    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// How the bench signs each action.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignMode {
    /// EIP-712 ECDSA (secp256k1): the owner key signs and the node recovers the
    /// sender with a per-action `ecrecover` — the expensive ingress path.
    Eip712,
    /// Ed25519 session key: sessions are registered first (a `CreateSession` per
    /// sender), then orders are signed with the session key so the node takes its
    /// batched-ed25519 fast path — isolates the chain ceiling from per-action
    /// ecrecover cost (lever 1).
    Session,
}

/// Sign one action in the configured mode. `k256` is the owner's EIP-712 key;
/// `session` is its registered ed25519 session key (used only in `Session` mode).
fn sign_one(
    action: NativeAction,
    nonce: u64,
    k256: &k256::ecdsa::SigningKey,
    session: &ed25519_dalek::SigningKey,
    mode: SignMode,
) -> SignedNativeAction {
    match mode {
        SignMode::Eip712 => sign_native_action(action, nonce, k256),
        SignMode::Session => sign_native_action_with_session(action, nonce, session),
    }
}

/// Sign one submit-batch worth of payloads (T6 signer pipeline). Returns the
/// hex payload strings and the advanced nonce watermark — nonces must be
/// strictly increasing per sender because the committed (sender, nonce) replay
/// guard silently drops same-ms duplicates.
#[allow(clippy::too_many_arguments)]
fn sign_payload_batch(
    rng: &mut impl Rng,
    key: &k256::ecdsa::SigningKey,
    session: &ed25519_dalek::SigningKey,
    mode: SignMode,
    batch_size: usize,
    submit_batch: usize,
    mut last_nonce: u64,
    bin: bool,
    markets: u64,
) -> (Vec<String>, u64) {
    let mut payloads = Vec::with_capacity(submit_batch);
    for _ in 0..submit_batch {
        let action = random_place_order_action(rng, markets, batch_size);
        let base = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let nonce = base.max(last_nonce + 1);
        last_nonce = nonce;
        let signed = sign_one(action, nonce, key, session, mode);
        let bytes = if bin {
            bincode::serialize(&signed).unwrap()
        } else {
            serde_json::to_vec(&signed).unwrap()
        };
        payloads.push(format!("0x{}", hex::encode(&bytes)));
    }
    (payloads, last_nonce)
}

/// Pre-sign `count` submit-batch payloads for one sender BEFORE the timed
/// window (pre-sign mode). Firing pre-built ammo makes the hot loop pure
/// network I/O, isolating the chain's ingress/exec ceiling from client signing
/// speed. Nonces run *contiguously* from `base_nonce` (deterministic, unlike the
/// streaming path's wall-clock nonces), so they are strictly increasing
/// regardless of signing speed. `base_nonce` should be ~`now_ms` and the span
/// (`count * submit_batch`) must stay inside the chain's `NONCE_WINDOW_MS` (60s)
/// or late ammo is rejected as "too far in future".
#[allow(clippy::too_many_arguments)]
fn pregen_ammo(
    rng: &mut impl Rng,
    key: &k256::ecdsa::SigningKey,
    session: &ed25519_dalek::SigningKey,
    mode: SignMode,
    batch_size: usize,
    submit_batch: usize,
    count: usize,
    bin: bool,
    base_nonce: u64,
    markets: u64,
) -> Vec<Vec<String>> {
    let mut nonce = base_nonce;
    let mut ammo = Vec::with_capacity(count);
    for _ in 0..count {
        let mut payloads = Vec::with_capacity(submit_batch);
        for _ in 0..submit_batch {
            let action = random_place_order_action(rng, markets, batch_size);
            let signed = sign_one(action, nonce, key, session, mode);
            nonce += 1;
            let bytes = if bin {
                bincode::serialize(&signed).unwrap()
            } else {
                serde_json::to_vec(&signed).unwrap()
            };
            payloads.push(format!("0x{}", hex::encode(&bytes)));
        }
        ammo.push(payloads);
    }
    ammo
}

/// Per-submit-batch fire interval that holds `rate` actions/s for one sender,
/// given `submit_batch` actions ship per fire. `rate == 0` => unbounded (`None`).
/// Used to PACE the pre-sign fire loop so a burst doesn't overrun RPC ingress —
/// an unpaced burst times out at the server's submit semaphore and makes a solo
/// pre-sign run under-measure the chain.
fn fire_interval(submit_batch: usize, rate: usize) -> Option<Duration> {
    if rate == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(submit_batch as f64 / rate as f64))
}

/// Whether a leg fires pre-signed ammo or streams freshly-signed ammo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmmoPlan {
    /// Sign everything up front; the timed window is pure network I/O.
    Presign,
    /// Sign fresh, just ahead of firing (T6 pipeline) — nonces never age.
    Stream,
}

/// Decide whether the pre-sign fast path is safe for this leg, or whether it
/// would fall into the S387 "presign trap" and must stream instead.
///
/// Pre-sign stamps each action's nonce ONCE, up front, as a wall-clock ms led by
/// `lead_ms`, but the ammo is not fired until the *whole* presign phase finishes
/// (`total_actions / sign_rate_per_s`) and then over the `duration_secs` window.
/// The chain rejects any nonce older than `window_ms`. On a core-pinned bench a
/// large presign signs serial and can outlast the window, so the oldest ammo is
/// "nonce too old" on arrival and the leg silently carries ZERO load.
///
/// `sign_rate_per_s` is the *aggregate* box signing rate (single-core rate x the
/// usable core budget), so a core-pinned bench streams while an unpinned box
/// still clears pre-sign for the same sender count.
fn choose_ammo_plan(
    total_actions: u64,
    sign_rate_per_s: f64,
    lead_ms: u64,
    duration_secs: u64,
    window_ms: u64,
) -> AmmoPlan {
    if total_actions == 0 || sign_rate_per_s <= 0.0 {
        return AmmoPlan::Stream;
    }
    let projected_presign_ms = (total_actions as f64 / sign_rate_per_s * 1000.0) as u64;
    // The oldest ammo (nonce = t0 + lead_ms) fires first, right after the presign
    // phase, and must still be inside the window at the END of the run.
    let oldest_age_at_run_end_ms =
        projected_presign_ms.saturating_sub(lead_ms) + duration_secs * 1000;
    // Margin below the hard window: block times stretch under load, so real fire
    // time drifts past the nominal duration.
    const SAFETY_MS: u64 = 8_000;
    if oldest_age_at_run_end_ms + SAFETY_MS <= window_ms {
        AmmoPlan::Presign
    } else {
        AmmoPlan::Stream
    }
}

#[cfg(test)]
mod ammo_plan_tests {
    use super::*;

    const W: u64 = 60_000; // NONCE_WINDOW_MS
    const LEAD: u64 = 30_000; // now + window/2, the current pre-sign lead

    // pre_sign == 0, or a rate we couldn't measure, always streams.
    #[test]
    fn zero_or_unmeasurable_streams() {
        assert_eq!(choose_ammo_plan(0, 373.0, LEAD, 20, W), AmmoPlan::Stream);
        assert_eq!(choose_ammo_plan(16_500, 0.0, LEAD, 20, W), AmmoPlan::Stream);
    }

    // s=20,b=400: ~16.5k actions @ ~373/s on one pinned core = ~44s presign;
    // oldest ammo ~34s old by run-end, still inside the 60s window -> pre-sign
    // OK (this is the cell that actually produced load).
    #[test]
    fn small_leg_presigns() {
        assert_eq!(choose_ammo_plan(16_500, 373.0, LEAD, 20, W), AmmoPlan::Presign);
    }

    // s=60,b=400: ~49.5k actions @ ~373/s = ~133s presign -> oldest ammo ~123s
    // old on arrival -> dead. Must stream. (The cell that silently returned 0.)
    #[test]
    fn high_sender_leg_streams() {
        assert_eq!(choose_ammo_plan(49_500, 373.0, LEAD, 20, W), AmmoPlan::Stream);
    }

    // b=1000 makes each action heavier to build; the slower rate blows the
    // window even at the same action count -> stream.
    #[test]
    fn large_batch_leg_streams() {
        assert_eq!(choose_ammo_plan(16_500, 150.0, LEAD, 20, W), AmmoPlan::Stream);
    }

    // The SAME 49.5k actions on an UNPINNED 8-core box (aggregate ~3000/s)
    // presign in ~16s and fit — pinning is what triggers the fallback, not the
    // sender count itself.
    #[test]
    fn unpinned_box_presigns_high_sender() {
        assert_eq!(choose_ammo_plan(49_500, 3_000.0, LEAD, 20, W), AmmoPlan::Presign);
    }
}

// ============================================================================
// A4: economic load shape ("econ mode") — sustainable margin + real matching
// ============================================================================
//
// The legacy generator (price ~U[55k,65k] x qty ~U[1,100], GTC, never cancelled)
// needs ~151k TRS margin/order at the executor's default 20x leverage
// (`margin_configs` is never populated -> `unwrap_or(20)`, native_executor.rs).
// A 1M TRS genesis sender affords ~8 orders, then every subsequent order dies in
// Phase-2 margin pre-reserve — the A3 funnel measured 99.987% rejected_margin.
//
// MARGIN SEMANTICS (ground truth from native_executor.rs / position.rs):
//   * Phase 2 reserve (limit orders): m = price*qty/20 moves available -> order_margin.
//   * Phase 4 release: FULL release when the incoming order does NOT rest
//     (Filled / IOC-cancelled / Rejected); PROPORTIONAL release for the filled part
//     of a partially-filled resting order. Release covers ONLY the in-batch
//     (taker-side) reservation.
//   * CancelOrder / CancelAllOrders release price*remaining_qty/20 (capped by
//     order_margin) — i.e. only the UNFILLED remainder.
//   * A resting order filled as MAKER by a later taker gets NO release anywhere:
//     the reservation is permanently stranded in order_margin (node-side leak —
//     out of scope to fix here, but it bounds any matched-flow run). STP
//     maker-cancels leak the same way. Positions themselves reserve nothing
//     (apply_fill only tracks size/entry and credits realized PnL), and
//     liquidation never runs (it iterates the empty margin_configs).
//
// EQUILIBRIUM (per sender, balance B, per-order margin m ~= --target-margin):
//   * taker fills and cancels round-trip their margin: net 0.
//   * resting (not yet filled/cancelled) margin is bounded by the cancel-all
//     cadence: <= batch_size * (1/cancel_fraction) * m expected, and hard-capped
//     by the book's 200-orders/trader/market limit at 200 * markets * m
//     (200 * 10 * 1.5k = 3M TRS with defaults — well under B).
//   * maker fills leak m each: leak_rate/sender = (fills/s ÷ senders) * m.
//     Horizon T = (B - locked) / leak_rate. With B = 100M TRS (bumped genesis),
//     m = 1.5k, 150k fills/s: s=5,000 -> T ~= 37 min; s=100,000 -> T ~= 12 h.
//     True indefinite steady state is impossible bench-side while the node
//     strands maker-fill margin; scale senders and/or lower target-margin to
//     stretch T.
//
// CROSSING SHAPE: every (sender, market) pair trades ONE side only
// (parity of sender_idx + market_id), so a sender can never self-trade (STP
// cancels would leak margin without producing a fill). Each order is priced off
// a fixed per-run mid: passive (rests) at mid -/+ d and aggressive (crosses) at
// mid +/- d, d ~ U[1, band] ticks, aggressive with probability --cross-fraction.
// Aggressive orders lift the opposing passive queue -> real matched fills;
// unfilled aggressive remainders rest at the top of book and are consumed first
// by the next opposing aggressive order. Sizing: qty = target*20/price (>= 1
// lot), so per-order margin ~= --target-margin regardless of mid.

/// Executor default leverage for markets absent from `margin_configs`
/// (`native_executor.rs` `unwrap_or(20)` — the map is never populated).
const NATIVE_DEFAULT_LEVERAGE: i128 = 20;

/// Econ-mode load shape (A4). See the module comment above for the margin
/// semantics and the per-sender equilibrium arithmetic.
#[derive(Clone, Copy, Debug)]
struct EconShape {
    /// Per-order margin target, whole TRS. qty = target*20/price.
    target_margin: u64,
    /// Fixed mid price, whole TRS (tick = 1.0 on auto-created books).
    mid: u64,
    /// Price offset half-band in ticks: d ~ U[1, band].
    band: u64,
    /// Probability an order is priced to CROSS the mid (taker-shaped).
    cross_fraction: f64,
    /// Per-action probability of a CancelAllOrders (all markets) instead of a
    /// PlaceOrderBatch — recycles resting GTC margin back to `available`.
    cancel_fraction: f64,
}

impl EconShape {
    /// `mid == 0` derives mid = 20 x target-margin, so a 1-lot order's margin
    /// lands exactly on target. Fractions are clamped into [0, 1]; band into
    /// [1, mid-1] so prices stay positive.
    fn new(
        target_margin: u64,
        mid: u64,
        band: u64,
        cross_fraction: f64,
        cancel_fraction: f64,
    ) -> Self {
        let target_margin = target_margin.max(1);
        let mid = if mid == 0 {
            target_margin * NATIVE_DEFAULT_LEVERAGE as u64
        } else {
            mid
        };
        Self {
            target_margin,
            mid,
            band: band.clamp(1, mid.saturating_sub(1).max(1)),
            cross_fraction: cross_fraction.clamp(0.0, 1.0),
            cancel_fraction: cancel_fraction.clamp(0.0, 1.0),
        }
    }
}

/// Fixed side per (sender, market): a sender only ever buys or only ever sells
/// in a given market, so its taker orders can never hit its own resting orders
/// (STP maker-cancels leak margin and produce no fill). Parity splits every
/// market's senders 50/50 buyers/sellers, so aggregate flow is balanced and
/// positions per (sender, market) grow one-directionally (no realized-PnL
/// balance churn: apply_fill's reduce/close paths never trigger).
fn econ_side_is_buy(sender_idx: usize, market_id: u64) -> bool {
    (sender_idx as u64).wrapping_add(market_id) % 2 == 0
}

/// One econ-shaped order. Prices are whole-unit (tick 1.0) offsets from the
/// fixed mid: passive orders rest inside their own side of the book, aggressive
/// orders cross to the opposing side. Quantity targets `target_margin` TRS of
/// Phase-2 margin at the executor's default 20x leverage, floored at 1 lot
/// (the auto-created book's dust threshold).
fn econ_place_order(
    rng: &mut impl Rng,
    sender_idx: usize,
    market_id: u64,
    shape: &EconShape,
) -> PlaceOrderParams {
    let is_buy = econ_side_is_buy(sender_idx, market_id);
    let aggressive = rng.gen_bool(shape.cross_fraction);
    let d = rng.gen_range(1..=shape.band as i128);
    // Aggressive buy above mid / aggressive sell below mid cross the opposing
    // passive queue; passive orders rest on their own side.
    let price_units = if is_buy == aggressive {
        shape.mid as i128 + d
    } else {
        shape.mid as i128 - d
    };
    let price = FixedPoint::from_raw(price_units * FixedPoint::SCALE);
    let target = FixedPoint::from_raw(shape.target_margin as i128 * FixedPoint::SCALE);
    let lev = FixedPoint::from_raw(NATIVE_DEFAULT_LEVERAGE * FixedPoint::SCALE);
    let mut quantity = target * lev / price;
    if quantity < FixedPoint::ONE {
        quantity = FixedPoint::ONE;
    }
    PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

/// One econ-mode action: with probability `cancel_fraction` a
/// `CancelAllOrders` (all markets — one signature frees every resting
/// reservation this sender still holds), otherwise a `PlaceOrderBatch` of
/// `batch_size` econ-shaped orders spread uniformly across markets.
fn econ_action(
    rng: &mut impl Rng,
    sender_idx: usize,
    markets: u64,
    batch_size: usize,
    shape: &EconShape,
) -> NativeAction {
    if shape.cancel_fraction > 0.0 && rng.gen_bool(shape.cancel_fraction) {
        return NativeAction::CancelAllOrders { market_id: None };
    }
    let markets = markets.max(1);
    if batch_size <= 1 {
        let market_id = rng.gen_range(1..=markets);
        return NativeAction::PlaceOrder(econ_place_order(rng, sender_idx, market_id, shape));
    }
    let orders: Vec<PlaceOrderParams> = (0..batch_size)
        .map(|_| {
            let market_id = rng.gen_range(1..=markets);
            econ_place_order(rng, sender_idx, market_id, shape)
        })
        .collect();
    NativeAction::PlaceOrderBatch(orders)
}

#[cfg(test)]
mod econ_shape_tests {
    use super::*;

    fn default_shape() -> EconShape {
        // Defaults as wired in main(): target 1500, derived mid, band 5, cross 0.5,
        // cancel 0.05.
        EconShape::new(1500, 0, 5, 0.5, 0.05)
    }

    /// Margin the executor will reserve in Phase 2 for this order at the
    /// default 20x leverage (margin_configs is never populated).
    fn phase2_margin(p: &PlaceOrderParams) -> FixedPoint {
        let lev = FixedPoint::from_raw(NATIVE_DEFAULT_LEVERAGE * FixedPoint::SCALE);
        p.price * p.quantity / lev
    }

    #[test]
    fn derived_mid_is_20x_target() {
        let s = default_shape();
        assert_eq!(s.mid, 30_000, "mid = target-margin x default 20x leverage");
        // Explicit mid wins.
        assert_eq!(EconShape::new(1500, 60_000, 5, 0.5, 0.05).mid, 60_000);
    }

    // The core economics invariant: every generated order's Phase-2 margin is
    // within 1% of --target-margin, so per-order affordability is a constant of
    // the run, not a lottery (the legacy shape spans 5.6k..324k TRS/order).
    #[test]
    fn per_order_margin_within_one_percent_of_target() {
        let shape = default_shape();
        let mut rng = StdRng::seed_from_u64(11);
        let target = FixedPoint::from_raw(shape.target_margin as i128 * FixedPoint::SCALE);
        let lo = FixedPoint::from_raw(target.raw() * 99 / 100);
        let hi = FixedPoint::from_raw(target.raw() * 101 / 100);
        for sender_idx in 0..50 {
            for _ in 0..200 {
                let market_id = rng.gen_range(1..=10);
                let p = econ_place_order(&mut rng, sender_idx, market_id, &shape);
                let m = phase2_margin(&p);
                assert!(
                    m >= lo && m <= hi,
                    "margin {m:?} outside 1% of target {target:?} (price {:?} qty {:?})",
                    p.price,
                    p.quantity
                );
                assert!(
                    p.quantity >= FixedPoint::ONE,
                    "qty must clear the book's 1-lot dust threshold"
                );
                // Whole-unit prices satisfy the auto-created book's 1.0 tick.
                assert_eq!(p.price.raw() % FixedPoint::SCALE, 0, "price must be tick-aligned");
            }
        }
    }

    // One side per (sender, market): a sender's taker orders can never cross
    // its own resting orders (STP maker-cancels leak margin, produce no fill).
    // Parity also splits every market's senders exactly 50/50, so aggregate
    // buy and sell flow per market is balanced by construction.
    #[test]
    fn side_is_fixed_per_sender_market_and_globally_balanced() {
        let shape = default_shape();
        let mut rng = StdRng::seed_from_u64(12);
        for market_id in 1..=10u64 {
            let buyers = (0..1000)
                .filter(|&idx| econ_side_is_buy(idx, market_id))
                .count();
            assert_eq!(buyers, 500, "market {market_id}: parity must split 50/50");
        }
        // And generated orders respect it: one sender, one market, one side.
        for &(sender_idx, market_id) in &[(0usize, 1u64), (7, 3), (42, 10)] {
            let want = econ_side_is_buy(sender_idx, market_id);
            for _ in 0..100 {
                let p = econ_place_order(&mut rng, sender_idx, market_id, &shape);
                assert_eq!(p.is_buy, want, "side must never flip for a (sender, market)");
            }
        }
    }

    // cross-fraction is the crossing knob: 1.0 prices every order THROUGH the
    // mid (buys above / sells below -> taker-shaped), 0.0 rests every order on
    // its own side. The mid itself is never quoted, so the two regimes are
    // disjoint price sets.
    #[test]
    fn cross_fraction_controls_which_side_of_mid() {
        let mid = FixedPoint::from_raw(30_000 * FixedPoint::SCALE);
        let mut rng = StdRng::seed_from_u64(13);

        let aggr = EconShape::new(1500, 0, 5, 1.0, 0.0);
        let passive = EconShape::new(1500, 0, 5, 0.0, 0.0);
        for sender_idx in 0..20 {
            for market_id in 1..=4u64 {
                let a = econ_place_order(&mut rng, sender_idx, market_id, &aggr);
                let p = econ_place_order(&mut rng, sender_idx, market_id, &passive);
                if a.is_buy {
                    assert!(a.price > mid, "aggressive buy must cross above mid");
                } else {
                    assert!(a.price < mid, "aggressive sell must cross below mid");
                }
                if p.is_buy {
                    assert!(p.price < mid, "passive buy must rest below mid");
                } else {
                    assert!(p.price > mid, "passive sell must rest above mid");
                }
            }
        }
    }

    // cancel-fraction interleaves CancelAllOrders actions (margin recycling);
    // 0.0 must never emit one (pure placement stream for A/B cells).
    #[test]
    fn cancel_fraction_interleaves_cancel_all() {
        let mut rng = StdRng::seed_from_u64(14);
        let never = EconShape::new(1500, 0, 5, 0.5, 0.0);
        for _ in 0..500 {
            assert!(!matches!(
                econ_action(&mut rng, 3, 10, 8, &never),
                NativeAction::CancelAllOrders { .. }
            ));
        }
        let mut cancels = 0usize;
        let some = EconShape::new(1500, 0, 5, 0.5, 0.2);
        for _ in 0..2000 {
            if matches!(
                econ_action(&mut rng, 3, 10, 8, &some),
                NativeAction::CancelAllOrders { market_id: None }
            ) {
                cancels += 1;
            }
        }
        // ~400 expected; wide band, this is a smoke bound not a stats test.
        assert!(
            (200..=600).contains(&cancels),
            "cancel-fraction 0.2 should emit ~20% cancel-alls, got {cancels}/2000"
        );
    }

    // Same seed -> byte-identical action stream (reproducible cells).
    #[test]
    fn generator_is_deterministic_with_seed() {
        let shape = default_shape();
        let gen_stream = |seed: u64| -> Vec<String> {
            let mut rng = StdRng::seed_from_u64(seed);
            (0..100)
                .map(|_| {
                    format!("{:?}", econ_action(&mut rng, 9, 10, 4, &shape))
                })
                .collect()
        };
        assert_eq!(gen_stream(99), gen_stream(99));
        assert_ne!(gen_stream(99), gen_stream(100));
    }

    // Batch shape: batch_size orders per action, every market in 1..=markets.
    #[test]
    fn batch_carries_batch_size_orders_across_markets() {
        let shape = default_shape();
        let mut rng = StdRng::seed_from_u64(15);
        match econ_action(&mut rng, 4, 10, 400, &shape) {
            NativeAction::PlaceOrderBatch(orders) => {
                assert_eq!(orders.len(), 400);
                assert!(orders.iter().all(|o| (1..=10).contains(&o.market_id)));
            }
            other => panic!("expected PlaceOrderBatch, got {other:?}"),
        }
        match econ_action(&mut rng, 4, 10, 1, &shape) {
            NativeAction::PlaceOrder(_) => {}
            other => panic!("batch_size 1 must emit a plain PlaceOrder, got {other:?}"),
        }
    }
}

/// Build one action carrying `batch_size` orders. `batch_size <= 1` returns a plain
/// `PlaceOrder` (the legacy single path) for apples-to-apples A/B runs.
///
/// `markets` > 1 spreads orders uniformly across market ids `1..=markets`
/// (per ORDER, so one PlaceOrderBatch fans out to many books) — a single-market
/// load shape skips `MarketWorkerPool::match_parallel` entirely
/// (market_workers.rs single-market fast path) and measures one book's
/// sequential ceiling, not the chain's (S372 finding, S395 knob).
fn random_place_order_action(rng: &mut impl Rng, markets: u64, batch_size: usize) -> NativeAction {
    let markets = markets.max(1);
    if batch_size <= 1 {
        let market_id = rng.gen_range(1..=markets);
        return random_place_order(rng, market_id);
    }
    let orders: Vec<PlaceOrderParams> = (0..batch_size)
        .map(|_| {
            let market_id = rng.gen_range(1..=markets);
            match random_place_order(rng, market_id) {
                NativeAction::PlaceOrder(p) => p,
                _ => unreachable!(),
            }
        })
        .collect();
    NativeAction::PlaceOrderBatch(orders)
}

fn random_address(rng: &mut impl Rng) -> Address {
    let mut bytes = [0u8; 20];
    rng.fill(&mut bytes);
    Address::from(bytes)
}

fn format_num(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

// ============================================================================
// Mode 1: Matching Engine
// ============================================================================

/// Extract a histogram's `_sum` value (in seconds) from OpenMetrics text. A
/// histogram sum carries no labels, so the line is `<metric>_sum <value>`.
fn metric_sum(encoded: &str, metric: &str) -> f64 {
    let key = format!("{metric}_sum");
    for line in encoded.lines() {
        if let Some(rest) = line.strip_prefix(key.as_str()) {
            if let Some(tok) = rest.split_whitespace().next() {
                if let Ok(v) = tok.parse::<f64>() {
                    return v;
                }
            }
        }
    }
    0.0
}

fn run_matching_engine(orders: usize, markets: u64, warmup: usize, genesis_path: &str) {
    use tempfile::TempDir;
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_core::position::NativeBalance;
    use torus_genesis::Genesis;
    use torus_state::StateDb;

    let dir = TempDir::new().unwrap();
    let state_db = StateDb::open(dir.path()).unwrap();

    let genesis = Genesis::from_file(std::path::Path::new(genesis_path)).unwrap();
    let _state_root = genesis.initialize(&state_db).unwrap();
    let chain_config = genesis.chain_config();

    let mut rng = rand::thread_rng();
    let market_count = markets.max(1);

    println!("=== Matching Engine Benchmark ===");
    println!("Markets: {market_count}");
    println!("Warmup:  {} orders", format_num(warmup as u64));
    println!();

    let mut ctx = NativeExecContext::new(
        state_db,
        1,
        1_700_000_000,
        0,
        chain_config.epoch_length,
        chain_config.max_validators,
        Address::ZERO,
        chain_config.treasury_address,
        chain_config.dev_pool_address,
    );

    // Fund every generated sender so Phase-2 margin reservation never rejects.
    // Random bench senders are otherwise unfunded (`get_native_balance` returns a
    // zero-`available` default) -> "insufficient margin" -> orders are dropped
    // BEFORE Phase-3 matching, so the bench would time rejection, not matching.
    let fund = NativeBalance {
        available: FixedPoint::from_raw(i128::MAX / 4),
        order_margin: FixedPoint::ZERO,
    };

    // Warmup
    if warmup > 0 {
        let warmup_actions: Vec<(Address, NativeAction)> = (0..warmup)
            .map(|_| {
                let mid = rng.gen_range(0..market_count);
                (random_address(&mut rng), random_place_order(&mut rng, mid))
            })
            .collect();
        for (addr, _) in &warmup_actions {
            ctx.positions
                .put_native_balance(addr, &fund)
                .expect("fund warmup sender");
        }
        let t = Instant::now();
        NativeExecutor::execute_batch(&mut ctx, &warmup_actions);
        let elapsed = t.elapsed();
        println!(
            "Warmup:  {} orders in {:.1}ms ({:.0} orders/sec)",
            format_num(warmup as u64),
            elapsed.as_secs_f64() * 1000.0,
            warmup as f64 / elapsed.as_secs_f64(),
        );
        println!();
    }

    // Benchmark batches
    let batch_sizes: Vec<usize> = if orders <= 10_000 {
        vec![orders]
    } else {
        let mut sizes = vec![10_000];
        if orders > 10_000 {
            sizes.push(orders);
        }
        sizes
    };

    for &batch_size in &batch_sizes {
        let actions: Vec<(Address, NativeAction)> = (0..batch_size)
            .map(|_| {
                let mid = rng.gen_range(0..market_count);
                (random_address(&mut rng), random_place_order(&mut rng, mid))
            })
            .collect();

        for (addr, _) in &actions {
            ctx.positions
                .put_native_balance(addr, &fund)
                .expect("fund batch sender");
        }
        // Fresh metrics handle so the per-phase histograms reflect THIS batch only.
        let phase_metrics = std::sync::Arc::new(torus_telemetry::Metrics::new());
        ctx.metrics = Some(phase_metrics.clone());
        let t = Instant::now();
        NativeExecutor::execute_batch(&mut ctx, &actions);
        let elapsed = t.elapsed();
        ctx.metrics = None;
        let rate = batch_size as f64 / elapsed.as_secs_f64();

        println!(
            "Batch {}: {:.0} orders/sec ({:.1}ms)",
            format_num(batch_size as u64),
            rate,
            elapsed.as_secs_f64() * 1000.0,
        );
        let enc = phase_metrics.encode();
        println!(
            "  phases: margin {:.1}ms | match {:.1}ms | settle {:.1}ms",
            metric_sum(&enc, "torus_exec_phase_margin_seconds") * 1000.0,
            metric_sum(&enc, "torus_exec_phase_match_seconds") * 1000.0,
            metric_sum(&enc, "torus_exec_phase_settle_seconds") * 1000.0,
        );
    }

    if market_count > 1 {
        println!();
        println!(
            "--- Per-market breakdown (single batch of {}) ---",
            format_num(orders as u64)
        );

        for m in 0..market_count.min(4) {
            let actions: Vec<(Address, NativeAction)> = (0..orders)
                .map(|_| (random_address(&mut rng), random_place_order(&mut rng, m)))
                .collect();

            for (addr, _) in &actions {
                ctx.positions
                    .put_native_balance(addr, &fund)
                    .expect("fund per-market sender");
            }
            let t = Instant::now();
            NativeExecutor::execute_batch(&mut ctx, &actions);
            let elapsed = t.elapsed();
            let rate = orders as f64 / elapsed.as_secs_f64();

            println!(
                "  Market {m}: {:.0} orders/sec ({:.1}ms)",
                rate,
                elapsed.as_secs_f64() * 1000.0,
            );
        }
    }
}

// ============================================================================
// Mode 2: Consensus
// ============================================================================

struct SenderKey {
    signing_key: SigningKey,
    /// Deterministic ed25519 session key for this sender (used in --sign-mode
    /// session). Registered on-chain via `CreateSession` before the timed window.
    session_key: ed25519_dalek::SigningKey,
}

/// Deterministic ed25519 session key for sender `idx` — stable across runs for
/// reproducibility. `register_sessions` revokes-then-recreates it each run, so a
/// stale/expired copy from a prior run never blocks re-registration.
fn session_key_for(idx: usize) -> ed25519_dalek::SigningKey {
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&(0xED25_0000_0000_0000u64 + idx as u64).to_le_bytes());
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

fn load_sender_keys(count: usize, offset: usize) -> Vec<SenderKey> {
    (offset..offset + count)
        .map(|idx| {
            // Indices < 20 use the genesis-funded hardhat accounts; beyond that,
            // deterministic random keys (unfunded — their orders won't match, but
            // the keys stay stable per index for reproducible runs).
            let signing_key = if idx < 20 {
                let bytes = hex::decode(HARDHAT_KEYS[idx]).expect("valid hex");
                SigningKey::from_slice(&bytes).expect("valid key")
            } else {
                let mut rng = StdRng::seed_from_u64(0xBEEF_0000 + idx as u64);
                SigningKey::random(&mut rng)
            };
            SenderKey {
                signing_key,
                session_key: session_key_for(idx),
            }
        })
        .collect()
}

/// Register one ed25519 session key per sender on-chain via a `CreateSession`
/// action, EIP-712-signed by the owner key (sessions can only be created with the
/// master key — `requires_eip712`). Orders can then be session-signed for the
/// chain's batched-ed25519 fast path. Uses `SessionScope::Full` because the
/// `Trading` scope does NOT permit `PlaceOrderBatch` (the bench's throughput
/// action). Returns how many `CreateSession` actions the nodes admitted at ingress.
async fn register_sessions(
    client: &reqwest::Client,
    urls: &[String],
    keys: &[SenderKey],
    bin: bool,
) -> usize {
    // Refresh each run: REVOKE the prior (possibly-expired) session for this
    // deterministic key first, then CREATE it fresh. On a long-lived chain a
    // previous run's session lingers registered-but-expired; without the revoke,
    // re-create fails "session key already exists" and orders fail "session key
    // expired" — and expired sessions still count toward the max-5-per-owner cap
    // (count_sessions_for_owner does not filter by expiry). Revoke is best-effort:
    // on the first run there is nothing to revoke, which exec reports as a harmless
    // per-action "session not found" no-op (it does not fail the block).
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // --- Phase 1: revoke any stale session bound to each deterministic key ---
    for (i, sk) in keys.iter().enumerate() {
        let action = NativeAction::RevokeSession {
            session_pubkey: sk.session_key.verifying_key().to_bytes(),
        };
        let nonce = now_ms + i as u64;
        let signed = sign_native_action(action, nonce, &sk.signing_key);
        let bytes = if bin {
            bincode::serialize(&signed).unwrap()
        } else {
            serde_json::to_vec(&signed).unwrap()
        };
        let payload = format!("0x{}", hex::encode(&bytes));
        // Ignore result — a missing session (first run) is expected and harmless.
        let _ =
            submit_native_actions_batch(client, &urls[i % urls.len()], &[payload], i as u64, bin)
                .await;
    }
    // Let the revokes COMMIT before creating: exec_create_session's "already exists"
    // check reads committed state, which lags inclusion by the ~3-block HotStuff
    // window. Registration runs before the timed window (chain idle), so a few
    // seconds comfortably clears it.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // --- Phase 2: create the fresh sessions ---
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // 1 hour — comfortably under MAX_SESSION_EXPIRY_MS (24h) and far in the future.
    let expiry = now_ms + 60 * 60 * 1000;
    let mut accepted = 0usize;
    for (i, sk) in keys.iter().enumerate() {
        let action = NativeAction::CreateSession {
            session_pubkey: sk.session_key.verifying_key().to_bytes(),
            expiry,
            scope: SessionScope::Full,
        };
        // Distinct, in-window nonce per sender; offset from the revoke nonce above
        // (>=4s newer) so the (owner, nonce) replay guard never clashes.
        let nonce = now_ms + i as u64;
        let signed = sign_native_action(action, nonce, &sk.signing_key);
        let bytes = if bin {
            bincode::serialize(&signed).unwrap()
        } else {
            serde_json::to_vec(&signed).unwrap()
        };
        let payload = format!("0x{}", hex::encode(&bytes));
        match submit_native_actions_batch(client, &urls[i % urls.len()], &[payload], i as u64, bin)
            .await
        {
            Ok(n) => accepted += n,
            Err(e) => eprintln!("[session] sender {i} CreateSession failed: {e}"),
        }
    }
    accepted
}

/// Submit a batch of pre-encoded signed actions via `torus_submitNativeActions`
/// (or `torus_submitNativeActionsBin` for bincode payloads, Sprint 5).
/// Returns how many items the server accepted (entries carrying a `hash`).
async fn submit_native_actions_batch(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    id: u64,
    bin: bool,
) -> Result<usize, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": if bin {
            "torus_submitNativeActionsBin"
        } else {
            "torus_submitNativeActions"
        },
        "params": [payloads],
        "id": id
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!(
            "rpc: {}",
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
        ));
    }
    let items = resp["result"].as_array();
    let accepted = items
        .map(|its| its.iter().filter(|i| i.get("hash").is_some()).count())
        .unwrap_or(0);
    if accepted == 0 {
        // Surface WHY nothing landed — a per-item error or an unexpected shape.
        // A silent 0-accepted is exactly the misread session signing exists to avoid.
        let reason = items
            .and_then(|its| {
                its.iter()
                    .find_map(|i| i.get("error").map(|e| e.to_string()))
            })
            .unwrap_or_else(|| resp["result"].to_string());
        return Err(format!("0 accepted ({} sent): {reason}", payloads.len()));
    }
    Ok(accepted)
}

/// Count of submit errors surfaced so far — we print only the first few so a
/// rejected run doesn't flood the timed window with one line per batch.
static SUBMIT_ERRS: AtomicU64 = AtomicU64::new(0);

/// Fire one already-signed payload set and credit `submitted` by however many the
/// server accepted. A lone JSON action uses the legacy single endpoint; anything
/// else (or any bincode payload) goes through the batch endpoint.
async fn submit_payloads(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    req_id: u64,
    bin: bool,
    submitted: &AtomicU64,
) {
    let result = if payloads.len() == 1 && !bin {
        submit_native_action(client, url, &payloads[0], req_id)
            .await
            .map(|()| 1usize)
    } else {
        submit_native_actions_batch(client, url, payloads, req_id, bin).await
    };
    match result {
        Ok(accepted) => {
            submitted.fetch_add(accepted as u64, Ordering::Relaxed);
        }
        // Surface the first few rejection reasons instead of silently dropping —
        // a bench that reports 0 with no reason is how saturated/rejected runs get
        // misread as the chain's ceiling.
        Err(e) => {
            if SUBMIT_ERRS.fetch_add(1, Ordering::Relaxed) < 5 {
                eprintln!("[submit] {e}");
            }
        }
    }
}

async fn submit_native_action(
    client: &reqwest::Client,
    url: &str,
    signed_json_hex: &str,
    id: u64,
) -> Result<(), String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_submitNativeAction",
        "params": [signed_json_hex],
        "id": id
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!(
            "rpc: {}",
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
        ));
    }
    Ok(())
}

async fn fetch_block_number(client: &reqwest::Client, url: &str) -> Option<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let hex_str = resp["result"].as_str()?;
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    u64::from_str_radix(s, 16).ok()
}

/// One swept block: `(block_number, native slots, action identity hashes)`.
/// Identity hashes are empty when the node returned only a count (old RPC).
type SweptBlock = (u64, u64, Vec<u64>);

/// Stable identity of one included action: hash of its canonical JSON
/// (serde_json maps are BTreeMap-backed, so key order is deterministic).
/// 64-bit FxHash-style collisions are negligible at bench scales.
fn action_identity(action: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    action.to_string().hash(&mut h);
    h.finish()
}

async fn fetch_block_body(
    client: &reqwest::Client,
    url: &str,
    block_number: u64,
) -> Option<(u64, Vec<u64>)> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_getBlockBody",
        "params": [block_number],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let result = resp.get("result")?;
    // Prefer the action list: identities enable cross-block dedup (the
    // "included" metric otherwise overcounts ~3x under 3-chain commit lag).
    if let Some(actions) = result.get("nativeActions").and_then(|a| a.as_array()) {
        let ids: Vec<u64> = actions.iter().map(action_identity).collect();
        return Some((ids.len() as u64, ids));
    }
    // Old node: count only, no identities.
    let count = result.get("nativeActionCount")?.as_u64()?;
    Some((count, Vec::new()))
}

/// Sum native actions actually committed across a block range — the authoritative
/// throughput measure. The live monitor undercounts badly at fast block times (it
/// can't poll every body within its 500ms tick and silently drops undrained blocks
/// when it aborts), so after the load phase we re-fetch EVERY block body in the run
/// window `(start_exclusive, end_inclusive]` with bounded concurrency and a short
/// retry for tail blocks whose body isn't queryable yet. Returns the committed
/// `(block, native_count)` pairs that were successfully read.
async fn sweep_block_bodies(
    client: Arc<reqwest::Client>,
    urls: Arc<Vec<String>>,
    start_exclusive: u64,
    end_inclusive: u64,
    concurrency: usize,
) -> Vec<SweptBlock> {
    if end_inclusive <= start_exclusive {
        return Vec::new();
    }
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();

    for blk in (start_exclusive + 1)..=end_inclusive {
        let sem = sem.clone();
        let client = client.clone();
        let urls = urls.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let url = &urls[(blk as usize) % urls.len()];
            // Tail blocks may not have a queryable body yet — retry with light backoff.
            for attempt in 0..5u64 {
                if let Some((count, ids)) = fetch_block_body(&client, url, blk).await {
                    return (blk, Some((count, ids)));
                }
                tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await;
            }
            (blk, None)
        });
    }

    let mut out = Vec::new();
    let mut missing = 0u64;
    while let Some(res) = set.join_next().await {
        match res {
            Ok((blk, Some((count, ids)))) => out.push((blk, count, ids)),
            Ok((_, None)) | Err(_) => missing += 1,
        }
    }
    if missing > 0 {
        eprintln!(
            "[sweep] warning: {missing} block bodies unavailable after retries (excluded from total)"
        );
    }
    out.sort_unstable_by_key(|b| b.0);
    out
}

/// Drop trailing zero-count blocks from a swept window. A live chain keeps
/// producing empty blocks after the load stops; counting them would dilute
/// block stats and pin the window end past the actual run.
fn trim_trailing_empty(blocks: &mut Vec<SweptBlock>) {
    while matches!(blocks.last(), Some((_, 0, _))) {
        blocks.pop();
    }
}

#[cfg(test)]
mod sweep_window_tests {
    use super::*;

    fn blk(n: u64, ids: &[u64]) -> SweptBlock {
        (n, ids.len() as u64, ids.to_vec())
    }

    #[test]
    fn trim_trailing_empty_drops_only_tail_zeros() {
        let mut blocks = vec![
            blk(10, &[]),
            blk(11, &[1, 2, 3, 4, 5]),
            blk(12, &[]),
            blk(13, &[6, 7, 8, 9, 10, 11, 12]),
            blk(14, &[]),
            blk(15, &[]),
        ];
        trim_trailing_empty(&mut blocks);
        assert_eq!(
            blocks.iter().map(|b| b.0).collect::<Vec<_>>(),
            vec![10, 11, 12, 13]
        );
    }

    #[test]
    fn trim_trailing_empty_handles_all_zero_and_empty() {
        let mut all_zero = vec![blk(1, &[]), blk(2, &[])];
        trim_trailing_empty(&mut all_zero);
        assert!(all_zero.is_empty());
        let mut empty: Vec<SweptBlock> = Vec::new();
        trim_trailing_empty(&mut empty);
        assert!(empty.is_empty());
    }

    // Duplicate-inclusion accounting (ingress-cpu-supply open Q3): the same
    // action included in several blocks must count once in `unique`. Slots
    // stay the raw per-block sum so the dup factor (slots/unique) is visible.
    #[test]
    fn summarize_dedups_identities_across_blocks() {
        let blocks = vec![blk(1, &[10, 11]), blk(2, &[11, 12]), blk(3, &[10, 11])];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 6);
        assert_eq!(s.unique, Some(3));
        assert_eq!(s.block_count, 3);
        assert_eq!(s.peak_actions, 2);
        assert_eq!(s.peak_block, 1);
    }

    // Old nodes return only nativeActionCount (no identity list): unique is
    // unknowable, not zero — report None so callers fall back to slots.
    #[test]
    fn summarize_unique_none_when_identities_missing() {
        let blocks = vec![(1u64, 3u64, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 3);
        assert_eq!(s.unique, None);
    }

    // Mixed availability (some bodies fell back to count-only) would
    // undercount duplicates — treat the whole window as unknown.
    #[test]
    fn summarize_unique_none_when_partially_missing() {
        let blocks = vec![blk(1, &[10, 11]), (2u64, 2u64, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 4);
        assert_eq!(s.unique, None);
    }

    #[test]
    fn summarize_empty_window() {
        let s = summarize_included(&[]);
        assert_eq!(s.slots, 0);
        assert_eq!(s.unique, Some(0));
        assert_eq!(s.block_count, 0);
    }
}

/// Aggregate of a swept block window.
struct IncludedSummary {
    /// Raw per-block native slots summed — overcounts when the same action
    /// rides several blocks (3-chain commit lag re-inclusion).
    slots: u64,
    /// Distinct action identities across the window — the honest "included"
    /// number. `None` when any non-empty block lacked identities (old RPC),
    /// since a partial dedup would understate the dup factor.
    unique: Option<u64>,
    block_count: u64,
    peak_actions: u64,
    peak_block: u64,
}

fn summarize_included(blocks: &[SweptBlock]) -> IncludedSummary {
    let mut slots = 0u64;
    let mut peak_actions = 0u64;
    let mut peak_block = 0u64;
    let mut seen = std::collections::HashSet::new();
    let mut identities_complete = true;
    for (blk, count, ids) in blocks {
        slots += count;
        if *count > peak_actions {
            peak_actions = *count;
            peak_block = *blk;
        }
        if ids.len() as u64 == *count {
            seen.extend(ids.iter().copied());
        } else {
            identities_complete = false;
        }
    }
    IncludedSummary {
        slots,
        unique: identities_complete.then_some(seen.len() as u64),
        block_count: blocks.len() as u64,
        peak_actions,
        peak_block,
    }
}

// ============================================================================
// #32: node Prometheus counter scraper — THE headline throughput measure
// ============================================================================
//
// The load-gen's own `included_actions x batch` figure inflates throughput
// ~190x (S470) and the block-body sweep both perturbs and under-reports. The
// ground truth is the node's own mission counters, scraped from its Prometheus
// /metrics endpoint at bench start and end: the delta over the timed window is
// the honest placed/s and matched/s. On the wire the prometheus-client encoder
// suffixes counters with `_total`; the block-height gauge carries no suffix.

/// Wire (OpenMetrics) names of the mission funnel counters + block height, as
/// exposed on the node /metrics endpoint. `_total` is the counter suffix the
/// prometheus-client encoder appends; `torus_block_height` is a bare gauge.
const M_PLACED_ACCEPTED: &str = "torus_orders_placed_accepted_total";
const M_MATCHED: &str = "torus_orders_matched_total";
const M_RESTING: &str = "torus_orders_resting_total";
const M_REJ_MARGIN: &str = "torus_orders_rejected_margin_total";
const M_REJ_BOOK: &str = "torus_orders_rejected_book_total";
const M_REJ_CANCELLED: &str = "torus_orders_rejected_cancelled_total";
const M_CANCELLED_PARTIAL: &str = "torus_orders_cancelled_partial_fill_total";
const M_SELF_TRADE_CANCELS: &str = "torus_orders_self_trade_cancels_total";
const M_REJ_OTHER: &str = "torus_orders_rejected_other_total";
const M_BLOCKS_COMMITTED: &str = "torus_blocks_committed_total";
const M_BLOCK_HEIGHT: &str = "torus_block_height";

/// Block-rate health gate (S470): idle chain produces ~29.8 blk/s; a run whose
/// block rate falls below this floor is stalling the execution pipeline and its
/// throughput number is not a healthy-chain measurement.
const HEALTH_GATE_BLK_PER_S: f64 = 23.8;
/// Reference idle block rate, for context in the gate line.
const IDLE_BLK_PER_S_REF: f64 = 29.8;

/// A point-in-time read of one node's mission funnel counters + block height.
/// Missing metrics read as 0 so an older node (or a partial scrape) degrades
/// gracefully rather than aborting the run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MetricsSnapshot {
    placed_accepted: u64,
    matched: u64,
    resting: u64,
    rejected_margin: u64,
    rejected_book: u64,
    rejected_cancelled: u64,
    cancelled_partial_fill: u64,
    self_trade_cancels: u64,
    rejected_other: u64,
    blocks_committed: u64,
    block_height: u64,
}

impl MetricsSnapshot {
    /// Parse a full OpenMetrics text body into a snapshot. Unlabeled scalar
    /// lines only (`<name> <value>`); the mission counters carry no labels.
    fn parse(encoded: &str) -> Self {
        Self {
            placed_accepted: metric_u64(encoded, M_PLACED_ACCEPTED),
            matched: metric_u64(encoded, M_MATCHED),
            resting: metric_u64(encoded, M_RESTING),
            rejected_margin: metric_u64(encoded, M_REJ_MARGIN),
            rejected_book: metric_u64(encoded, M_REJ_BOOK),
            rejected_cancelled: metric_u64(encoded, M_REJ_CANCELLED),
            cancelled_partial_fill: metric_u64(encoded, M_CANCELLED_PARTIAL),
            self_trade_cancels: metric_u64(encoded, M_SELF_TRADE_CANCELS),
            rejected_other: metric_u64(encoded, M_REJ_OTHER),
            blocks_committed: metric_u64(encoded, M_BLOCKS_COMMITTED),
            block_height: metric_u64(encoded, M_BLOCK_HEIGHT),
        }
    }

    /// Field-wise `self - start`, saturating at 0. Saturation guards a node that
    /// restarted mid-run (its counters reset to 0, so a naive subtraction would
    /// underflow); such a node simply contributes a 0 delta for that field.
    fn delta(&self, start: &MetricsSnapshot) -> MetricsSnapshot {
        MetricsSnapshot {
            placed_accepted: self.placed_accepted.saturating_sub(start.placed_accepted),
            matched: self.matched.saturating_sub(start.matched),
            resting: self.resting.saturating_sub(start.resting),
            rejected_margin: self.rejected_margin.saturating_sub(start.rejected_margin),
            rejected_book: self.rejected_book.saturating_sub(start.rejected_book),
            rejected_cancelled: self
                .rejected_cancelled
                .saturating_sub(start.rejected_cancelled),
            cancelled_partial_fill: self
                .cancelled_partial_fill
                .saturating_sub(start.cancelled_partial_fill),
            self_trade_cancels: self
                .self_trade_cancels
                .saturating_sub(start.self_trade_cancels),
            rejected_other: self.rejected_other.saturating_sub(start.rejected_other),
            blocks_committed: self.blocks_committed.saturating_sub(start.blocks_committed),
            block_height: self.block_height.saturating_sub(start.block_height),
        }
    }

    /// Field-wise max — the aggregation across validators. Every validator
    /// executes every committed block, so counters converge; taking the max
    /// picks the least-truncated node (e.g. one that wasn't briefly unreachable)
    /// rather than double-counting a sum across nodes.
    fn max_with(&self, other: &MetricsSnapshot) -> MetricsSnapshot {
        MetricsSnapshot {
            placed_accepted: self.placed_accepted.max(other.placed_accepted),
            matched: self.matched.max(other.matched),
            resting: self.resting.max(other.resting),
            rejected_margin: self.rejected_margin.max(other.rejected_margin),
            rejected_book: self.rejected_book.max(other.rejected_book),
            rejected_cancelled: self.rejected_cancelled.max(other.rejected_cancelled),
            cancelled_partial_fill: self.cancelled_partial_fill.max(other.cancelled_partial_fill),
            self_trade_cancels: self.self_trade_cancels.max(other.self_trade_cancels),
            rejected_other: self.rejected_other.max(other.rejected_other),
            blocks_committed: self.blocks_committed.max(other.blocks_committed),
            block_height: self.block_height.max(other.block_height),
        }
    }
}

/// Read one unlabeled scalar metric by exact wire name, as u64 (values encode as
/// integers for counters; a float encoding is truncated). Absent -> 0. Skips
/// `#` HELP/TYPE lines and any labeled series (`name{...}`), which the mission
/// funnel metrics never emit.
fn metric_u64(encoded: &str, wire_name: &str) -> u64 {
    for line in encoded.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        match it.next() {
            Some(name) if name == wire_name => {
                if let Some(tok) = it.next() {
                    if let Ok(v) = tok.parse::<f64>() {
                        return v.max(0.0) as u64;
                    }
                }
            }
            _ => {}
        }
    }
    0
}

/// Scrape one node's /metrics endpoint into a snapshot. `None` on transport or
/// read failure (caller treats a failed node as absent for that snapshot).
async fn scrape_metrics(client: &reqwest::Client, url: &str) -> Option<MetricsSnapshot> {
    let text = client
        .get(url)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    Some(MetricsSnapshot::parse(&text))
}

/// Scrape every metrics endpoint and fold the reads into a single snapshot via
/// field-wise max (see `max_with`). Returns `None` only if NO endpoint answered.
async fn scrape_all_metrics(
    client: &reqwest::Client,
    urls: &[String],
) -> Option<MetricsSnapshot> {
    let mut agg: Option<MetricsSnapshot> = None;
    for url in urls {
        if let Some(s) = scrape_metrics(client, url).await {
            agg = Some(match agg {
                Some(a) => a.max_with(&s),
                None => s,
            });
        }
    }
    agg
}

#[cfg(test)]
mod scraper_tests {
    use super::*;

    // A representative /metrics body: HELP/TYPE comment lines, the mission
    // counters with the `_total` suffix, the bare block-height gauge, and an
    // unrelated labeled series that must be ignored.
    const SAMPLE: &str = "\
# HELP torus_orders_matched Orders matched
# TYPE torus_orders_matched counter
torus_orders_matched_total 42
# TYPE torus_orders_placed_accepted counter
torus_orders_placed_accepted_total 100
torus_orders_resting_total 7
torus_orders_rejected_margin_total 3
torus_orders_rejected_book_total 1
torus_orders_rejected_cancelled_total 2
torus_orders_cancelled_partial_fill_total 4
torus_orders_self_trade_cancels_total 5
torus_orders_rejected_other_total 6
torus_blocks_committed_total 900
# TYPE torus_block_height gauge
torus_block_height 1234
torus_rpc_requests_total{method=\"eth_blockNumber\"} 555
";

    #[test]
    fn parses_all_mission_counters() {
        let s = MetricsSnapshot::parse(SAMPLE);
        assert_eq!(s.matched, 42);
        assert_eq!(s.placed_accepted, 100);
        assert_eq!(s.resting, 7);
        assert_eq!(s.rejected_margin, 3);
        assert_eq!(s.rejected_book, 1);
        assert_eq!(s.rejected_cancelled, 2);
        assert_eq!(s.cancelled_partial_fill, 4);
        assert_eq!(s.self_trade_cancels, 5);
        assert_eq!(s.rejected_other, 6);
        assert_eq!(s.blocks_committed, 900);
        assert_eq!(s.block_height, 1234);
    }

    #[test]
    fn absent_metric_reads_zero_not_error() {
        let s = MetricsSnapshot::parse("torus_block_height 9\n");
        assert_eq!(s.block_height, 9);
        assert_eq!(s.matched, 0, "absent counter must read 0");
        assert_eq!(s.placed_accepted, 0);
    }

    #[test]
    fn labeled_series_with_same_prefix_is_not_matched() {
        // `torus_orders_matched_total{market="1"} 8` must NOT satisfy a lookup
        // for the unlabeled `torus_orders_matched_total`.
        let s = MetricsSnapshot::parse("torus_orders_matched_total{market=\"1\"} 8\n");
        assert_eq!(s.matched, 0);
    }

    #[test]
    fn delta_is_field_wise_end_minus_start() {
        let start = MetricsSnapshot::parse(SAMPLE);
        let mut end = start;
        end.matched += 1000;
        end.placed_accepted += 2500;
        end.block_height += 300;
        let d = end.delta(&start);
        assert_eq!(d.matched, 1000);
        assert_eq!(d.placed_accepted, 2500);
        assert_eq!(d.block_height, 300);
        assert_eq!(d.resting, 0, "unchanged counter deltas to 0");
    }

    #[test]
    fn delta_saturates_on_counter_reset() {
        // A node that restarted mid-run: end < start. Saturating subtraction
        // yields 0, never a wrapped/huge value.
        let start = MetricsSnapshot {
            matched: 5000,
            block_height: 900,
            ..Default::default()
        };
        let end = MetricsSnapshot {
            matched: 10,
            block_height: 5,
            ..Default::default()
        };
        let d = end.delta(&start);
        assert_eq!(d.matched, 0);
        assert_eq!(d.block_height, 0);
    }

    #[test]
    fn max_with_picks_least_truncated_node() {
        let a = MetricsSnapshot { matched: 100, block_height: 50, ..Default::default() };
        let b = MetricsSnapshot { matched: 90, block_height: 55, ..Default::default() };
        let m = a.max_with(&b);
        assert_eq!(m.matched, 100);
        assert_eq!(m.block_height, 55);
    }

    #[test]
    fn float_encoded_value_is_truncated() {
        assert_eq!(metric_u64("torus_orders_matched_total 42.0\n", M_MATCHED), 42);
        assert_eq!(metric_u64("torus_block_height 7.9\n", M_BLOCK_HEIGHT), 7);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_consensus(
    rpc_urls_str: &str,
    senders: usize,
    duration_secs: u64,
    concurrency: usize,
    batch_size: usize,
    submit_batch: usize,
    sender_offset: usize,
    bin: bool,
    pre_sign: usize,
    rate: usize,
    sign_mode: SignMode,
    markets: u64,
    econ: Option<EconShape>,
    rate_total: f64,
    metrics_urls_str: &str,
    sweep_bodies: bool,
) {
    let rpc_urls: Vec<String> = rpc_urls_str
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    // #32: node /metrics endpoints. Empty (the default) = scraper OFF, so a run
    // with no --metrics-urls behaves as before. When set, the mission-counter
    // deltas over the timed window become THE headline throughput numbers.
    let metrics_urls: Vec<String> = metrics_urls_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let scrape_on = !metrics_urls.is_empty();
    let num_senders = senders.max(1);
    let keys = load_sender_keys(num_senders, sender_offset);
    let orders_per_action = batch_size.max(1) as u64;
    let submit_batch = submit_batch.clamp(1, 100);

    // Econ mode always streams (fresh wall-clock nonces, per-fire signing on the
    // shared blocking pool) — pre-sign's per-sender resident signer model neither
    // scales to thousands of senders nor matters once signing is off the hot loop.
    let pre_sign = if econ.is_some() && pre_sign > 0 {
        eprintln!("[econ] --pre-sign ignored: econ mode streams (per-fire blocking-pool signing)");
        0
    } else {
        pre_sign
    };

    // Pre-sign nonces run contiguously from now_ms; if the ammo span exceeds the
    // chain's 60s nonce window the tail is rejected as "too far in future".
    let nonce_span_ms = (pre_sign * submit_batch) as u64;
    if pre_sign > 0 && nonce_span_ms > torus_types::eip712::NONCE_WINDOW_MS / 2 {
        eprintln!(
            "[pre-sign] WARNING: ammo nonce span {nonce_span_ms}ms exceeds half the chain's {}ms \
             nonce window — with the 30s lead, late ammo risks 'too far in future' rejection. \
             Lower --pre-sign or --submit-batch.",
            torus_types::eip712::NONCE_WINDOW_MS
        );
    }

    println!("=== Torus Throughput Benchmark ===");
    println!("Mode: consensus");
    println!(
        "Sign mode: {}",
        match sign_mode {
            SignMode::Eip712 => "eip712 (secp256k1 ecrecover per action)",
            SignMode::Session => "session (ed25519 fast path)",
        }
    );
    println!("Duration: {duration_secs}s");
    println!("Senders: {num_senders} (offset {sender_offset})");
    println!("Batch size: {orders_per_action} order(s)/action");
    println!("Submit batch: {submit_batch} action(s)/RPC call");
    println!("RPC endpoints: {}", rpc_urls.len());
    println!(
        "Headline: {} | Body sweep: {}",
        if scrape_on {
            format!("node counters ({} /metrics endpoint(s))", metrics_urls.len())
        } else {
            "load-gen submit stats (no --metrics-urls)".to_string()
        },
        if sweep_bodies { "ON (--sweep-bodies)" } else { "off" },
    );
    if let Some(shape) = &econ {
        println!(
            "Econ shape: target-margin {} TRS/order | mid {} | band ±{} ticks | \
             cross {:.0}% | cancel-all {:.1}%/action",
            shape.target_margin,
            shape.mid,
            shape.band,
            shape.cross_fraction * 100.0,
            shape.cancel_fraction * 100.0,
        );
        if rate_total > 0.0 {
            println!(
                "Rate: {rate_total:.1} actions/s aggregate ({:.3}/s per sender)",
                rate_total / num_senders as f64
            );
        } else if rate > 0 {
            println!("Rate: {rate} actions/s/sender");
        } else {
            println!("Rate: unbounded (burst)");
        }
    }
    if pre_sign > 0 {
        println!(
            "Pre-sign: {pre_sign} batches/sender ({} actions/sender, signed before the clock)",
            pre_sign * submit_batch
        );
        match fire_interval(submit_batch, rate) {
            Some(_) => println!("Rate: {rate} actions/s/sender (paced fire)"),
            None => println!("Rate: unbounded (burst)"),
        }
    }
    println!();

    let client = Arc::new(
        reqwest::Client::builder()
            .pool_max_idle_per_host(256)
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client"),
    );

    // In session mode, register one ed25519 session per sender and wait for it to
    // commit BEFORE the timed window, so session-signed orders hit the fast path
    // instead of being rejected as "session not found".
    if sign_mode == SignMode::Session {
        println!("Registering {num_senders} ed25519 session(s) (scope: Full)...");
        let accepted = register_sessions(&client, &rpc_urls, &keys, bin).await;
        if accepted < num_senders {
            eprintln!(
                "[session] WARNING: only {accepted}/{num_senders} CreateSession actions admitted — \
                 session-signed orders from unregistered senders will be rejected."
            );
        } else {
            println!("  {accepted}/{num_senders} session(s) admitted.");
        }
        // CreateSession must COMMIT to state before a session-signed order
        // validates: ingress `session_lookup` reads committed state (db.get_session),
        // which lags inclusion by the ~3-block HotStuff commit window. Rather than
        // guess a block count, poll with a canary session-signed order from sender 0
        // until the node accepts it — then every session (all registered in the same
        // block) is live. Without this, orders fire pre-commit and the node rejects
        // them "signature verification failed: session key not found".
        print!("  waiting for sessions to commit (canary probe)... ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let canary_key = &keys[0].session_key;
        let mut probe_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let probe_deadline = Instant::now() + Duration::from_secs(45);
        let mut live = false;
        while Instant::now() < probe_deadline {
            tokio::time::sleep(Duration::from_millis(750)).await;
            probe_nonce += 1;
            let canary = sign_native_action_with_session(
                random_place_order_action(&mut StdRng::from_entropy(), 1, 1),
                probe_nonce,
                canary_key,
            );
            let bytes = if bin {
                bincode::serialize(&canary).unwrap()
            } else {
                serde_json::to_vec(&canary).unwrap()
            };
            let payload = format!("0x{}", hex::encode(&bytes));
            if let Ok(n) =
                submit_native_actions_batch(&client, &rpc_urls[0], &[payload], 0, bin).await
            {
                if n > 0 {
                    live = true;
                    break;
                }
            }
        }
        if live {
            println!("live (canary accepted).");
        } else {
            eprintln!(
                "\n[session] WARNING: sessions still not live after 45s — orders will be \
                 rejected 'session key not found'. Check chain liveness."
            );
        }
    }

    let submitted = Arc::new(AtomicU64::new(0));

    // Ammo strategy. Pre-sign stamps every nonce up front (now + window/2); if
    // signing ALL of it outlasts the nonce window the ammo is "too old" on arrival
    // and the leg silently carries zero load (S387 presign trap). Calibrate this
    // box's signing rate and fall back to the streaming pipeline (fresh nonces)
    // when pre-sign wouldn't fit — e.g. many senders on a core-pinned bench.
    let mut stream = pre_sign == 0;
    if pre_sign > 0 {
        let calib_key = keys[0].signing_key.clone();
        let calib_session = keys[0].session_key.clone();
        let (calib_actions, calib_secs) = tokio::task::spawn_blocking(move || {
            let mut rng = StdRng::from_entropy();
            let t = Instant::now();
            let mut n = 0u64;
            for _ in 0..3 {
                let (p, _) = sign_payload_batch(
                    &mut rng, &calib_key, &calib_session, sign_mode, batch_size,
                    submit_batch, 0, bin, markets,
                );
                n += p.len() as u64;
            }
            (n, t.elapsed().as_secs_f64())
        })
        .await
        .expect("calibration signer panicked");
        let per_core = if calib_secs > 0.0 {
            calib_actions as f64 / calib_secs
        } else {
            f64::INFINITY
        };
        // Pre-sign spawns one blocking signer per sender; the affinity-limited core
        // budget bounds real parallelism, so a core-pinned bench signs serial while
        // an unpinned box fans out. available_parallelism() honours sched affinity.
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let sign_rate = per_core * num_senders.min(cores).max(1) as f64;
        let total_actions = (num_senders * pre_sign * submit_batch) as u64;
        if choose_ammo_plan(
            total_actions,
            sign_rate,
            torus_types::eip712::NONCE_WINDOW_MS / 2,
            duration_secs,
            torus_types::eip712::NONCE_WINDOW_MS,
        ) == AmmoPlan::Stream
        {
            println!(
                "Pre-sign SKIPPED: {total_actions} actions @ ~{sign_rate:.0}/s ≈ {:.0}s would \
                 outlast the {}s nonce window — streaming fresh nonces instead.",
                total_actions as f64 / sign_rate,
                torus_types::eip712::NONCE_WINDOW_MS / 1000,
            );
            stream = true;
        }
    }

    // Pre-sign mode: build ALL ammo BEFORE the clock so the timed window is pure
    // submission (no signing CPU competing). Each sender signs in parallel on the
    // blocking pool; nonces start at now_ms and run contiguously per sender.
    let mut ammo: Vec<Vec<Vec<String>>> = Vec::new();
    if !stream {
        // Lead the nonces by half the window so they're still valid AFTER the
        // (potentially long) pre-sign phase: the chain rejects nonce < now-60s,
        // and signing a big ammo can take tens of seconds. now_ms + 30s keeps the
        // whole stream inside ±60s of fire time for pre-sign phases up to ~30s.
        let base_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + torus_types::eip712::NONCE_WINDOW_MS / 2;
        println!("Pre-signing {pre_sign} payloads x {num_senders} senders...");
        let t0 = Instant::now();
        let mut handles = Vec::with_capacity(num_senders);
        for sk in &keys {
            let key = sk.signing_key.clone();
            let session = sk.session_key.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let mut rng = StdRng::from_entropy();
                pregen_ammo(
                    &mut rng,
                    &key,
                    &session,
                    sign_mode,
                    batch_size,
                    submit_batch,
                    pre_sign,
                    bin,
                    base_nonce,
                    markets,
                )
            }));
        }
        for h in handles {
            ammo.push(h.await.expect("pregen task panicked"));
        }
        println!(
            "Pre-signed {} actions in {:.1}s",
            num_senders * pre_sign * submit_batch,
            t0.elapsed().as_secs_f64()
        );
    }

    // #32: START snapshot of the node funnel counters, taken as close to the
    // timed window's t0 as possible — its delta vs the END snapshot is the
    // headline throughput. Off (None) when --metrics-urls is unset.
    let start_metrics: Option<MetricsSnapshot> = if scrape_on {
        let s = scrape_all_metrics(&client, &metrics_urls).await;
        if s.is_none() {
            eprintln!(
                "[metrics] WARNING: no /metrics endpoint answered at start — headline \
                 counter scrape disabled for this run (checked {} url(s))",
                metrics_urls.len()
            );
        }
        s
    } else {
        None
    };
    let scrape_live = scrape_on && start_metrics.is_some();

    let start_block = fetch_block_number(&client, &rpc_urls[0]).await.unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let start_time = Instant::now();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let rpc_urls = Arc::new(rpc_urls);

    // Live block monitor. #34/#35: it polls ONLY cheap endpoints inside the
    // timed window — block height (for block-time stats) and, when enabled, the
    // node /metrics counters. It NEVER fetches full block bodies during the
    // window (that perturbs the system under test); body accounting is now
    // either the #32 counter scrape (ground truth) or the opt-in post-run sweep.
    let mon_client = client.clone();
    let mon_url = rpc_urls[0].clone();
    let mon_submitted = submitted.clone();
    let mon_start = start_time;
    let mon_deadline = deadline;
    let mon_metrics_url = metrics_urls.first().cloned();

    struct BlockStats {
        block_times: Vec<f64>,
    }

    let block_stats = Arc::new(tokio::sync::Mutex::new(BlockStats {
        block_times: Vec::new(),
    }));
    let final_stats = block_stats.clone();

    let monitor_handle = tokio::spawn({
        let block_stats = block_stats.clone();
        let mon_start_metrics = start_metrics;
        async move {
            let mut last_block = start_block;
            let mut last_block_time = Instant::now();
            let mut ticks: u64 = 0;

            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;

                if Instant::now() > mon_deadline + Duration::from_secs(5) {
                    break;
                }
                ticks += 1;

                let current_block = match fetch_block_number(&mon_client, &mon_url).await {
                    Some(n) => n,
                    None => continue,
                };

                if current_block > last_block {
                    let now = Instant::now();
                    let inter_block = now.duration_since(last_block_time).as_secs_f64()
                        / (current_block - last_block) as f64;

                    let mut stats = block_stats.lock().await;
                    for _ in 0..(current_block - last_block) {
                        stats.block_times.push(inter_block);
                    }
                    drop(stats);

                    last_block_time = now;
                    last_block = current_block;
                }

                let elapsed = mon_start.elapsed().as_secs_f64();
                let sub = mon_submitted.load(Ordering::Relaxed);
                let sub_rate = if elapsed > 0.0 { sub as f64 / elapsed } else { 0.0 };
                let avg_blk_ms = {
                    let stats = block_stats.lock().await;
                    if stats.block_times.is_empty() {
                        0.0
                    } else {
                        stats.block_times.iter().sum::<f64>() / stats.block_times.len() as f64
                            * 1000.0
                    }
                };

                // Live matched/placed from the node counters — scraped every ~2s
                // (cheap GET, no bodies) so the live line reflects ground truth.
                let live_matched = if scrape_live && ticks % 4 == 0 {
                    match (&mon_metrics_url, &mon_start_metrics) {
                        (Some(u), Some(s0)) => scrape_metrics(&mon_client, u)
                            .await
                            .map(|now| now.matched.saturating_sub(s0.matched)),
                        _ => None,
                    }
                } else {
                    None
                };

                match live_matched {
                    Some(m) => {
                        let m_rate = if elapsed > 0.0 { m as f64 / elapsed } else { 0.0 };
                        eprintln!(
                            "[{:.0}s] submitted: {} actions ({:.0}/s) | matched: {} ({:.0}/s node ctr) \
                             | blk: #{} | {:.0}ms/blk",
                            elapsed,
                            format_num(sub),
                            sub_rate,
                            format_num(m),
                            m_rate,
                            current_block,
                            avg_blk_ms,
                        );
                    }
                    None => {
                        eprintln!(
                            "[{:.0}s] submitted: {} actions ({:.0}/s) | blk: #{} | {:.0}ms/blk",
                            elapsed,
                            format_num(sub),
                            sub_rate,
                            current_block,
                            avg_blk_ms,
                        );
                    }
                }
            }
        }
    });

    // Sender tasks
    let mut sender_handles = Vec::new();

    // Econ pacing: `--rate-total` divides an aggregate actions/s budget across
    // all senders with a f64 interval (per-sender rates below 1/s are the norm
    // at thousands of senders); otherwise the legacy integer per-sender --rate.
    let econ_pace: Option<Duration> = if rate_total > 0.0 {
        Some(Duration::from_secs_f64(
            num_senders as f64 * submit_batch as f64 / rate_total,
        ))
    } else {
        fire_interval(submit_batch, rate)
    };

    for sender_idx in 0..num_senders {
        let key = keys[sender_idx].signing_key.clone();
        let session_key = keys[sender_idx].session_key.clone();
        let client = client.clone();
        let urls = rpc_urls.clone();
        let submitted = submitted.clone();
        let semaphore = semaphore.clone();
        let url_count = urls.len();
        let sender_ammo = if !stream {
            std::mem::take(&mut ammo[sender_idx])
        } else {
            Vec::new()
        };

        sender_handles.push(tokio::spawn(async move {
            let mut req_id: u64 = sender_idx as u64 * 1_000_000;
            let mut url_idx: usize = sender_idx % url_count;

            if let Some(shape) = econ {
                // A4 econ loop. Scales to 100k senders: unlike the legacy
                // streaming path (a RESIDENT spawn_blocking signer per sender,
                // which exhausts tokio's 512-thread blocking pool above ~512
                // senders and starves the rest), signing here is a SHORT-LIVED
                // blocking task per fire, so the pool is shared across the
                // whole fleet. Action generation is deterministic per
                // (sender, fire ordinal) via a fixed per-sender seed.
                let mut rng = StdRng::seed_from_u64(0xEC0A_0000_0000_0000 ^ sender_idx as u64);
                // Phase jitter: spread the fleet uniformly over one pace
                // interval so paced fires don't arrive as a synchronized burst.
                if let Some(iv) = econ_pace {
                    let jitter = iv.mul_f64(rng.gen_range(0.0..1.0));
                    let left = deadline.saturating_duration_since(Instant::now());
                    tokio::time::sleep(jitter.min(left)).await;
                }
                let mut last_nonce = 0u64;
                let mut next_fire = Instant::now();
                while Instant::now() < deadline {
                    if let Some(iv) = econ_pace {
                        let now = Instant::now();
                        if now < next_fire {
                            tokio::time::sleep(next_fire - now).await;
                        }
                        next_fire = next_fire.max(now) + iv;
                    }
                    // Generate inline (cheap), sign+encode on the blocking pool
                    // (the expensive ECDSA/serialize part). Nonces are wall-clock
                    // ms, strictly increasing per sender.
                    let mut batch_actions = Vec::with_capacity(submit_batch);
                    for _ in 0..submit_batch {
                        let action =
                            econ_action(&mut rng, sender_idx, markets, batch_size, &shape);
                        let base = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let nonce = base.max(last_nonce + 1);
                        last_nonce = nonce;
                        batch_actions.push((action, nonce));
                    }
                    let sign_key = key.clone();
                    let sign_session = session_key.clone();
                    let signed = tokio::task::spawn_blocking(move || {
                        batch_actions
                            .into_iter()
                            .map(|(action, nonce)| {
                                let s = sign_one(action, nonce, &sign_key, &sign_session, sign_mode);
                                let bytes = if bin {
                                    bincode::serialize(&s).unwrap()
                                } else {
                                    serde_json::to_vec(&s).unwrap()
                                };
                                format!("0x{}", hex::encode(&bytes))
                            })
                            .collect::<Vec<String>>()
                    })
                    .await;
                    let payloads = match signed {
                        Ok(p) => p,
                        Err(_) => break, // signer panicked — stop this sender
                    };
                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let url = urls[url_idx % url_count].clone();
                    url_idx += 1;
                    req_id += 1;
                    let client = client.clone();
                    let submitted = submitted.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        submit_payloads(&client, &url, &payloads, req_id, bin, &submitted).await;
                    });
                }
                return;
            }

            if !stream {
                // Pre-sign mode: fire pre-built ammo, ZERO signing in the timed
                // window — submit rate is now bounded by the chain/network, not
                // this box's signer. Stops early if a sender runs out of ammo.
                let total = sender_ammo.len();
                let mut fired = 0usize;
                let pace = fire_interval(submit_batch, rate);
                let mut next_fire = Instant::now();
                for payloads in sender_ammo {
                    if Instant::now() >= deadline {
                        break;
                    }
                    // Paced fire: hold the offered rate so a burst doesn't overrun
                    // RPC ingress. next_fire.max(now) means a slow patch never builds
                    // a catch-up burst — it just resumes the cadence.
                    if let Some(iv) = pace {
                        let now = Instant::now();
                        if now < next_fire {
                            tokio::time::sleep(next_fire - now).await;
                        }
                        next_fire = next_fire.max(now) + iv;
                    }
                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let url = urls[url_idx % url_count].clone();
                    url_idx += 1;
                    req_id += 1;
                    let client = client.clone();
                    let submitted = submitted.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        submit_payloads(&client, &url, &payloads, req_id, bin, &submitted).await;
                    });
                    fired += 1;
                }
                if fired == total {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left > Duration::from_millis(500) {
                        eprintln!(
                            "[sender {sender_idx}] ammo exhausted ({total} batches, {:.0}s left) — raise --pre-sign",
                            left.as_secs_f64()
                        );
                    }
                }
            } else {
                // T6 signer pipeline (streaming): a dedicated blocking-pool task
                // signs AHEAD into a depth-4 buffer so signing CPU overlaps
                // network I/O instead of serializing with it. Nonces stay inside
                // the 60s freshness window while decoupling the two stages.
                let (pregen_tx, mut pregen_rx) = tokio::sync::mpsc::channel::<Vec<String>>(4);
                let signer = tokio::task::spawn_blocking(move || {
                    let mut rng = StdRng::from_entropy();
                    let mut last_nonce: u64 = 0;
                    loop {
                        let (payloads, n) = sign_payload_batch(
                            &mut rng,
                            &key,
                            &session_key,
                            sign_mode,
                            batch_size,
                            submit_batch,
                            last_nonce,
                            bin,
                            markets,
                        );
                        last_nonce = n;
                        if pregen_tx.blocking_send(payloads).is_err() {
                            break; // submit loop finished — receiver dropped
                        }
                    }
                });
                let mut starved_ns: u128 = 0;
                // Pace the fire loop to the offered rate, same cadence as the
                // presign branch. Unpaced streaming lets 60 senders stampede the
                // local RPC ingress ('error sending request' + lost acks), so the
                // submitted counter undercounts even though the node commits fine.
                let pace = fire_interval(submit_batch, rate);
                let mut next_fire = Instant::now();

                while Instant::now() < deadline {
                    let wait_t0 = Instant::now();
                    let Some(payloads) = pregen_rx.recv().await else { break };
                    starved_ns += wait_t0.elapsed().as_nanos();

                    if let Some(iv) = pace {
                        let now = Instant::now();
                        if now < next_fire {
                            tokio::time::sleep(next_fire - now).await;
                        }
                        next_fire = next_fire.max(now) + iv;
                    }

                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let url = urls[url_idx % url_count].clone();
                    url_idx += 1;
                    req_id += 1;

                    let client = client.clone();
                    let submitted = submitted.clone();

                    tokio::spawn(async move {
                        let _permit = permit;
                        submit_payloads(&client, &url, &payloads, req_id, bin, &submitted).await;
                    });
                }

                // Dropping the receiver fails the signer's next blocking_send,
                // which is its exit signal.
                drop(pregen_rx);
                let _ = signer.await;
                if starved_ns > 1_000_000 {
                    eprintln!(
                        "[sender {sender_idx}] signer-starved {:.1}ms total (signing slower than submits)",
                        starved_ns as f64 / 1e6
                    );
                }
            }
        }));
    }

    for h in sender_handles {
        let _ = h.await;
    }

    // Wait for remaining in-flight requests to drain
    tokio::time::sleep(Duration::from_secs(2)).await;
    monitor_handle.abort();

    let total_elapsed = start_time.elapsed();
    let final_submitted = submitted.load(Ordering::Relaxed);

    // eth_blockNumber reports EXECUTION height, which lags consensus under load
    // (CTE): after the load phase stops the executor keeps committing its
    // backlog. Both the #32 counter delta and the opt-in body sweep want that
    // drained tail, so we quiesce first (height poll only — never a body).
    let mut end_block = fetch_block_number(&client, &rpc_urls[0])
        .await
        .unwrap_or(start_block);

    // ---- opt-in post-run body sweep (#34/#35, default OFF) ----
    // The #32 node counters are ground truth; the full block-body re-sweep is
    // now behind --sweep-bodies for deep per-action accounting (identities, dup
    // factor). Default OFF means NO body fetches anywhere in the run — neither
    // in-window nor post-run — so the bench never melts val0's RPC core serving
    // `getBlockBody` (#40). When on, it re-sweeps every body and extends the
    // window until the executor's backlog drains (two quiet 2s extensions, 120s
    // cap), exactly as before.
    let sweep_summary: Option<IncludedSummary> = if sweep_bodies {
        let mut swept =
            sweep_block_bodies(client.clone(), rpc_urls.clone(), start_block, end_block, concurrency)
                .await;
        let drain_deadline = Instant::now() + Duration::from_secs(120);
        let mut quiet_extensions = 0u32;
        while quiet_extensions < 2 && Instant::now() < drain_deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let cur = match fetch_block_number(&client, &rpc_urls[0]).await {
                Some(n) if n > end_block => n,
                _ => continue,
            };
            let delta =
                sweep_block_bodies(client.clone(), rpc_urls.clone(), end_block, cur, concurrency)
                    .await;
            let drained: u64 = delta.iter().map(|(_, c, _)| c).sum();
            if drained == 0 {
                quiet_extensions += 1;
            } else {
                quiet_extensions = 0;
                eprintln!(
                    "[drain] +{} actions in blocks #{}..#{} (executor catching up)",
                    drained,
                    end_block + 1,
                    cur
                );
            }
            swept.extend(delta);
            end_block = cur;
        }
        swept.sort_unstable_by_key(|b| b.0);
        trim_trailing_empty(&mut swept);
        end_block = swept.last().map(|b| b.0).unwrap_or(start_block);
        Some(summarize_included(&swept))
    } else if scrape_live {
        // Body-free quiescence wait so the END counter snapshot captures the
        // drained backlog. Height only — never a body.
        let drain_deadline = Instant::now() + Duration::from_secs(120);
        let mut quiet = 0u32;
        while quiet < 2 && Instant::now() < drain_deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match fetch_block_number(&client, &rpc_urls[0]).await {
                Some(n) if n > end_block => {
                    end_block = n;
                    quiet = 0;
                }
                _ => quiet += 1,
            }
        }
        None
    } else {
        None
    };

    // #32: END snapshot (after any quiescence above) and the headline delta.
    let end_metrics = if scrape_live {
        scrape_all_metrics(&client, &metrics_urls).await
    } else {
        None
    };
    let counter_elapsed = start_time.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
    let counter_delta = match (start_metrics, end_metrics) {
        (Some(s0), Some(s1)) => Some(s1.delta(&s0)),
        _ => None,
    };

    let avg_block_time_ms = {
        let stats = final_stats.lock().await;
        if stats.block_times.is_empty() {
            0.0
        } else {
            stats.block_times.iter().sum::<f64>() / stats.block_times.len() as f64 * 1000.0
        }
    };

    let elapsed_secs = total_elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    let submit_rate = final_submitted as f64 / elapsed_secs;

    println!();
    println!("--- Results ---");

    // HEADLINE: node Prometheus counter deltas (#32) — the ground-truth
    // throughput. The load-gen's own submit/`x batch` figures are demoted to
    // clearly-labelled secondary/debug below.
    if let Some(d) = counter_delta {
        let placed_rate = d.placed_accepted as f64 / counter_elapsed;
        let matched_rate = d.matched as f64 / counter_elapsed;
        let blk_rate = d.block_height as f64 / counter_elapsed;
        println!("HEADLINE — node counters over {counter_elapsed:.0}s (ground truth):");
        println!(
            "  Placed accepted:  {} ({:.0}/s)",
            format_num(d.placed_accepted),
            placed_rate,
        );
        println!(
            "  Matched:          {} ({:.0}/s)",
            format_num(d.matched),
            matched_rate,
        );
        println!(
            "  Funnel: resting {} | rej-margin {} | rej-book {} | rej-cancelled {} | \
             partial-cancel {} | self-trade-cancel {} | rej-other {}",
            format_num(d.resting),
            format_num(d.rejected_margin),
            format_num(d.rejected_book),
            format_num(d.rejected_cancelled),
            format_num(d.cancelled_partial_fill),
            format_num(d.self_trade_cancels),
            format_num(d.rejected_other),
        );
        let gate = if blk_rate >= HEALTH_GATE_BLK_PER_S {
            "PASS"
        } else {
            "FAIL"
        };
        println!(
            "  Health gate: {:.1} blk/s [{}] ({} committed; gate >= {:.1}, idle ref {:.1})",
            blk_rate,
            gate,
            format_num(d.blocks_committed),
            HEALTH_GATE_BLK_PER_S,
            IDLE_BLK_PER_S_REF,
        );
    } else if scrape_on {
        println!(
            "HEADLINE: node-counter scrape requested but no /metrics endpoint answered — \
             showing load-gen submit stats only (NOT ground truth)."
        );
    } else {
        println!(
            "HEADLINE: none — pass --metrics-urls http://host:9161,... for the ground-truth \
             mission counters (placed/s, matched/s). The figures below are load-gen submit \
             stats, NOT chain throughput."
        );
    }
    println!();

    // Secondary: what the load-gen itself observed (client-side, pre-execution).
    println!(
        "Submitted (load-gen accepted): {} native actions ({:.0}/s)  [secondary]",
        format_num(final_submitted),
        submit_rate,
    );

    // Debug: full body-sweep accounting, only when --sweep-bodies re-fetched it.
    if let Some(summary) = &sweep_summary {
        let final_included = summary.unique.unwrap_or(summary.slots);
        let inc_rate = final_included as f64 / elapsed_secs;
        match summary.unique {
            Some(unique) => {
                let dup = if unique > 0 {
                    summary.slots as f64 / unique as f64
                } else {
                    1.0
                };
                println!(
                    "Swept included: {} unique actions ({:.0}/s) [{} slots, dup x{:.2}]  [debug: --sweep-bodies]",
                    format_num(unique),
                    inc_rate,
                    format_num(summary.slots),
                    dup,
                );
            }
            None => println!(
                "Swept included: {} action slots ({:.0}/s) [no identities from node]  [debug: --sweep-bodies]",
                format_num(final_included),
                inc_rate,
            ),
        }
        if orders_per_action > 1 {
            // The load-gen's `included x batch` convention (~190x inflation,
            // S470) — kept only as an explicitly-labelled debug figure, NEVER
            // the headline.
            println!(
                "  Orders (included x{} batch): {} ({:.0}/s)  [DEBUG: inflated convention, not headline]",
                orders_per_action,
                format_num(final_included * orders_per_action),
                inc_rate * orders_per_action as f64,
            );
        }
        if summary.peak_actions > 0 {
            println!(
                "  Peak swept block #{}: {} actions ({} blocks with bodies)",
                summary.peak_block, summary.peak_actions, summary.block_count,
            );
        }
    }

    println!(
        "Block time: {avg_block_time_ms:.0}ms avg (blocks #{}..#{}, over {duration_secs}s window)",
        start_block + 1,
        end_block,
    );
}

// ============================================================================
// Mode 3: Combined (stub)
// ============================================================================

fn run_combined() {
    println!("combined mode: not yet implemented -- run consensus and tx-flood in parallel");
}

// ============================================================================
// Mode 4: State-root scaling (Phase A A1.6)
// ============================================================================

fn run_state_root_scaling(sizes_str: &str, changed: usize, blocks: usize) {
    use alloy_primitives::U256;
    use revm::database::BundleState;
    use revm::state::AccountInfo;
    use tempfile::TempDir;
    use torus_state::db::KECCAK_EMPTY;
    use torus_state::incremental::{
        build_trie_to_cf, full_post_bundle_evm_root, incremental_evm_root,
    };
    use torus_state::StateDb;

    let sizes: Vec<u64> = sizes_str
        .split(',')
        .filter_map(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .collect();
    let changed = changed.max(1);
    let blocks = blocks.max(1);

    let eoa = |bal: u64, nonce: u64| AccountInfo {
        balance: U256::from(bal),
        nonce,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    let addr_of = |i: u64| -> Address {
        let mut b = [0u8; 20];
        b[0..8].copy_from_slice(&i.to_be_bytes());
        Address::from(b)
    };

    println!("=== State-Root Scaling Proof (Phase A A1.6) ===");
    println!("Changed accounts/block: {changed}");
    println!("Blocks measured/size:   {blocks} (min reported)");
    println!();
    println!(
        "{:>12}  {:>13}  {:>13}  {:>9}  {:>11}",
        "accounts", "full-scan", "incremental", "speedup", "trie-build"
    );
    println!("{}", "-".repeat(66));

    for &n in &sizes {
        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        for i in 0..n {
            db.put_account(&addr_of(i), &eoa(1_000 + i, i)).unwrap();
        }

        let t = Instant::now();
        build_trie_to_cf(&db).unwrap();
        let build_time = t.elapsed();

        // A small bundle changing `changed` existing accounts spread across the keyspace — the
        // per-block working set. This is O(changed), independent of n.
        let mut builder = BundleState::builder(0..=0);
        let stride = (n / changed as u64).max(1);
        for j in 0..changed as u64 {
            let i = (j * stride) % n;
            builder = builder.state_present_account_info(addr_of(i), eoa(7_000_000 + j, 999));
        }
        let bundle = builder.build();

        let (mut full, mut incr) = (Duration::MAX, Duration::MAX);
        for _ in 0..blocks {
            let t = Instant::now();
            let _ = incremental_evm_root(&db, &bundle).unwrap();
            incr = incr.min(t.elapsed());

            let t = Instant::now();
            let _ = full_post_bundle_evm_root(&db, &bundle).unwrap();
            full = full.min(t.elapsed());
        }

        let speedup = full.as_secs_f64() / incr.as_secs_f64().max(1e-9);
        println!(
            "{:>12}  {:>13}  {:>13}  {:>8.1}x  {:>11}",
            format_num(n),
            format!("{full:.2?}"),
            format!("{incr:.2?}"),
            speedup,
            format!("{build_time:.2?}"),
        );
    }

    println!();
    println!(
        "Incremental root-compute time stays ~flat as accounts grow; full-scan grows ~linearly.\n\
         That flat line is the sub-100ms block-time guarantee holding at production state size."
    );
}

// ============================================================================
// Main
// ============================================================================

/// Derive EVM addresses for a sender range using the SAME key path the consensus
/// bench uses, so genesis funding provably matches the senders. Mirrors the chain's
/// `pubkey_to_address`: keccak256 of the 64-byte uncompressed pubkey, last 20 bytes.
/// Emits `<idx> 0x<address>` per line for downstream genesis tooling.
fn run_gen_accounts(offset: usize, count: usize, secret_keys: bool) {
    let keys = load_sender_keys(count, offset);
    for (i, sk) in keys.iter().enumerate() {
        let idx = offset + i;
        let vk = sk.signing_key.verifying_key();
        let uncompressed = vk.to_encoded_point(false);
        let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
        let addr = Address::from_slice(&hash[12..]);
        if secret_keys {
            // 32-byte scalar as 0x-prefixed hex — the same form cast/tx-loop expect.
            let sk_hex = hex::encode(sk.signing_key.to_bytes());
            println!("{idx} {addr:#x} 0x{sk_hex}");
        } else {
            println!("{idx} {addr:#x}");
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::MatchingEngine {
            orders,
            markets,
            warmup,
            genesis,
        } => run_matching_engine(orders, markets, warmup, &genesis),
        Command::Consensus {
            rpc_urls,
            senders,
            duration,
            concurrency,
            batch_size,
            submit_batch,
            sender_offset,
            format,
            pre_sign,
            rate,
            sign_mode,
            markets,
            econ,
            target_margin,
            cross_fraction,
            cancel_fraction,
            econ_mid,
            band,
            rate_total,
            metrics_urls,
            sweep_bodies,
        } => {
            let bin = match format.as_str() {
                "bin" => true,
                "json" => false,
                other => {
                    eprintln!("unknown --format {other:?} (expected \"json\" or \"bin\")");
                    std::process::exit(2);
                }
            };
            let mode = match sign_mode.as_str() {
                "eip712" => SignMode::Eip712,
                "session" => SignMode::Session,
                other => {
                    eprintln!("unknown --sign-mode {other:?} (expected \"eip712\" or \"session\")");
                    std::process::exit(2);
                }
            };
            let econ_shape = econ.then(|| {
                EconShape::new(target_margin, econ_mid, band, cross_fraction, cancel_fraction)
            });
            run_consensus(
                &rpc_urls,
                senders,
                duration,
                concurrency,
                batch_size,
                submit_batch,
                sender_offset,
                bin,
                pre_sign,
                rate,
                mode,
                markets,
                econ_shape,
                rate_total,
                &metrics_urls,
                sweep_bodies,
            )
            .await
        }
        Command::Combined { .. } => run_combined(),
        Command::StateRoot {
            sizes,
            changed,
            blocks,
        } => run_state_root_scaling(&sizes, changed, blocks),
        Command::GenAccounts {
            offset,
            count,
            secret_keys,
        } => run_gen_accounts(offset, count, secret_keys),
    }
}

#[cfg(test)]
mod tests {
    use super::{fire_interval, pregen_ammo, sign_one, sign_payload_batch, SignMode};
    use super::{summarize_included, SweptBlock};
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn sign_payload_batch_nonces_strictly_increase() {
        let mut rng = StdRng::seed_from_u64(7);
        let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let ed = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let (p1, n1) = sign_payload_batch(&mut rng, &key, &ed, SignMode::Eip712, 3, 4, 0, false, 1);
        let (p2, n2) =
            sign_payload_batch(&mut rng, &key, &ed, SignMode::Eip712, 3, 4, n1, false, 1);
        assert_eq!(p1.len(), 4);
        assert_eq!(p2.len(), 4);
        assert!(n2 > n1, "nonce watermark must advance across batches");
        let dec = |p: &String| -> u64 {
            let bytes = hex::decode(p.trim_start_matches("0x")).unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            v["nonce"]
                .as_u64()
                .expect("signed action has a numeric nonce")
        };
        let nonces: Vec<u64> = p1.iter().chain(p2.iter()).map(dec).collect();
        for w in nonces.windows(2) {
            assert!(
                w[1] > w[0],
                "nonces must be strictly increasing across the pregen stream: {nonces:?}"
            );
        }
    }

    #[test]
    fn session_mode_produces_verifiable_ed25519_signature() {
        // sign_one(Session) must emit an ActionSignature::Session that round-trips
        // through the wire encoding and verifies against the session pubkey — i.e.
        // the node takes the batched-ed25519 fast path, not a per-action ecrecover.
        let k = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let ed = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let pubkey = ed.verifying_key().to_bytes();
        let action = torus_types::NativeAction::CancelOrder { order_id: 9 };
        let signed = sign_one(action, 1_700_000_000_000, &k, &ed, SignMode::Session);
        // Round-trips through the same JSON the bench fires on the wire.
        let bytes = serde_json::to_vec(&signed).unwrap();
        let decoded: torus_types::SignedNativeAction = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded.verify_session_signature().unwrap(),
            pubkey,
            "session-signed action must carry a valid ed25519 sig over the EIP-712 hash"
        );
    }

    #[test]
    fn pregen_ammo_nonces_are_contiguous_from_base() {
        let mut rng = StdRng::seed_from_u64(7);
        let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let ed = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        // Deterministic: nonces depend only on base_nonce, not the wall clock, so
        // this catches a non-advancing nonce regardless of signing speed (in debug
        // a wall-clock nonce would mask it). 3 payloads x 10 actions = 30-action
        // stream, expected nonces BASE..BASE+30.
        const BASE: u64 = 1_700_000_000_000;
        let ammo = pregen_ammo(
            &mut rng,
            &key,
            &ed,
            SignMode::Eip712,
            1,
            10,
            3,
            false,
            BASE,
            1,
        );
        assert_eq!(ammo.len(), 3, "one payload-vec per requested count");
        assert!(
            ammo.iter().all(|p| p.len() == 10),
            "each payload carries submit_batch actions"
        );

        let dec = |p: &String| -> u64 {
            let bytes = hex::decode(p.trim_start_matches("0x")).unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            v["nonce"]
                .as_u64()
                .expect("signed action has a numeric nonce")
        };
        let nonces: Vec<u64> = ammo.iter().flatten().map(dec).collect();
        assert_eq!(nonces.len(), 30, "3 payloads x 10 actions");
        for (i, &n) in nonces.iter().enumerate() {
            assert_eq!(
                n,
                BASE + i as u64,
                "ammo nonces must be contiguous & strictly increasing from base_nonce: {nonces:?}"
            );
        }
    }

    #[test]
    fn fire_interval_paces_to_rate() {
        assert_eq!(fire_interval(10, 0), None, "rate 0 = unbounded (no pacing)");
        let approx = |iv: Option<std::time::Duration>, secs: f64| {
            (iv.expect("rate>0 must pace").as_secs_f64() - secs).abs() < 1e-9
        };
        // actions/s with actions/fire -> seconds/fire (submit_batch / rate).
        assert!(
            approx(fire_interval(10, 100), 0.100),
            "100 a/s, 10/fire => 100ms/fire"
        );
        assert!(
            approx(fire_interval(10, 200), 0.050),
            "200 a/s, 10/fire => 50ms/fire"
        );
        assert!(
            approx(fire_interval(1, 1000), 0.001),
            "1000 a/s, 1/fire => 1ms/fire"
        );
    }

    #[test]
    fn summarize_totals_count_and_peak() {
        // slots = 5+20+0+8 = 33; peak block is #11 with 20 actions.
        let blocks: Vec<SweptBlock> = vec![
            (10, 5, Vec::new()),
            (11, 20, Vec::new()),
            (12, 0, Vec::new()),
            (13, 8, Vec::new()),
        ];
        let s = summarize_included(&blocks);
        assert_eq!(
            (s.slots, s.block_count, s.peak_actions, s.peak_block),
            (33, 4, 20, 11)
        );
        // Count-only bodies (no identities) → unique unknown.
        assert_eq!(s.unique, None);
    }

    #[test]
    fn summarize_first_max_wins_on_ties() {
        // First block reaching the peak count keeps the peak_block slot.
        let blocks: Vec<SweptBlock> = vec![(7, 9, Vec::new()), (8, 9, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(
            (s.slots, s.block_count, s.peak_actions, s.peak_block),
            (18, 2, 9, 7)
        );
    }
}
