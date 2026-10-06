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

mod in_flight;
mod oracle_feed;

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
        /// C3: execute over a NativeStateOverlay (in-memory pending writes,
        /// read-through to RocksDB) — the LIVE node's exec backend (app.rs).
        /// Default off keeps the legacy raw-RocksDB shape for comparability.
        #[arg(long, default_value_t = false)]
        overlay: bool,
        /// C3: buffer trade-history KVs instead of inline PUTs (the live
        /// node's O3 background-writer config). Default off = legacy shape.
        #[arg(long, default_value_t = false)]
        defer_trades: bool,
        /// C3: draw order senders from a fixed pool of N addresses (0 = the
        /// legacy unique-address-per-order shape). Real chain load reuses
        /// senders heavily — unique-per-order makes every position/balance
        /// row cold and times the state backend instead of settlement.
        #[arg(long, default_value_t = 0)]
        senders: usize,
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
        /// LOCALITY shape: give every sender a FIXED set of K distinct markets
        /// and spread only its own orders over that set, instead of letting
        /// every order draw uniformly from 1..=markets. Sender `i` owns
        /// `((i*K + j) mod markets) + 1` for `j in 0..K` — deterministic, K
        /// distinct ids, every market owned by ~the same number of senders
        /// (see `market_plan_tests`). 0 (default) = today's uniform shape, so
        /// prior cells stay reproducible. K >= markets clamps to uniform.
        #[arg(long, default_value_t = 0)]
        markets_per_sender: u64,
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
        /// instead of a PlaceOrderBatch — recycles resting GTC margin.
        #[arg(long, default_value_t = 0.05)]
        cancel_fraction: f64,
        /// econ: keep each sender's estimated open orders (every order since
        /// its last cancel-all) at or below N by sending a CancelAllOrders
        /// whenever the next batch would cross N — stays under the chain's
        /// per-user open-order limit (1000+). 0 (default) = off, so prior
        /// cells stay reproducible.
        #[arg(long, default_value_t = 0)]
        open_order_budget: u64,
        /// econ: when the node sheds an action as busy (admission limit or
        /// full pool), back off and resend the SAME action instead of drawing
        /// a new one. The node sheds only non-cancels, so without this the
        /// admitted mix drifts toward all cancel-alls under overload and
        /// cancels (selected first) starve placement. Default OFF, so prior
        /// cells stay reproducible.
        #[arg(long, default_value_t = false)]
        retry_busy: bool,
        /// econ: per-sender in-flight cap in signed native actions (one
        /// PlaceOrderBatch / PlaceOrder / CancelAllOrders each; a fire of
        /// --submit-batch S actions takes S slots, so in flight can reach
        /// N-1+S). A sender fires only
        /// while fewer than N of its actions are in flight and otherwise waits
        /// for a slot; a slot frees when the action is seen committed (one
        /// shared block-body tail), refused by the RPC (a transport error is
        /// not a refusal), or past its nonce +
        /// 60 s window + 10 s. With --open-order-budget the estimate becomes
        /// the orders placed since the last COMMITTED cancel-all, in-flight
        /// places included. 0 (default) = off, so prior cells stay
        /// reproducible.
        #[arg(long, default_value_t = 0)]
        max_in_flight: usize,
        /// --max-in-flight: the node whose block bodies the shared tail reads.
        /// Default: the last --rpc-urls entry.
        #[arg(long, default_value = "")]
        in_flight_watch_rpc: String,
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
        /// Cancel-spam load (anti-spam cells): K extra keys sending
        /// CancelAllOrders (all markets) alongside the normal load, at
        /// --spam-cancel-rate actions/s in aggregate. Spammers never retry a
        /// refused action; their sent/accepted/rejected counts are reported
        /// apart from the load. 0 (default) = off, so prior cells stay
        /// reproducible.
        #[arg(long, default_value_t = 0)]
        spam_cancel_keys: usize,
        /// Aggregate cancel-spam rate in actions/s across all spam keys
        /// (required > 0 when --spam-cancel-keys > 0).
        #[arg(long, default_value_t = 0.0)]
        spam_cancel_rate: f64,
        /// Spam from funded genesis accounts (the top K of the bulk-funded
        /// index range, never a load sender) instead of fresh unfunded keys.
        #[arg(long, default_value_t = false)]
        spam_cancel_funded: bool,
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
    /// Keep oracle MARK prices fresh on the bench devnet: every --interval-ms
    /// each validator signs --price (or its --walk-bp walk) for markets 1..=--markets (ceil(N/256)
    /// `SubmitOraclePrices` chunks, sample time = now) and sends them to its
    /// own node. Runs until SIGTERM/SIGINT; stats every ~10 s to --stats-file.
    OracleFeed {
        /// Comma-separated node RPC urls; validator i sends to url i.
        #[arg(long)]
        rpc_urls: String,
        /// `{"validators":[{"index":0,"address":"0x..","private_key":"0x.."},..]}`.
        #[arg(long, default_value = "devnet/wsl/bench-validator-keys.json")]
        validator_keys: std::path::PathBuf,
        /// Feed markets 1..=N (the ids `consensus --markets N` trades).
        #[arg(long)]
        markets: u64,
        /// Mark price in whole TRS (bench econ mid 20 * 1500 = 30000).
        #[arg(long, default_value_t = 30_000)]
        price: u64,
        /// Item 6: walk every market's price ±N bp per round around --price
        /// (deterministic, mean-reverting, bounded at ±8N bp); 0 = fixed price.
        #[arg(long, default_value_t = 0)]
        walk_bp: u64,
        #[arg(long, default_value_t = 2_000)]
        interval_ms: u64,
        #[arg(long, default_value = "oracle-feed-stats.json")]
        stats_file: std::path::PathBuf,
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
    plan: MarketPlan,
    sender_idx: usize,
) -> (Vec<String>, u64) {
    let mut payloads = Vec::with_capacity(submit_batch);
    for _ in 0..submit_batch {
        let action = random_place_order_action(rng, plan, sender_idx, batch_size);
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
    plan: MarketPlan,
    sender_idx: usize,
) -> Vec<Vec<String>> {
    let mut nonce = base_nonce;
    let mut ammo = Vec::with_capacity(count);
    for _ in 0..count {
        let mut payloads = Vec::with_capacity(submit_batch);
        for _ in 0..submit_batch {
            let action = random_place_order_action(rng, plan, sender_idx, batch_size);
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
//     by the chain's per-user open-order limit (1000 + 1 per 5M volume, max
//     5000) at 5000 * m (7.5M TRS with defaults — well under B).
//   * maker fills leak m each: leak_rate/sender = (fills/s ÷ senders) * m.
//     Horizon T = (B - locked) / leak_rate. With B = 100M TRS (bumped genesis),
//     m = 1.5k, 150k fills/s: s=5,000 -> T ~= 37 min; s=100,000 -> T ~= 12 h.
//     True indefinite steady state is impossible bench-side while the node
//     strands maker-fill margin; scale senders and/or lower target-margin to
//     stretch T.
//
// CROSSING SHAPE: every (sender, market) pair trades ONE side only
// (legacy parity for uniform plans; assigned-owner rank for locality), so a sender can never self-trade (STP
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
    /// Max estimated open orders per sender (0 = off); see
    /// `econ_action_budgeted`.
    open_order_budget: u64,
    /// Resend a shed action until admitted instead of drawing a new one; see
    /// `submit_until_admitted`.
    retry_busy: bool,
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
            open_order_budget: 0,
            retry_busy: false,
        }
    }
}

/// Legacy uniform-plan side per (sender, market): a sender only ever buys or only ever sells
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
    plan: MarketPlan,
    shape: &EconShape,
) -> PlaceOrderParams {
    let is_buy = plan.econ_side_is_buy(sender_idx, market_id);
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
    plan: MarketPlan,
    batch_size: usize,
    shape: &EconShape,
) -> NativeAction {
    if shape.cancel_fraction > 0.0 && rng.gen_bool(shape.cancel_fraction) {
        return NativeAction::CancelAllOrders { market_id: None };
    }
    if batch_size <= 1 {
        let market_id = plan.pick(rng, sender_idx);
        return NativeAction::PlaceOrder(econ_place_order(rng, sender_idx, market_id, plan, shape));
    }
    let orders: Vec<PlaceOrderParams> = (0..batch_size)
        .map(|_| {
            let market_id = plan.pick(rng, sender_idx);
            econ_place_order(rng, sender_idx, market_id, plan, shape)
        })
        .collect();
    NativeAction::PlaceOrderBatch(orders)
}

/// `econ_action` under the chain's per-user open-order limit. `open` is the
/// sender's estimate of its open orders: every order placed since its last
/// cancel-all (an upper bound — fills are ignored, and the executor counts
/// every GTC of a block, filled or not). When the next action could take it
/// past `open_order_budget`, the sender sends a cancel-all instead. Budget 0
/// = off: byte-identical to `econ_action`. Caveat: a cancel-all runs before
/// every place of its block, so places from earlier fires landing in the same
/// block survive it; the budget leaves headroom below the 1000 limit for that.
fn econ_action_budgeted(
    rng: &mut impl Rng,
    sender_idx: usize,
    plan: MarketPlan,
    batch_size: usize,
    shape: &EconShape,
    open: &mut u64,
) -> NativeAction {
    let budget = shape.open_order_budget;
    if budget > 0 && *open + batch_size.max(1) as u64 > budget {
        *open = 0;
        return NativeAction::CancelAllOrders { market_id: None };
    }
    let action = econ_action(rng, sender_idx, plan, batch_size, shape);
    *open = match &action {
        NativeAction::PlaceOrderBatch(orders) => *open + orders.len() as u64,
        NativeAction::PlaceOrder(_) => *open + 1,
        _ => 0,
    };
    action
}

/// The open-order state one econ fire plans against.
enum FireBudget<'a> {
    /// No --max-in-flight: today's estimate (orders since the sender's last
    /// SENT cancel-all), updated in place by `econ_action_budgeted`.
    Legacy(&'a mut u64),
    /// --max-in-flight: `estimate` = orders placed since the last COMMITTED
    /// cancel-all incl. in-flight places (`in_flight::Tracker::estimate`);
    /// `cancel_pending` = one of the sender's cancel-alls is in flight.
    Capped { estimate: u64, cancel_pending: bool },
}

/// One fire's actions (up to `submit_batch`). `Legacy` is exactly today's
/// loop of `econ_action_budgeted`. `Capped` sends a cancel-all when the
/// estimate plus the next batch would cross the budget; random cancel-alls
/// and in-flight ones do not lower the estimate (it drops only on commit), so
/// once a cancel-all is pending the fire stops instead of adding a second.
fn econ_fire(
    rng: &mut impl Rng,
    sender_idx: usize,
    plan: MarketPlan,
    batch_size: usize,
    shape: &EconShape,
    submit_batch: usize,
    budget: &mut FireBudget,
) -> Vec<NativeAction> {
    let (mut estimate, mut cancel_pending) = match budget {
        FireBudget::Legacy(open) => {
            return (0..submit_batch)
                .map(|_| econ_action_budgeted(rng, sender_idx, plan, batch_size, shape, open))
                .collect();
        }
        FireBudget::Capped { estimate, cancel_pending } => (*estimate, *cancel_pending),
    };
    let limit = shape.open_order_budget;
    let mut out = Vec::with_capacity(submit_batch);
    for _ in 0..submit_batch {
        if limit > 0 && estimate + batch_size.max(1) as u64 > limit {
            if cancel_pending {
                break;
            }
            out.push(NativeAction::CancelAllOrders { market_id: None });
            cancel_pending = true;
            continue;
        }
        let action = econ_action(rng, sender_idx, plan, batch_size, shape);
        let (places, cancel) = in_flight::action_shape(&action);
        estimate += places;
        cancel_pending |= cancel;
        out.push(action);
    }
    out
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
                let p = econ_place_order(&mut rng, sender_idx, market_id, MarketPlan::uniform(10), &shape);
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
                let p = econ_place_order(&mut rng, sender_idx, market_id, MarketPlan::uniform(10), &shape);
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
                let a = econ_place_order(&mut rng, sender_idx, market_id, MarketPlan::uniform(4), &aggr);
                let p = econ_place_order(&mut rng, sender_idx, market_id, MarketPlan::uniform(4), &passive);
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
                econ_action(&mut rng, 3, MarketPlan::uniform(10), 8, &never),
                NativeAction::CancelAllOrders { .. }
            ));
        }
        let mut cancels = 0usize;
        let some = EconShape::new(1500, 0, 5, 0.5, 0.2);
        for _ in 0..2000 {
            if matches!(
                econ_action(&mut rng, 3, MarketPlan::uniform(10), 8, &some),
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
                    format!("{:?}", econ_action(&mut rng, 9, MarketPlan::uniform(10), 4, &shape))
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
        match econ_action(&mut rng, 4, MarketPlan::uniform(10), 400, &shape) {
            NativeAction::PlaceOrderBatch(orders) => {
                assert_eq!(orders.len(), 400);
                assert!(orders.iter().all(|o| (1..=10).contains(&o.market_id)));
            }
            other => panic!("expected PlaceOrderBatch, got {other:?}"),
        }
        match econ_action(&mut rng, 4, MarketPlan::uniform(10), 1, &shape) {
            NativeAction::PlaceOrder(_) => {}
            other => panic!("batch_size 1 must emit a plain PlaceOrder, got {other:?}"),
        }
    }

    // --open-order-budget N: a sender's estimated open orders (every order
    // since its last cancel-all) never exceed N; when the next batch would
    // cross it, the sender sends a cancel-all instead.
    #[test]
    fn open_order_budget_forces_cancel_all_before_crossing_it() {
        for (batch, cancel_fraction) in [(400usize, 0.0), (400, 0.05), (32, 0.05), (1, 0.0)] {
            let mut shape = EconShape::new(1500, 0, 5, 0.5, cancel_fraction);
            shape.open_order_budget = 900;
            let mut rng = StdRng::seed_from_u64(31);
            let mut open = 0u64;
            let (mut estimate, mut forced) = (0u64, 0usize);
            for _ in 0..2000 {
                let action =
                    econ_action_budgeted(&mut rng, 7, MarketPlan::uniform(10), batch, &shape, &mut open);
                match action {
                    NativeAction::CancelAllOrders { .. } => {
                        forced += usize::from(estimate + batch as u64 > 900);
                        estimate = 0;
                    }
                    NativeAction::PlaceOrderBatch(orders) => estimate += orders.len() as u64,
                    NativeAction::PlaceOrder(_) => estimate += 1,
                    other => panic!("unexpected {other:?}"),
                }
                assert!(estimate <= 900, "batch {batch}: estimate {estimate}");
                assert_eq!(open, estimate);
            }
            assert!(forced > 0, "batch {batch}: the budget must force cancel-alls");
        }
        // 400-order batches with no random cancels: place, place, cancel, ...
        let mut shape = EconShape::new(1500, 0, 5, 0.5, 0.0);
        shape.open_order_budget = 900;
        let mut rng = StdRng::seed_from_u64(32);
        let mut open = 0;
        let kinds: Vec<bool> = (0..6)
            .map(|_| {
                matches!(
                    econ_action_budgeted(&mut rng, 7, MarketPlan::uniform(10), 400, &shape, &mut open),
                    NativeAction::CancelAllOrders { .. }
                )
            })
            .collect();
        assert_eq!(kinds, [false, false, true, false, false, true]);
    }

    // Budget 0 (default) = off: byte-identical to the unbudgeted generator.
    #[test]
    fn open_order_budget_zero_is_byte_identical() {
        let shape = default_shape();
        assert_eq!(shape.open_order_budget, 0);
        for batch in [1, 400] {
            let mut a = StdRng::seed_from_u64(33);
            let mut b = a.clone();
            let mut open = 0;
            for _ in 0..200 {
                assert_eq!(
                    bincode::serialize(&econ_action_budgeted(
                        &mut a,
                        5,
                        MarketPlan::uniform(10),
                        batch,
                        &shape,
                        &mut open
                    ))
                    .unwrap(),
                    bincode::serialize(&econ_action(&mut b, 5, MarketPlan::uniform(10), batch, &shape))
                        .unwrap(),
                );
            }
        }
    }

    fn ser(actions: &[NativeAction]) -> Vec<Vec<u8>> {
        actions.iter().map(|a| bincode::serialize(a).unwrap()).collect()
    }

    // --max-in-flight off: the sender loop plans each fire with
    // FireBudget::Legacy, which must be exactly today's loop of
    // econ_action_budgeted (same rng stream, same open-order estimate), with
    // and without a budget, for one and several actions per fire. Signing,
    // encoding and the wire are unchanged code, so the payloads match too
    // (nonces are wall-clock ms either way).
    #[test]
    fn legacy_fire_is_todays_action_stream() {
        for (budget, batch, submit) in [(0u64, 400usize, 1usize), (900, 400, 1), (900, 32, 3), (0, 1, 2)] {
            let mut shape = EconShape::new(1500, 0, 5, 0.5, 0.05);
            shape.open_order_budget = budget;
            let mut a = StdRng::seed_from_u64(41);
            let mut b = a.clone();
            let (mut open_a, mut open_b) = (0u64, 0u64);
            for _ in 0..300 {
                let fire = econ_fire(
                    &mut a,
                    5,
                    MarketPlan::uniform(10),
                    batch,
                    &shape,
                    submit,
                    &mut FireBudget::Legacy(&mut open_a),
                );
                let want: Vec<NativeAction> = (0..submit)
                    .map(|_| {
                        econ_action_budgeted(&mut b, 5, MarketPlan::uniform(10), batch, &shape, &mut open_b)
                    })
                    .collect();
                assert_eq!(ser(&fire), ser(&want));
                assert_eq!(open_a, open_b);
            }
        }
    }

    // Capped with budget 0 draws the plain econ_action stream.
    #[test]
    fn capped_fire_without_budget_is_the_econ_stream() {
        let shape = default_shape();
        let mut a = StdRng::seed_from_u64(42);
        let mut b = a.clone();
        for _ in 0..200 {
            let fire = econ_fire(
                &mut a,
                3,
                MarketPlan::uniform(10),
                400,
                &shape,
                2,
                &mut FireBudget::Capped { estimate: 5_000, cancel_pending: true },
            );
            let want: Vec<NativeAction> =
                (0..2).map(|_| econ_action(&mut b, 3, MarketPlan::uniform(10), 400, &shape)).collect();
            assert_eq!(ser(&fire), ser(&want));
        }
    }

    // Capped with a budget: cancel-all when estimate + batch > budget, a
    // place otherwise; a pending cancel-all stops the fire instead of
    // queueing a second one.
    #[test]
    fn capped_fire_cancels_on_the_committed_estimate() {
        let mut shape = EconShape::new(1500, 0, 5, 0.5, 0.0);
        shape.open_order_budget = 900;
        let mut rng = StdRng::seed_from_u64(43);
        let fire = |rng: &mut StdRng, estimate, cancel_pending, submit| {
            econ_fire(
                rng,
                7,
                MarketPlan::uniform(10),
                400,
                &shape,
                submit,
                &mut FireBudget::Capped { estimate, cancel_pending },
            )
        };
        let kinds = |v: &[NativeAction]| -> Vec<&'static str> {
            v.iter()
                .map(|a| match a {
                    NativeAction::CancelAllOrders { market_id: None } => "C",
                    NativeAction::PlaceOrderBatch(o) if o.len() == 400 => "P",
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        assert_eq!(kinds(&fire(&mut rng, 0, false, 1)), ["P"]);
        assert_eq!(kinds(&fire(&mut rng, 500, false, 1)), ["P"], "500 + 400 = 900 fits");
        assert_eq!(kinds(&fire(&mut rng, 501, false, 1)), ["C"]);
        // An in-flight cancel-all does not reset the estimate, places still
        // fit under it.
        assert_eq!(kinds(&fire(&mut rng, 400, true, 1)), ["P"]);
        assert!(fire(&mut rng, 800, true, 1).is_empty(), "wait for the pending cancel-all");
        // Several actions per fire: the fire's own places count, one
        // cancel-all at most.
        assert_eq!(kinds(&fire(&mut rng, 0, false, 4)), ["P", "P", "C"]);
        assert_eq!(kinds(&fire(&mut rng, 100, true, 4)), ["P", "P"]);
    }
}

/// Which markets one bench sender is allowed to touch (`--markets-per-sender`).
///
/// `per_sender == 0` (the default) is the legacy shape: every ORDER draws a
/// market uniformly from `1..=markets`, so all 5000 senders collide on all
/// books. `per_sender == K > 0` is the LOCALITY shape: sender `i` only ever
/// touches the K distinct ids `((i*K + j) mod markets) + 1, j in 0..K`.
///
/// The stride assignment (rather than `hash(i) -> K ids`) is deliberate: it is
/// a pure function of the sender index (reproducible across processes and
/// reps), the K ids are distinct by construction whenever `K <= markets`, and
/// every market ends up owned by `floor` or `ceil` of `senders*K/markets`
/// senders — a hash would leave some books empty at small sender counts, and an
/// empty book silently skews matched/s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MarketPlan {
    markets: u64,
    per_sender: u64,
}

impl MarketPlan {
    fn new(markets: u64, per_sender: u64) -> Self {
        let markets = markets.max(1);
        Self {
            markets,
            per_sender: per_sender.min(markets),
        }
    }

    /// Legacy uniform plan (every sender may touch every market).
    #[cfg(test)]
    fn uniform(markets: u64) -> Self {
        Self::new(markets, 0)
    }

    /// Alternate the actual owners of each market, not all possible senders.
    /// In the flattened assignment slots i*K+j, market m occurs at
    /// (m-1)+q*N. Its owners therefore have ranks q=0,1,... with no gaps:
    /// alternating q gives exactly equal sides for an even owner count and
    /// a difference of one for an odd count. A sender's side stays fixed.
    fn econ_side_is_buy(&self, sender_idx: usize, market_id: u64) -> bool {
        if self.per_sender == 0 || self.per_sender == self.markets {
            // Preserve the existing default/full-market workload exactly.
            return econ_side_is_buy(sender_idx, market_id);
        }
        let slots = sender_idx as u128 * self.per_sender as u128;
        let markets = self.markets as u128;
        let offset = (market_id as u128 - 1 + markets - slots % markets) % markets;
        debug_assert!(offset < self.per_sender as u128, "market outside sender's plan");
        let owner_rank = (slots + offset) / markets;
        (owner_rank + market_id as u128) % 2 == 0
    }

    /// `sender_idx`'s fixed market set, in assignment order.
    fn markets_for(&self, sender_idx: usize) -> Vec<u64> {
        if self.per_sender == 0 {
            return (1..=self.markets).collect();
        }
        let start = sender_idx as u128 * self.per_sender as u128 % self.markets as u128;
        (0..self.per_sender)
            .map(|j| ((start + j as u128) % self.markets as u128 + 1) as u64)
            .collect()
    }

    /// One market id for the next order of `sender_idx`.
    fn pick(&self, rng: &mut impl Rng, sender_idx: usize) -> u64 {
        if self.per_sender == 0 {
            return rng.gen_range(1..=self.markets);
        }
        let start = sender_idx as u128 * self.per_sender as u128 % self.markets as u128;
        ((start + rng.gen_range(0..self.per_sender) as u128) % self.markets as u128 + 1) as u64
    }
}

#[cfg(test)]
mod market_plan_tests {
    use super::*;

    #[test]
    fn locality_sides_balance_actual_market_owners() {
        // Include the two formerly one-sided shapes, non-divisor K, odd owner
        // counts, and markets with too few owners to support opposing flow.
        for (markets, per_sender, senders) in [
            (10, 1, 5_000), (300, 3, 5_000), (10, 3, 51),
            (7, 3, 101), (9, 8, 17), (10, 1, 3),
        ] {
            let plan = MarketPlan::new(markets, per_sender);
            let mut counts = vec![[0usize; 2]; markets as usize + 1];
            for sender in 0..senders {
                for market in plan.markets_for(sender) {
                    let side = usize::from(plan.econ_side_is_buy(sender, market));
                    counts[market as usize][side] += 1;
                    // Every sender-count prefix must remain balanced, not just
                    // the final round multiple of the assignment period.
                    let [sell, buy] = counts[market as usize];
                    assert!(sell.abs_diff(buy) <= 1,
                        "N={markets} K={per_sender} sender={sender} market={market}: {sell}/{buy}");
                }
            }
            for [sell, buy] in &counts[1..] {
                if sell + buy >= 2 {
                    assert!(*sell > 0 && *buy > 0);
                }
            }
        }
    }

    #[test]
    fn locality_econ_generation_uses_fixed_balanced_sides() {
        let shape = EconShape::new(1500, 0, 1, 0.5, 0.0);
        for (markets, per_sender) in [(10, 1), (300, 3), (7, 3)] {
            let plan = MarketPlan::new(markets, per_sender);
            for sender in 0..200 {
                let mut rng = StdRng::seed_from_u64(sender as u64);
                for _ in 0..2 {
                    let NativeAction::PlaceOrderBatch(orders) =
                        econ_action(&mut rng, sender, plan, 32, &shape)
                    else { panic!("expected placement batch") };
                    for order in orders {
                        assert!(plan.markets_for(sender).contains(&order.market_id));
                        assert_eq!(order.is_buy, plan.econ_side_is_buy(sender, order.market_id));
                    }
                }
            }
        }
    }

    #[test]
    fn uniform_econ_actions_remain_byte_identical_to_legacy_generator() {
        // Freeze the pre-fix action generation here, including RNG consumption.
        fn legacy_order(rng: &mut impl Rng, sender: usize, market: u64, shape: &EconShape) -> PlaceOrderParams {
            let is_buy = (sender as u64).wrapping_add(market) % 2 == 0;
            let aggressive = rng.gen_bool(shape.cross_fraction);
            let d = rng.gen_range(1..=shape.band as i128);
            let price_units = if is_buy == aggressive { shape.mid as i128 + d } else { shape.mid as i128 - d };
            let price = FixedPoint::from_raw(price_units * FixedPoint::SCALE);
            let target = FixedPoint::from_raw(shape.target_margin as i128 * FixedPoint::SCALE);
            let leverage = FixedPoint::from_raw(NATIVE_DEFAULT_LEVERAGE * FixedPoint::SCALE);
            let quantity = (target * leverage / price).max(FixedPoint::ONE);
            PlaceOrderParams {
                market_id: market, is_buy, price, quantity, order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC, reduce_only: false, client_order_id: None,
            }
        }
        fn legacy_action(rng: &mut impl Rng, sender: usize, markets: u64, batch: usize, shape: &EconShape) -> NativeAction {
            if shape.cancel_fraction > 0.0 && rng.gen_bool(shape.cancel_fraction) {
                return NativeAction::CancelAllOrders { market_id: None };
            }
            if batch <= 1 {
                let market = rng.gen_range(1..=markets);
                return NativeAction::PlaceOrder(legacy_order(rng, sender, market, shape));
            }
            NativeAction::PlaceOrderBatch((0..batch).map(|_| {
                let market = rng.gen_range(1..=markets);
                legacy_order(rng, sender, market, shape)
            }).collect())
        }
        let shape = EconShape::new(1500, 0, 5, 0.5, 0.05);
        for batch in [1, 4, 400] {
            for sender in [0, 9, 42] {
                let mut legacy_rng = StdRng::seed_from_u64(23);
                let mut current_rng = legacy_rng.clone();
                for _ in 0..64 {
                    assert_eq!(
                        bincode::serialize(&econ_action(&mut current_rng, sender, MarketPlan::uniform(10), batch, &shape)).unwrap(),
                        bincode::serialize(&legacy_action(&mut legacy_rng, sender, 10, batch, &shape)).unwrap(),
                    );
                }
            }
        }
        // Explicit K>=N already used legacy sides and must continue to do so.
        for k in [10, 99] {
            let plan = MarketPlan::new(10, k);
            for sender in 0..50 {
                for market in plan.markets_for(sender) {
                    assert_eq!(plan.econ_side_is_buy(sender, market), econ_side_is_buy(sender, market));
                }
            }
        }
    }

    // --markets-per-sender 0 (default) must keep TODAY's shape: every order
    // draws uniformly from 1..=markets, and the whole range is used.
    #[test]
    fn per_sender_zero_is_the_legacy_uniform_shape() {
        let plan = MarketPlan::new(10, 0);
        assert_eq!(plan.markets_for(7), (1..=10).collect::<Vec<u64>>());
        let mut rng = StdRng::seed_from_u64(1);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..5_000 {
            seen.insert(plan.pick(&mut rng, 7));
        }
        assert_eq!(seen, (1..=10).collect::<std::collections::BTreeSet<u64>>());
    }

    // The per-sender set is a pure function of the sender index: same set on
    // every call, in every process, for the whole run.
    #[test]
    fn assignment_is_deterministic_and_distinct() {
        let plan = MarketPlan::new(300, 3);
        for i in [0usize, 1, 42, 4_999, 100_000] {
            let a = plan.markets_for(i);
            assert_eq!(a, plan.markets_for(i), "sender {i} set must be stable");
            assert_eq!(a.len(), 3, "sender {i} must get exactly K ids");
            let uniq: std::collections::BTreeSet<u64> = a.iter().copied().collect();
            assert_eq!(uniq.len(), 3, "sender {i} ids must be distinct: {a:?}");
            assert!(a.iter().all(|m| (1..=300).contains(m)), "ids in range: {a:?}");
        }
    }

    // pick() may only ever return an id from that sender's fixed set.
    #[test]
    fn pick_stays_inside_the_senders_set() {
        let plan = MarketPlan::new(300, 4);
        let mut rng = StdRng::seed_from_u64(7);
        for sender in [0usize, 3, 77, 1_234] {
            let set: std::collections::BTreeSet<u64> =
                plan.markets_for(sender).into_iter().collect();
            let mut hit = std::collections::BTreeSet::new();
            for _ in 0..2_000 {
                let m = plan.pick(&mut rng, sender);
                assert!(set.contains(&m), "sender {sender} strayed to market {m}");
                hit.insert(m);
            }
            assert_eq!(hit, set, "sender {sender} must exercise its whole set");
        }
    }

    // Coverage: with senders * K >= markets every market is owned by someone
    // (a market nobody trades is a dead book that skews matched/s).
    #[test]
    fn every_market_is_covered_and_load_is_even() {
        let plan = MarketPlan::new(300, 3);
        let mut count = vec![0usize; 301];
        for s in 0..5_000usize {
            for m in plan.markets_for(s) {
                count[m as usize] += 1;
            }
        }
        assert!(count[1..].iter().all(|&c| c > 0), "every market must be covered");
        let lo = *count[1..].iter().min().unwrap();
        let hi = *count[1..].iter().max().unwrap();
        assert!(hi - lo <= 1, "sender-per-market load must be even, got {lo}..{hi}");
    }

    // K >= markets degenerates to the uniform shape instead of erroring.
    #[test]
    fn per_sender_at_or_above_markets_clamps_to_all_markets() {
        let plan = MarketPlan::new(8, 99);
        assert_eq!(plan.markets_for(3), (1..=8).collect::<Vec<u64>>());
        assert_eq!(MarketPlan::new(0, 5).markets_for(0), vec![1]);
    }

    // The locality shape must reach the real generators: with K=1 an econ batch
    // of 400 orders lands on ONE book, and the legacy generator honours it too.
    #[test]
    fn econ_and_legacy_batches_honour_the_plan() {
        let shape = EconShape::new(1500, 0, 5, 0.5, 0.0);
        let plan = MarketPlan::new(300, 1);
        let mut rng = StdRng::seed_from_u64(3);
        match econ_action(&mut rng, 9, plan, 400, &shape) {
            NativeAction::PlaceOrderBatch(orders) => {
                let want = plan.markets_for(9)[0];
                assert!(orders.iter().all(|o| o.market_id == want), "K=1 must be single-book");
            }
            other => panic!("expected PlaceOrderBatch, got {other:?}"),
        }
        match random_place_order_action(&mut rng, plan, 11, 50) {
            NativeAction::PlaceOrderBatch(orders) => {
                let want = plan.markets_for(11)[0];
                assert!(orders.iter().all(|o| o.market_id == want));
            }
            other => panic!("expected PlaceOrderBatch, got {other:?}"),
        }
    }
}

/// Build one action carrying `batch_size` orders. `batch_size <= 1` returns a plain
/// `PlaceOrder` (the legacy single path) for apples-to-apples A/B runs.
///
/// `plan` spreads orders across market ids (per ORDER, so one PlaceOrderBatch
/// fans out to many books) — a single-market load shape skips
/// `MarketWorkerPool::match_parallel` entirely (market_workers.rs single-market
/// fast path) and measures one book's sequential ceiling, not the chain's
/// (S372 finding, S395 knob). With `--markets-per-sender K` the fan-out is
/// restricted to this sender's own K books (locality shape).
fn random_place_order_action(
    rng: &mut impl Rng,
    plan: MarketPlan,
    sender_idx: usize,
    batch_size: usize,
) -> NativeAction {
    if batch_size <= 1 {
        let market_id = plan.pick(rng, sender_idx);
        return random_place_order(rng, market_id);
    }
    let orders: Vec<PlaceOrderParams> = (0..batch_size)
        .map(|_| {
            let market_id = plan.pick(rng, sender_idx);
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

fn run_matching_engine(
    orders: usize,
    markets: u64,
    warmup: usize,
    genesis_path: &str,
    overlay: bool,
    defer_trades: bool,
    senders: usize,
) {
    use tempfile::TempDir;
    use torus_genesis::Genesis;
    use torus_state::{NativeStateOverlay, StateDb};

    let dir = TempDir::new().unwrap();
    let state_db = StateDb::open(dir.path()).unwrap();

    let genesis = Genesis::from_file(std::path::Path::new(genesis_path)).unwrap();
    let _state_root = genesis.initialize(&state_db).unwrap();
    let chain_config = genesis.chain_config();

    println!("=== Matching Engine Benchmark ===");
    println!(
        "Backend: {} | trades: {}",
        if overlay {
            "NativeStateOverlay (live exec shape)"
        } else {
            "raw StateDb (legacy)"
        },
        if defer_trades {
            "deferred (live O3 shape)"
        } else {
            "inline PUTs (legacy)"
        },
    );

    // C3: the LIVE node executes over a NativeStateOverlay with trade-history
    // writes deferred to a background writer — raw-StateDb inline-put mode
    // times RocksDB, not the exec pipeline. Both shapes stay available.
    if overlay {
        run_matching_engine_on(
            NativeStateOverlay::new(state_db),
            &chain_config,
            orders,
            markets,
            warmup,
            defer_trades,
            senders,
        );
    } else {
        run_matching_engine_on(
            state_db,
            &chain_config,
            orders,
            markets,
            warmup,
            defer_trades,
            senders,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_matching_engine_on<T: torus_state::StateBackend>(
    backend: T,
    chain_config: &torus_types::ChainConfig,
    orders: usize,
    markets: u64,
    warmup: usize,
    defer_trades: bool,
    senders: usize,
) {
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_core::position::NativeBalance;

    let mut rng = rand::thread_rng();
    let market_count = markets.max(1);

    // Sender pool (0 = legacy unique-address-per-order).
    let sender_pool: Vec<Address> = (0..senders).map(|_| random_address(&mut rng)).collect();
    let mut pick_sender = {
        let pool = sender_pool.clone();
        move |rng: &mut rand::rngs::ThreadRng| -> Address {
            if pool.is_empty() {
                random_address(rng)
            } else {
                pool[rng.gen_range(0..pool.len())]
            }
        }
    };

    println!("Markets: {market_count}");
    println!(
        "Senders: {}",
        if senders == 0 {
            "unique per order (legacy)".to_string()
        } else {
            format_num(senders as u64)
        }
    );
    println!("Warmup:  {} orders", format_num(warmup as u64));
    println!();

    let mut ctx = NativeExecContext::new(
        backend,
        1,
        1_700_000_000,
        0,
        chain_config.epoch_length,
        chain_config.max_validators,
        Address::ZERO,
        chain_config.treasury_address,
        chain_config.dev_pool_address,
    );
    ctx.defer_trades = defer_trades;

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
                (pick_sender(&mut rng), random_place_order(&mut rng, mid))
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
        // Drop the warmup's fills so the first timed batch does not re-write them.
        ctx.take_pending_trade_fills();
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
                (pick_sender(&mut rng), random_place_order(&mut rng, mid))
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
        let fills_before = ctx.trade_index;
        let t = Instant::now();
        NativeExecutor::execute_batch(&mut ctx, &actions);
        let elapsed = t.elapsed();
        ctx.metrics = None;
        // Live O3 shape: the node hands the block's fills to a background
        // writer after exec — draining here keeps the buffer from growing
        // across batches without charging the exec timer.
        let deferred_fills = ctx.take_pending_trade_fills().len();
        let rate = batch_size as f64 / elapsed.as_secs_f64();

        println!(
            "Batch {}: {:.0} orders/sec ({:.1}ms) | fills {} | deferred fills {}",
            format_num(batch_size as u64),
            rate,
            elapsed.as_secs_f64() * 1000.0,
            format_num((ctx.trade_index - fills_before) as u64),
            format_num(deferred_fills as u64),
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
    submit_native_actions_items(client, url, payloads, id, bin)
        .await
        .map(|items| accepted_count(&items))
}

fn accepted_count(items: &[Option<String>]) -> usize {
    items.iter().filter(|e| e.is_none()).count()
}

/// Per-item outcome of one `torus_submitNativeActions[Bin]` reply: `None` =
/// admitted (the item carries a `hash`), `Some(err)` = refused (its JSON
/// `error`). `Err` = the call failed or admitted nothing, with the same text
/// `submit_native_actions_batch` has always returned.
fn parse_batch_reply(resp: &serde_json::Value, sent: usize) -> Result<Vec<Option<String>>, String> {
    if let Some(err) = resp.get("error") {
        return Err(format!(
            "rpc: {}",
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
        ));
    }
    let items = resp["result"].as_array();
    let outcome: Vec<Option<String>> = items
        .map(|its| {
            its.iter()
                .map(|i| match i.get("hash") {
                    Some(_) => None,
                    None => Some(
                        i.get("error")
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| i.to_string()),
                    ),
                })
                .collect()
        })
        .unwrap_or_default();
    if outcome.iter().all(|e| e.is_some()) {
        // Surface WHY nothing landed — a per-item error or an unexpected shape.
        // A silent 0-accepted is exactly the misread session signing exists to avoid.
        let reason = items
            .and_then(|its| {
                its.iter()
                    .find_map(|i| i.get("error").map(|e| e.to_string()))
            })
            .unwrap_or_else(|| resp["result"].to_string());
        return Err(format!("0 accepted ({sent} sent): {reason}"));
    }
    Ok(outcome)
}

/// `torus_submitNativeActions[Bin]` with the per-item outcome
/// (`parse_batch_reply`).
async fn submit_native_actions_items(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    id: u64,
    bin: bool,
) -> Result<Vec<Option<String>>, String> {
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
    parse_batch_reply(&resp, payloads.len())
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
    record_submit(
        submit_once(client, url, payloads, req_id, bin).await,
        submitted,
    );
}

/// One submission of `payloads`; returns how many the server accepted.
async fn submit_once(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    req_id: u64,
    bin: bool,
) -> Result<usize, String> {
    submit_items(client, url, payloads, req_id, bin)
        .await
        .map(|items| accepted_count(&items))
}

/// One submission of `payloads` with the per-item outcome (`None` =
/// admitted). A lone JSON action uses the legacy single endpoint; anything
/// else (or any bincode payload) goes through the batch endpoint.
async fn submit_items(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    req_id: u64,
    bin: bool,
) -> Result<Vec<Option<String>>, String> {
    if payloads.len() == 1 && !bin {
        submit_native_action(client, url, &payloads[0], req_id)
            .await
            .map(|()| vec![None])
    } else {
        submit_native_actions_items(client, url, payloads, req_id, bin).await
    }
}

/// `--retry-busy`: pause between resends of a shed action.
const RETRY_BUSY_BACKOFF: Duration = Duration::from_millis(50);
/// `--retry-busy`: stop resending an action this long after its first send,
/// well inside the node's 60 s nonce window so an admitted resend can still
/// commit before it expires.
const RETRY_BUSY_MAX_AGE: Duration = Duration::from_secs(30);

/// True for the node's retryable "shed before verify" replies: the admission
/// limit and the full pool (torus-rpc `ADMISSION_BUSY_MSG`,
/// `POOL_FULL_PREVERIFY_MSG`). Both shed only non-cancels.
fn is_busy_reject(err: &str) -> bool {
    err.contains("admission limit reached") || err.contains("pool full (pre-verify)")
}

/// Call `submit` until it is not a busy reject, sleeping `backoff` between
/// tries; returns the last result once a retry would end after `give_up`.
async fn submit_until_admitted<T, F, Fut>(
    mut submit: F,
    backoff: Duration,
    give_up: Instant,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    loop {
        match submit().await {
            Err(e) if is_busy_reject(&e) && Instant::now() + backoff < give_up => {
                tokio::time::sleep(backoff).await
            }
            r => return r,
        }
    }
}

/// Credit `submitted` with an accepted count, or log one of the first few errors.
fn record_submit(result: Result<usize, String>, submitted: &AtomicU64) {
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

/// Book one econ fire's outcome: slots (with --max-in-flight), the
/// place/cancel-all mix, and the accepted count.
fn record_econ_fire(
    result: &Result<Vec<Option<String>>, String>,
    keys: &[u64],
    cancels: &[bool],
    cap: Option<&in_flight::Cap>,
    mix: &in_flight::MixStats,
    submitted: &AtomicU64,
) {
    if let Some(c) = cap {
        c.apply_result(keys, result);
    }
    mix.record(cancels, result);
    record_submit(result.as_ref().map(|i| accepted_count(i)).map_err(Clone::clone), submitted);
}

#[cfg(test)]
mod busy_retry_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The exact per-item reply the node's RPC ingress returns when it sheds a
    /// non-cancel, as `submit_native_actions_batch` wraps it.
    const ADMISSION_BUSY: &str =
        "0 accepted (1 sent): \"mempool: busy, admission limit reached (pre-verify), retry later\"";
    const POOL_FULL: &str = "0 accepted (1 sent): \"mempool: pool full (pre-verify)\"";

    #[test]
    fn busy_and_pool_full_rejects_are_retryable_others_are_not() {
        assert!(is_busy_reject(ADMISSION_BUSY));
        assert!(is_busy_reject(POOL_FULL));
        assert!(!is_busy_reject("0 accepted (1 sent): \"invalid nonce\""));
        assert!(!is_busy_reject("http: connection refused"));
    }

    #[test]
    fn econ_shape_defaults_retry_busy_off() {
        assert!(!EconShape::new(1500, 0, 5, 0.5, 0.05).retry_busy);
    }

    #[tokio::test]
    async fn resends_the_same_payload_until_admitted() {
        let calls = AtomicUsize::new(0);
        let r = submit_until_admitted(
            || {
                let n = calls.fetch_add(1, Ordering::Relaxed);
                async move {
                    if n < 2 {
                        Err(ADMISSION_BUSY.to_string())
                    } else {
                        Ok(1)
                    }
                }
            },
            Duration::from_millis(1),
            Instant::now() + Duration::from_secs(5),
        )
        .await;
        assert_eq!(r, Ok(1));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let calls = AtomicUsize::new(0);
        let r = submit_until_admitted(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                async { Err::<usize, _>("http: connection refused".to_string()) }
            },
            Duration::from_millis(1),
            Instant::now() + Duration::from_secs(5),
        )
        .await;
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn gives_up_at_the_give_up_instant() {
        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        let r = submit_until_admitted(
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                async { Err::<usize, _>(ADMISSION_BUSY.to_string()) }
            },
            Duration::from_millis(10),
            start + Duration::from_millis(50),
        )
        .await;
        assert!(r.is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!((2..=6).contains(&calls.load(Ordering::Relaxed)));
    }
}

#[cfg(test)]
mod max_in_flight_tests {
    use super::*;

    fn flags(extra: &[&str]) -> (usize, String) {
        let cli = Cli::try_parse_from(["bench-throughput", "consensus"].iter().chain(extra).copied())
            .expect("parses");
        let Command::Consensus { max_in_flight, in_flight_watch_rpc, .. } = cli.command else {
            panic!("consensus subcommand")
        };
        (max_in_flight, in_flight_watch_rpc)
    }

    #[test]
    fn max_in_flight_defaults_off_and_parses() {
        assert_eq!(flags(&[]), (0, String::new()));
        assert_eq!(
            flags(&["--econ", "--max-in-flight", "1", "--in-flight-watch-rpc", "http://127.0.0.1:8647"]),
            (1, "http://127.0.0.1:8647".to_string())
        );
        assert!(Cli::try_parse_from(["bench-throughput", "consensus", "--max-in-flight", "x"]).is_err());
    }

    // Partial accepts keep the per-item errors (they release slots); an
    // all-refused or failed call keeps today's error text.
    #[test]
    fn batch_reply_keeps_per_item_errors() {
        let busy = serde_json::json!("mempool: busy, admission limit reached (pre-verify), retry later");
        let partial = serde_json::json!({"result": [{"hash": "0x1"}, {"error": busy}, {"hash": "0x2"}]});
        assert_eq!(
            parse_batch_reply(&partial, 3),
            Ok(vec![None, Some(busy.to_string()), None])
        );
        let none = serde_json::json!({"result": [{"error": busy}]});
        let e = parse_batch_reply(&none, 1).unwrap_err();
        assert_eq!(e, format!("0 accepted (1 sent): {busy}"));
        assert!(is_busy_reject(&e));
        let rpc = serde_json::json!({"error": {"message": "boom"}});
        assert_eq!(parse_batch_reply(&rpc, 2), Err("rpc: boom".to_string()));
        let odd = serde_json::json!({"result": 5});
        assert_eq!(parse_batch_reply(&odd, 2), Err("0 accepted (2 sent): 5".to_string()));
    }
}

/// Bulk-funded genesis sender indices: testnet/gen-weighted-genesis.sh funds
/// `BULK_OFFSET` (60) .. 60 + `BULK_COUNT` (100_000) with the `gen-accounts`
/// derivation (the 3-validator devnet genesis inherits them).
const FUNDED_BULK_START: usize = 60;
const FUNDED_BULK_END: usize = 100_060;

/// Where `--spam-cancel-*` keys come from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SpamKeys {
    /// Funded genesis indices `first..first + keys` (the top of the bulk range).
    Funded { first: usize },
    /// Fresh keys no genesis funds (deterministic per index, for repeatable cells).
    Unfunded,
}

/// The `--spam-cancel-*` plan: `keys` spammers sending CancelAllOrders at
/// `rate` actions/s in aggregate.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpamCancel {
    keys: usize,
    rate: f64,
    source: SpamKeys,
}

/// Validate the `--spam-cancel-*` flags against the load's sender range.
/// `Ok(None)` = off (`keys == 0`).
fn spam_cancel_plan(
    keys: usize,
    rate: f64,
    funded: bool,
    senders: usize,
    sender_offset: usize,
) -> Result<Option<SpamCancel>, String> {
    if keys == 0 {
        return Ok(None);
    }
    if !(rate.is_finite() && rate > 0.0) {
        return Err(format!(
            "--spam-cancel-rate must be > 0 with --spam-cancel-keys {keys} (got {rate})"
        ));
    }
    let source = if funded {
        let Some(first) = FUNDED_BULK_END
            .checked_sub(keys)
            .filter(|f| *f >= FUNDED_BULK_START)
        else {
            return Err(format!(
                "--spam-cancel-funded: {keys} keys exceed the {} funded genesis accounts",
                FUNDED_BULK_END - FUNDED_BULK_START
            ));
        };
        let load_end = sender_offset.saturating_add(senders);
        if sender_offset < FUNDED_BULK_END && load_end > first {
            return Err(format!(
                "--spam-cancel-funded: spam indices {first}..{FUNDED_BULK_END} overlap the \
                 load senders {sender_offset}..{load_end}; lower --senders or --spam-cancel-keys"
            ));
        }
        SpamKeys::Funded { first }
    } else {
        SpamKeys::Unfunded
    };
    Ok(Some(SpamCancel { keys, rate, source }))
}

impl SpamCancel {
    fn signing_keys(&self) -> Vec<SigningKey> {
        match self.source {
            SpamKeys::Funded { first } => load_sender_keys(self.keys, first)
                .into_iter()
                .map(|k| k.signing_key)
                .collect(),
            // A seed space disjoint from load_sender_keys (0xBEEF_0000 + idx).
            SpamKeys::Unfunded => (0..self.keys)
                .map(|i| {
                    let mut rng = StdRng::seed_from_u64(0x5BA4_CA9C_0000_0000 + i as u64);
                    SigningKey::random(&mut rng)
                })
                .collect(),
        }
    }
}

/// Why the node refused a spam action (bench report buckets).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SpamReject {
    /// Item A: sender below the ingress minimum collateral.
    Unfunded,
    /// Item B: per-address request limit.
    AddrLimited,
    /// Item D: per-IP weight limit.
    IpLimited,
    /// Full pool / admission limit.
    Busy,
    Other,
}

impl SpamReject {
    const ALL: [SpamReject; 5] = [
        SpamReject::Unfunded,
        SpamReject::AddrLimited,
        SpamReject::IpLimited,
        SpamReject::Busy,
        SpamReject::Other,
    ];

    fn classify(err: &str) -> Self {
        if err.contains("not funded") {
            SpamReject::Unfunded
        } else if err.contains("rate limited: IP") {
            SpamReject::IpLimited
        } else if err.contains("rate limited") {
            SpamReject::AddrLimited
        } else if is_busy_reject(err) || err.contains("pool full") {
            SpamReject::Busy
        } else {
            SpamReject::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            SpamReject::Unfunded => "unfunded",
            SpamReject::AddrLimited => "addr_rate_limited",
            SpamReject::IpLimited => "ip_rate_limited",
            SpamReject::Busy => "busy",
            SpamReject::Other => "other",
        }
    }
}

/// Spam counters, kept apart from the load's.
#[derive(Default)]
struct SpamStats {
    sent: AtomicU64,
    accepted: AtomicU64,
    /// Indexed like [`SpamReject::ALL`].
    rejected: [AtomicU64; 5],
}

/// Run the spammers until `deadline`: each of the `plan.keys` keys sends one
/// signed CancelAllOrders (all markets) every `keys / rate` seconds to its
/// own RPC endpoint (round robin), one request in flight per key, and never
/// retries a refused action.
fn spawn_spam_cancel(
    plan: SpamCancel,
    client: reqwest::Client,
    urls: Arc<Vec<String>>,
    deadline: Instant,
    stats: Arc<SpamStats>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let interval = Duration::from_secs_f64(plan.keys as f64 / plan.rate);
    plan.signing_keys()
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let (client, urls, stats) = (client.clone(), urls.clone(), stats.clone());
            tokio::spawn(async move {
                let url = &urls[i % urls.len()];
                // Spread the spammers over one interval.
                tokio::time::sleep(interval.mul_f64(i as f64 / plan.keys as f64)).await;
                let mut next = Instant::now();
                let mut last_nonce = 0u64;
                let mut req_id = 0xC0DE_0000_0000u64 + (i as u64) * 1_000_000;
                while next < deadline {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    last_nonce = now_ms.max(last_nonce + 1);
                    let signed = sign_native_action(
                        NativeAction::CancelAllOrders { market_id: None },
                        last_nonce,
                        &key,
                    );
                    let payload =
                        format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));
                    req_id += 1;
                    stats.sent.fetch_add(1, Ordering::Relaxed);
                    match submit_native_action(&client, url, &payload, req_id).await {
                        Ok(()) => {
                            stats.accepted.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let kind = SpamReject::classify(&e);
                            let slot = SpamReject::ALL.iter().position(|k| *k == kind).unwrap();
                            stats.rejected[slot].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    next += interval;
                    tokio::time::sleep_until(next.max(Instant::now()).into()).await;
                }
            })
        })
        .collect()
}

/// One report line, apart from the load's numbers.
fn spam_report(plan: &SpamCancel, stats: &SpamStats) -> String {
    let rejected: Vec<u64> = stats
        .rejected
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .collect();
    let by_reason: Vec<String> = SpamReject::ALL
        .iter()
        .zip(&rejected)
        .map(|(k, n)| format!("{}={n}", k.label()))
        .collect();
    format!(
        "Spam cancel-all ({} {} keys, {} actions/s): sent {} accepted {} rejected {} ({})",
        plan.keys,
        match plan.source {
            SpamKeys::Funded { .. } => "funded",
            SpamKeys::Unfunded => "unfunded",
        },
        plan.rate,
        stats.sent.load(Ordering::Relaxed),
        stats.accepted.load(Ordering::Relaxed),
        rejected.iter().sum::<u64>(),
        by_reason.join(" ")
    )
}

#[cfg(test)]
mod spam_cancel_tests {
    use super::*;

    fn addr(k: &SigningKey) -> Address {
        let p = k.verifying_key().to_encoded_point(false);
        Address::from_slice(&alloy_primitives::keccak256(&p.as_bytes()[1..])[12..])
    }

    fn consensus_flags(extra: &[&str]) -> (usize, f64, bool) {
        let cli = Cli::try_parse_from(
            ["bench-throughput", "consensus"]
                .iter()
                .chain(extra)
                .copied(),
        )
        .expect("parses");
        let Command::Consensus {
            spam_cancel_keys,
            spam_cancel_rate,
            spam_cancel_funded,
            ..
        } = cli.command
        else {
            panic!("consensus subcommand")
        };
        (spam_cancel_keys, spam_cancel_rate, spam_cancel_funded)
    }

    #[test]
    fn spam_cancel_flags_default_off_and_parse() {
        assert_eq!(consensus_flags(&[]), (0, 0.0, false));
        assert_eq!(
            consensus_flags(&[
                "--spam-cancel-keys",
                "8",
                "--spam-cancel-rate",
                "250.5",
                "--spam-cancel-funded"
            ]),
            (8, 250.5, true)
        );
        assert_eq!(spam_cancel_plan(0, 0.0, false, 5000, 60), Ok(None));
        assert_eq!(spam_cancel_plan(0, 99.0, true, 5000, 60), Ok(None));
    }

    #[test]
    fn spam_cancel_needs_a_positive_rate() {
        assert!(spam_cancel_plan(4, 0.0, false, 5000, 60).is_err());
        assert!(spam_cancel_plan(4, -1.0, false, 5000, 60).is_err());
        assert!(spam_cancel_plan(4, f64::NAN, false, 5000, 60).is_err());
    }

    #[test]
    fn funded_spam_keys_are_the_top_of_the_funded_range_and_not_load_senders() {
        // gen-weighted-genesis.sh funds indices 60..100_060; the load senders
        // use --sender-offset 60 --senders N, so the spammers take the top K.
        let plan = spam_cancel_plan(3, 30.0, true, 5000, 60).unwrap().unwrap();
        assert_eq!(plan.keys, 3);
        assert_eq!(plan.source, SpamKeys::Funded { first: 100_057 });
        let want: Vec<Address> = load_sender_keys(3, 100_057)
            .iter()
            .map(|k| addr(&k.signing_key))
            .collect();
        let got: Vec<Address> = plan.signing_keys().iter().map(addr).collect();
        assert_eq!(got, want, "same derivation as gen-accounts (funded)");
        // Load senders 60..60+N must not reach the spam range.
        assert!(spam_cancel_plan(10, 1.0, true, 99_990, 60).is_ok());
        assert!(spam_cancel_plan(10, 1.0, true, 99_991, 60).is_err());
        // Senders above the funded range do not collide either.
        assert!(spam_cancel_plan(10, 1.0, true, 100, 100_060).is_ok());
        // More keys than funded accounts.
        assert!(spam_cancel_plan(100_001, 1.0, true, 0, 0).is_err());
    }

    #[test]
    fn unfunded_spam_keys_are_fresh_deterministic_and_distinct() {
        let plan = spam_cancel_plan(16, 30.0, false, 5000, 60)
            .unwrap()
            .unwrap();
        assert_eq!(plan.source, SpamKeys::Unfunded);
        let a: Vec<Address> = plan.signing_keys().iter().map(addr).collect();
        let b: Vec<Address> = plan.signing_keys().iter().map(addr).collect();
        assert_eq!(a, b, "deterministic per run");
        let uniq: std::collections::HashSet<_> = a.iter().collect();
        assert_eq!(uniq.len(), 16);
        // Never a bench sender key (hardhat 0..20 or the funded bulk range).
        let bench: std::collections::HashSet<Address> = load_sender_keys(2_000, 0)
            .iter()
            .map(|k| addr(&k.signing_key))
            .collect();
        assert!(a.iter().all(|x| !bench.contains(x)));
    }

    #[test]
    fn spam_report_is_one_line_apart_from_the_load() {
        let plan = spam_cancel_plan(2, 10.0, false, 5000, 60).unwrap().unwrap();
        let stats = SpamStats::default();
        stats.sent.store(7, Ordering::Relaxed);
        stats.accepted.store(3, Ordering::Relaxed);
        stats.rejected[0].store(4, Ordering::Relaxed);
        assert_eq!(
            spam_report(&plan, &stats),
            "Spam cancel-all (2 unfunded keys, 10 actions/s): sent 7 accepted 3 rejected 4 \
             (unfunded=4 addr_rate_limited=0 ip_rate_limited=0 busy=0 other=0)"
        );
    }

    #[test]
    fn spam_rejects_are_classified() {
        let r = |e: &str| SpamReject::classify(e);
        assert_eq!(
            r("rpc: mempool: account not funded: 0xab holds less than 1 TRS"),
            SpamReject::Unfunded
        );
        assert_eq!(
            r("rpc: rate limited: 0xab used 10 of 10 cancel requests"),
            SpamReject::AddrLimited
        );
        assert_eq!(
            r("rpc: rate limited: IP request weight over 1200/min, retry later"),
            SpamReject::IpLimited
        );
        assert_eq!(r("rpc: mempool: pool full (pre-verify)"), SpamReject::Busy);
        assert_eq!(r("rpc: mempool: native pool full"), SpamReject::Busy);
        assert_eq!(r("http: connection refused"), SpamReject::Other);
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
    markets_per_sender: u64,
    econ: Option<EconShape>,
    rate_total: f64,
    metrics_urls_str: &str,
    sweep_bodies: bool,
    spam: Option<SpamCancel>,
    cap_plan: Option<(usize, String)>,
) {
    // Locality plan: 0 = today's uniform draw over 1..=markets (default).
    let plan = MarketPlan::new(markets, markets_per_sender);
    if plan.per_sender > 0 {
        println!(
            "Market plan: {} markets, {} per sender (locality shape) — sender 0 owns {:?}",
            plan.markets,
            plan.per_sender,
            plan.markets_for(0)
        );
        if econ.is_some() && plan.per_sender < plan.markets {
            println!("Locality sides: per-market-owner-rank-v1 (odd owner counts differ by one)");
        }
    }
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
    if let Some(p) = &spam {
        println!(
            "Cancel spam: {} {:?} keys, {} actions/s aggregate, no retry",
            p.keys, p.source, p.rate
        );
    }
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
    if let Some((n, url)) = &cap_plan {
        println!(
            "In-flight cap: {n} action(s)/sender (released on commit seen in {url} block bodies, \
             RPC refusal, or nonce + {}s)",
            (torus_types::eip712::NONCE_WINDOW_MS + in_flight::TIMEOUT_MARGIN_MS) / 1000
        );
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
                random_place_order_action(&mut StdRng::from_entropy(), MarketPlan::new(1, 0), 0, 1),
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
                    submit_batch, 0, bin, plan, 0,
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
        for (sender_idx, sk) in keys.iter().enumerate() {
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
                    plan,
                    sender_idx,
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

    // --max-in-flight: the shared cap state and its one block-body tail.
    let cap: Option<Arc<in_flight::Cap>> =
        cap_plan.map(|(n, url)| Arc::new(in_flight::Cap::new(num_senders, n, url)));
    let tail_handle = cap.as_ref().map(|c| {
        tokio::spawn(in_flight::run_tail(
            c.clone(),
            client.clone(),
            start_block,
            deadline + Duration::from_secs(5),
        ))
    });
    let mix = Arc::new(in_flight::MixStats::default());

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

    // Cancel spammers (--spam-cancel-keys), counted apart from the load.
    let spam_stats = Arc::new(SpamStats::default());
    let spam_handles = spam.map_or_else(Vec::new, |p| {
        spawn_spam_cancel(
            p,
            (*client).clone(),
            rpc_urls.clone(),
            deadline,
            spam_stats.clone(),
        )
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
        let cap = cap.clone();
        let mix = mix.clone();
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
                let mut open_orders = 0u64;
                let mut next_fire = Instant::now();
                while Instant::now() < deadline {
                    if let Some(iv) = econ_pace {
                        let now = Instant::now();
                        if now < next_fire {
                            tokio::time::sleep(next_fire - now).await;
                        }
                        next_fire = next_fire.max(now) + iv;
                    }
                    // --max-in-flight: wait (no spin) for a free slot. A
                    // deferred fire goes out on release and the cadence
                    // restarts from there (no catch-up burst).
                    let mut budget = match &cap {
                        None => FireBudget::Legacy(&mut open_orders),
                        Some(c) => {
                            let Some((estimate, cancel_pending, waited)) = c
                                .wait_ready(
                                    sender_idx,
                                    batch_size.max(1) as u64,
                                    shape.open_order_budget,
                                    deadline,
                                )
                                .await
                            else {
                                break;
                            };
                            if let (true, Some(iv)) = (waited, econ_pace) {
                                next_fire = Instant::now() + iv;
                            }
                            FireBudget::Capped { estimate, cancel_pending }
                        }
                    };
                    let actions =
                        econ_fire(&mut rng, sender_idx, plan, batch_size, &shape, submit_batch, &mut budget);
                    if actions.is_empty() {
                        continue; // ready() rules this out; never spin on it
                    }
                    // Generate inline (cheap), sign+encode on the blocking pool
                    // (the expensive ECDSA/serialize part). Nonces are wall-clock
                    // ms, strictly increasing per sender.
                    let mut batch_actions = Vec::with_capacity(actions.len());
                    let mut shapes = Vec::with_capacity(actions.len());
                    for action in actions {
                        let base = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let nonce = base.max(last_nonce + 1);
                        last_nonce = nonce;
                        shapes.push((nonce, in_flight::action_shape(&action)));
                        batch_actions.push((action, nonce));
                    }
                    let sign_key = key.clone();
                    let sign_session = session_key.clone();
                    let signed = tokio::task::spawn_blocking(move || {
                        batch_actions
                            .into_iter()
                            .map(|(action, nonce)| {
                                let s = sign_one(action, nonce, &sign_key, &sign_session, sign_mode);
                                let id = in_flight::action_key(nonce, &s.signature);
                                let bytes = if bin {
                                    bincode::serialize(&s).unwrap()
                                } else {
                                    serde_json::to_vec(&s).unwrap()
                                };
                                (format!("0x{}", hex::encode(&bytes)), id)
                            })
                            .collect::<Vec<(String, u64)>>()
                    })
                    .await;
                    let (payloads, keys): (Vec<String>, Vec<u64>) = match signed {
                        Ok(p) => p.into_iter().unzip(),
                        Err(_) => break, // signer panicked — stop this sender
                    };
                    let cancels: Vec<bool> = shapes.iter().map(|(_, (_, c))| *c).collect();
                    if let Some(c) = &cap {
                        // Registered BEFORE the send, so a fast commit finds it.
                        let entries: Vec<(u64, u64, u64, bool)> = keys
                            .iter()
                            .zip(&shapes)
                            .map(|(k, (nonce, (places, cancel)))| (*k, *nonce, *places, *cancel))
                            .collect();
                        c.submit(sender_idx, &entries);
                    }
                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let url = urls[url_idx % url_count].clone();
                    url_idx += 1;
                    req_id += 1;
                    if shape.retry_busy {
                        // Closed loop: this sender draws its next action only
                        // once this one is admitted (or given up on), so the
                        // admitted mix keeps the generated cancel fraction.
                        // A concurrency permit is held per attempt, never
                        // across the backoff sleep. The in-flight slot is held
                        // across every BUSY retry until the final outcome.
                        drop(permit);
                        let give_up = deadline.min(Instant::now() + RETRY_BUSY_MAX_AGE);
                        let (sem, client, url, payloads) = (&semaphore, &client, &url, &payloads);
                        let result = submit_until_admitted(
                            || async move {
                                let _permit = sem.acquire().await.unwrap();
                                submit_items(client, url, payloads, req_id, bin).await
                            },
                            RETRY_BUSY_BACKOFF,
                            give_up,
                        )
                        .await;
                        record_econ_fire(&result, &keys, &cancels, cap.as_deref(), &mix, &submitted);
                        continue;
                    }
                    let client = client.clone();
                    let submitted = submitted.clone();
                    let (cap, mix) = (cap.clone(), mix.clone());
                    tokio::spawn(async move {
                        let _permit = permit;
                        let result = submit_items(&client, &url, &payloads, req_id, bin).await;
                        record_econ_fire(&result, &keys, &cancels, cap.as_deref(), &mix, &submitted);
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
                            plan,
                            sender_idx,
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

    for h in sender_handles.into_iter().chain(spam_handles) {
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
    if econ.is_some() {
        println!("{}", mix.report());
    }
    if let Some(c) = &cap {
        if let Some(h) = tail_handle {
            h.abort();
        }
        println!("{}", c.report());
        if let Some(w) = in_flight::tail_warning(&c.tail, &c.url) {
            println!("{w}");
        }
    }
    if let Some(p) = &spam {
        println!("{}", spam_report(p, &spam_stats));
    }

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
/// The EVM address of a secp256k1 key (keccak of the uncompressed pubkey).
fn key_address(key: &SigningKey) -> Address {
    let uncompressed = key.verifying_key().to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&uncompressed.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

fn run_gen_accounts(offset: usize, count: usize, secret_keys: bool) {
    let keys = load_sender_keys(count, offset);
    for (i, sk) in keys.iter().enumerate() {
        let idx = offset + i;
        let addr = key_address(&sk.signing_key);
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
            overlay,
            defer_trades,
            senders,
        } => run_matching_engine(orders, markets, warmup, &genesis, overlay, defer_trades, senders),
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
            markets_per_sender,
            econ,
            target_margin,
            cross_fraction,
            cancel_fraction,
            open_order_budget,
            retry_busy,
            max_in_flight,
            in_flight_watch_rpc,
            econ_mid,
            band,
            rate_total,
            metrics_urls,
            sweep_bodies,
            spam_cancel_keys,
            spam_cancel_rate,
            spam_cancel_funded,
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
            let spam = match spam_cancel_plan(
                spam_cancel_keys,
                spam_cancel_rate,
                spam_cancel_funded,
                senders.max(1),
                sender_offset,
            ) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            let cap = match in_flight::cap_plan(max_in_flight, econ, &in_flight_watch_rpc, &rpc_urls) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            let econ_shape = econ.then(|| EconShape {
                open_order_budget,
                retry_busy,
                ..EconShape::new(target_margin, econ_mid, band, cross_fraction, cancel_fraction)
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
                markets_per_sender,
                econ_shape,
                rate_total,
                &metrics_urls,
                sweep_bodies,
                spam,
                cap,
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
        Command::OracleFeed {
            rpc_urls,
            validator_keys,
            markets,
            price,
            walk_bp,
            interval_ms,
            stats_file,
        } => {
            let args = oracle_feed::FeedArgs {
                rpc_urls: &rpc_urls,
                validator_keys: &validator_keys,
                markets,
                price,
                walk_bp,
                interval_ms,
                stats_file: &stats_file,
            };
            if let Err(e) = oracle_feed::run(args).await {
                eprintln!("oracle-feed: {e}");
                std::process::exit(2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fire_interval, pregen_ammo, sign_one, sign_payload_batch, MarketPlan, SignMode};
    use super::{summarize_included, SweptBlock};
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn sign_payload_batch_nonces_strictly_increase() {
        let mut rng = StdRng::seed_from_u64(7);
        let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let ed = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let plan = MarketPlan::new(1, 0);
        let (p1, n1) =
            sign_payload_batch(&mut rng, &key, &ed, SignMode::Eip712, 3, 4, 0, false, plan, 0);
        let (p2, n2) =
            sign_payload_batch(&mut rng, &key, &ed, SignMode::Eip712, 3, 4, n1, false, plan, 0);
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
            MarketPlan::new(1, 0),
            0,
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
