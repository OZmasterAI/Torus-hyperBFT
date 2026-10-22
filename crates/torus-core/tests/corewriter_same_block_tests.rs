//! Item 7 T (docs/plans/item7-step3-design.md section 3 "T", questions 8 a
//! and 9 a): same-block EVM -> native for the non-order CoreWriter kinds,
//! behind the P0.0 feature `Feature::SameBlockCoreWriter`.
//!
//! With the bit on in the queuing block's header, a non-order kind
//! (lockbox deposit / withdraw, delegate, undelegate, claim rewards, lock
//! permanent, claim unbonded) queued in block h targets h itself, in the
//! target's same-block seq range `0..200`; an order kind keeps
//! `max(h + K, last order target)` in the order range `200..400`. Each range
//! has its own cap of 200, its own dense seqs and its own spill walk; a
//! target drains in key order (same-block range first). With the bit off,
//! everything is as before (`CoreWriterQueue::enqueue`).

use torus_core::precompiles::*;
use torus_state::cf::{CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{Address, FixedPoint, Feature, U256};

const CAP: u64 = torus_types::CORE_WRITER_DRAIN_CAP;
const ORDER_BASE: u64 = torus_types::CORE_WRITER_ORDER_SEQ_BASE;

fn setup() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn queue_key(block: u64, seq: u64) -> Vec<u8> {
    let mut key = block.to_be_bytes().to_vec();
    key.extend_from_slice(&seq.to_be_bytes());
    key
}

fn seq_of(key: &[u8]) -> u64 {
    u64::from_be_bytes(key[8..16].try_into().unwrap())
}

fn target_of(key: &[u8]) -> u64 {
    u64::from_be_bytes(key[..8].try_into().unwrap())
}

fn qa(kind: QueuedActionKind, block_queued: u64) -> QueuedAction {
    QueuedAction {
        trader: addr(1),
        kind,
        block_queued,
    }
}

/// Enqueue with T on; returns `(target, seq)` of the row it wrote (the
/// only queue key that is new).
fn enqueue_t(db: &StateDb, kind: QueuedActionKind, block: u64) -> (u64, u64) {
    let before: std::collections::BTreeSet<Vec<u8>> = db
        .iterate_cf(CF_CORE_WRITER_QUEUE, None)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let seq = CoreWriterQueue::enqueue_with(db, &qa(kind, block), true).unwrap();
    let new: Vec<Vec<u8>> = db
        .iterate_cf(CF_CORE_WRITER_QUEUE, None)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| !before.contains(k))
        .collect();
    assert_eq!(new.len(), 1, "one new row");
    assert_eq!(seq_of(&new[0]), seq, "returned seq = the row's seq");
    (target_of(&new[0]), seq)
}

fn drain_seqs(db: &StateDb, block: u64) -> Vec<(u64, QueuedAction)> {
    CoreWriterQueue::drain_keyed(db, block)
        .unwrap()
        .into_iter()
        .map(|(s, a)| (s, a.expect("decodable")))
        .collect()
}

fn non_order_kinds() -> Vec<QueuedActionKind> {
    vec![
        QueuedActionKind::LockboxDeposit { amount: fp(1) },
        QueuedActionKind::LockboxWithdraw { amount: fp(1) },
        QueuedActionKind::Delegate {
            validator: addr(9),
            amount: fp(1),
        },
        QueuedActionKind::Undelegate {
            validator: addr(9),
            amount: fp(1),
        },
        QueuedActionKind::ClaimRewards,
        QueuedActionKind::LockPermanent { amount: fp(1) },
        QueuedActionKind::ClaimUnbonded,
    ]
}

fn order_kinds() -> Vec<QueuedActionKind> {
    vec![
        QueuedActionKind::PlaceOrder {
            market_id: 1,
            side: 0,
            order_type: 0,
            price: fp(100),
            quantity: fp(1),
            time_in_force: 0,
            cloid: None,
        },
        QueuedActionKind::CancelOrder { order_id: 7 },
        QueuedActionKind::CancelAll { market_id: 1 },
        QueuedActionKind::CancelByCloid {
            market_id: 1,
            cloid: 5,
        },
    ]
}

/// Test 12 (first point): non-order kinds target the queuing block (seqs
/// 0..), order kinds h + K in the order range (seqs 200..). The target
/// drains in key order: the same-block rows first.
#[test]
fn t_non_order_kinds_target_the_queuing_block_and_orders_the_order_range() {
    let (_dir, db) = setup();
    for (i, kind) in non_order_kinds().into_iter().enumerate() {
        assert!(!kind.is_order());
        assert_eq!(enqueue_t(&db, kind, 100), (100, i as u64), "non-order {i}");
    }
    for (i, kind) in order_kinds().into_iter().enumerate() {
        assert!(kind.is_order());
        assert_eq!(
            enqueue_t(&db, kind, 100),
            (110, ORDER_BASE + i as u64),
            "order {i} (K = 10)"
        );
    }
    // A later block's order at 101 targets 111, its deposit 101.
    assert_eq!(enqueue_t(&db, order_kinds()[1].clone(), 101), (111, ORDER_BASE));
    assert_eq!(enqueue_t(&db, non_order_kinds()[0].clone(), 101), (101, 0));
    let drained = drain_seqs(&db, 100);
    assert_eq!(
        drained.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        (0..7).collect::<Vec<u64>>()
    );
    assert!(drained.iter().all(|(_, a)| a.block_queued == 100 && !a.kind.is_order()));
    let drained = drain_seqs(&db, 110);
    assert_eq!(
        drained.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        (ORDER_BASE..ORDER_BASE + 4).collect::<Vec<u64>>()
    );
    // The same-block kinds write no cursor rows (their walk needs none).
    for key in [
        CORE_WRITER_FIRST_OPEN_TARGET_KEY,
        CORE_WRITER_FULL_ORDER_RUN_KEY,
    ] {
        assert_eq!(db.get_cf_raw(CF_FEE_CONFIG, key).unwrap(), None);
    }
}

/// Test 12 (second point, question 8 a): target h full of delayed orders
/// (200 rows in the order range) and a deposit queued at h still lands at h
/// (seq 0), and drains first. The 201st order spills to h + 1 in the order
/// range only.
#[test]
fn t_a_full_order_range_does_not_push_a_same_block_row_out() {
    let (_dir, db) = setup();
    for i in 0..CAP {
        let (t, s) = enqueue_t(
            &db,
            QueuedActionKind::CancelOrder {
                order_id: u128::from(i),
            },
            90,
        );
        assert_eq!((t, s), (100, ORDER_BASE + i));
    }
    // The 201st order of 90: 100's order range is full -> 101, seq 200.
    assert_eq!(
        enqueue_t(&db, QueuedActionKind::CancelOrder { order_id: 999 }, 90),
        (101, ORDER_BASE)
    );
    assert_eq!(
        enqueue_t(&db, QueuedActionKind::LockboxDeposit { amount: fp(3) }, 100),
        (100, 0),
        "the deposit keeps its block"
    );
    let drained = drain_seqs(&db, 100);
    assert_eq!(drained.len() as u64, CAP + 1, "<= 400 rows per block");
    assert!(matches!(
        drained[0].1.kind,
        QueuedActionKind::LockboxDeposit { .. }
    ));
    assert!(drained[1..].iter().all(|(_, a)| a.kind.is_order()));
}

/// Test 12 (third and fourth points): 201 same-block rows in one block: the
/// 201st drains at h + 1 (seq 0), in order; with h + 1's same-block range
/// full too, a row spills to h + 2. Order rows in the spill targets do not
/// count against the same-block range.
#[test]
fn t_same_block_rows_spill_within_their_own_range() {
    let (_dir, db) = setup();
    // Orders filling 101's and 102's order ranges (queued at 91 and 92).
    for (block, n) in [(91u64, CAP), (92, CAP)] {
        for i in 0..n {
            enqueue_t(
                &db,
                QueuedActionKind::CancelOrder {
                    order_id: u128::from(block * 1000 + i),
                },
                block,
            );
        }
    }
    let claim = || QueuedActionKind::ClaimRewards;
    for i in 0..CAP {
        assert_eq!(enqueue_t(&db, claim(), 100), (100, i));
    }
    assert_eq!(enqueue_t(&db, claim(), 100), (101, 0), "201st -> h + 1");
    for i in 1..CAP {
        assert_eq!(enqueue_t(&db, claim(), 100), (101, i));
    }
    assert_eq!(enqueue_t(&db, claim(), 100), (102, 0), "h, h + 1 full -> h + 2");
    // Bounded probes: the walk from 100 reads one slot per full target.
    let d101 = drain_seqs(&db, 101);
    assert_eq!(d101.len() as u64, 2 * CAP);
    assert!(d101[..CAP as usize]
        .iter()
        .all(|(_, a)| matches!(a.kind, QueuedActionKind::ClaimRewards)));
    assert!(d101[CAP as usize..].iter().all(|(_, a)| a.kind.is_order()));
}

/// With the bit off, `enqueue_with(.., false)` is `enqueue`: the same rows
/// (targets, seqs, cursor rows) over a random stream of every kind.
#[test]
fn t_off_is_exactly_the_old_enqueue() {
    let (_d1, a) = setup();
    let (_d2, b) = setup();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let kinds: Vec<QueuedActionKind> = non_order_kinds().into_iter().chain(order_kinds()).collect();
    for block in 100..140u64 {
        if next(5) == 0 {
            for db in [&a, &b] {
                CoreWriterQueue::drain(db, block).unwrap();
            }
        }
        for _ in 0..[0, 30, 250][next(3) as usize] {
            let kind = kinds[next(kinds.len() as u64) as usize].clone();
            let x = CoreWriterQueue::enqueue(&a, &qa(kind.clone(), block)).unwrap();
            let y = CoreWriterQueue::enqueue_with(&b, &qa(kind, block), false).unwrap();
            assert_eq!(x, y);
        }
    }
    for cf in [CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG] {
        assert_eq!(a.iterate_cf(cf, None).unwrap(), b.iterate_cf(cf, None).unwrap(), "{cf}");
    }
}

/// Activation boundary at the queue level: rows queued at H - 1 (bit off)
/// keep their targets and seqs; at H (bit on) a same-block row joins H's
/// target after the H - 1 rows (dense in the same-block range), orders take
/// the order range, and the old order rows (shared range, seqs 0..) of a
/// target after H drain before the new ones (queue order). An order queued
/// at H never targets a block before the last order target of H - 1.
#[test]
fn t_activation_boundary_rows_keep_queue_order() {
    let (_dir, db) = setup();
    const H: u64 = 120;
    // H - 1, bit off: two deposits target H (seqs 0, 1), two orders H - 1 + 10.
    for _ in 0..2 {
        CoreWriterQueue::enqueue_with(
            &db,
            &qa(QueuedActionKind::LockboxDeposit { amount: fp(1) }, H - 1),
            false,
        )
        .unwrap();
    }
    for id in 0..2u128 {
        CoreWriterQueue::enqueue_with(
            &db,
            &qa(QueuedActionKind::CancelOrder { order_id: id }, H - 1),
            false,
        )
        .unwrap();
    }
    assert_eq!(CoreWriterQueue::pending_count(&db, H).unwrap(), 2);
    assert_eq!(CoreWriterQueue::pending_count(&db, H + 9).unwrap(), 2);
    // Governance lowers K to 1 at H: the order still waits for H + 9.
    db.put_cf_raw(
        CF_FEE_CONFIG,
        torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
        b"1",
    )
    .unwrap();
    // H, bit on.
    assert_eq!(
        enqueue_t(&db, QueuedActionKind::LockboxWithdraw { amount: fp(1) }, H),
        (H, 2),
        "after the H - 1 rows, same range"
    );
    assert_eq!(
        enqueue_t(&db, QueuedActionKind::CancelOrder { order_id: 9 }, H),
        (H + 9, ORDER_BASE),
        "never before the last order target"
    );
    let d = drain_seqs(&db, H);
    assert_eq!(
        d.iter().map(|(s, a)| (*s, a.block_queued)).collect::<Vec<_>>(),
        vec![(0, H - 1), (1, H - 1), (2, H)]
    );
    let d = drain_seqs(&db, H + 9);
    assert_eq!(
        d.iter()
            .map(|(s, a)| match a.kind {
                QueuedActionKind::CancelOrder { order_id } => (*s, order_id),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>(),
        vec![(0, 0), (1, 1), (ORDER_BASE, 9)]
    );
}

/// Differential check of T's enqueue against the two-range walk (probe every
/// target from the candidate, per range), across an activation at block
/// `H`: before it the step 2 model (one range), from it the T model. Random
/// loads above and below the cap, every kind, K changes, reverted txs, and
/// the drain of each block (key order). Every row's target, seq and drain
/// position must match the model. Point reads per enqueue stay bounded.
#[test]
fn t_enqueue_places_rows_like_the_two_range_walk_across_activation() {
    use std::collections::BTreeMap;
    const H: u64 = 130;
    #[derive(Clone, Default)]
    struct Model {
        /// target -> seq -> row id
        targets: BTreeMap<u64, BTreeMap<u64, u64>>,
        last_order: u64,
    }
    impl Model {
        fn count(&self, t: u64, range: std::ops::Range<u64>) -> u64 {
            self.targets
                .get(&t)
                .map_or(0, |r| r.range(range).count() as u64)
        }
        fn next_seq(&self, t: u64, range: std::ops::Range<u64>) -> u64 {
            let start = range.start;
            self.targets
                .get(&t)
                .and_then(|r| r.range(range).next_back().map(|(s, _)| s + 1))
                .unwrap_or(start)
        }
        fn place(&mut self, id: u64, order: bool, block: u64, k: u64, t_on: bool) -> (u64, u64) {
            let range = match (t_on, order) {
                (false, _) => 0..CAP,
                (true, false) => 0..CAP,
                (true, true) => ORDER_BASE..ORDER_BASE + CAP,
            };
            let mut t = match (t_on, order) {
                (_, true) => (block + k).max(self.last_order),
                (false, false) => block + 1,
                (true, false) => block,
            };
            // Before T (one range) "full" = 200 rows; under T per range.
            while self.count(t, range.clone()) == CAP {
                t += 1;
            }
            let seq = self.next_seq(t, range);
            self.targets.entry(t).or_default().insert(seq, id);
            if order {
                self.last_order = t;
            }
            (t, seq)
        }
    }

    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let (_dir, db) = setup();
    let mut model = Model::default();
    let mut k = 10u64;
    let mut id = 0u64;
    let mut max_reads_t = 0usize;
    let id_of = |a: &QueuedAction| match a.kind {
        QueuedActionKind::CancelOrder { order_id } => order_id as u64,
        QueuedActionKind::LockboxWithdraw { amount } => amount.raw() as u64,
        _ => unreachable!(),
    };
    for block in 100..170u64 {
        let t_on = block >= H;
        // The drain of `block` runs after its EVM part (T: same-block rows
        // of `block` included), in key order.
        let txs = [0, 40, 150, 260][next(4) as usize];
        let order_share = [0, 50, 100][next(3) as usize];
        if next(6) == 0 {
            k = 1 + next(12);
            db.put_cf_raw(
                CF_FEE_CONFIG,
                torus_types::CORE_WRITER_ORDER_DELAY_PARAM.as_bytes(),
                k.to_string().as_bytes(),
            )
            .unwrap();
        }
        for _ in 0..txs {
            let overlay = NativeStateOverlay::new(db.clone());
            let mut tentative = model.clone();
            for _ in 0..1 + next(3) {
                id += 1;
                let order = next(100) < order_share;
                let kind = if order {
                    QueuedActionKind::CancelOrder {
                        order_id: u128::from(id),
                    }
                } else {
                    QueuedActionKind::LockboxWithdraw {
                        amount: FixedPoint::from_raw(id as i128),
                    }
                };
                let counted = PointReads::new(overlay.clone());
                let seq = CoreWriterQueue::enqueue_with(&counted, &qa(kind, block), t_on).unwrap();
                if t_on {
                    max_reads_t = max_reads_t.max(counted.count());
                }
                let (target, want) = tentative.place(id, order, block, k, t_on);
                assert_eq!(seq, want, "row {id} (order={order}, t_on={t_on}) at {target}");
                assert!(
                    overlay
                        .get_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(target, seq))
                        .unwrap()
                        .is_some(),
                    "row {id} at ({target}, {seq})"
                );
            }
            if next(4) == 0 {
                drop(overlay); // reverted
            } else {
                overlay.commit_tx(&db).unwrap();
                model = tentative;
            }
        }
        let drained: Vec<(u64, u64)> = drain_seqs(&db, block)
            .iter()
            .map(|(s, a)| (*s, id_of(a)))
            .collect();
        let want: Vec<(u64, u64)> = model
            .targets
            .remove(&block)
            .unwrap_or_default()
            .into_iter()
            .collect();
        assert_eq!(drained, want, "drain of {block}");
        assert!(drained.len() as u64 <= 2 * CAP, "<= 400 rows per block");
        if t_on {
            // Under T a block's same-block rows never outlive it unless they
            // spilled: the same-block range of a drained target is empty.
            assert_eq!(CoreWriterQueue::pending_count(&db, block).unwrap(), 0);
        }
    }
    assert!(id > 10_000, "enough rows to fill targets: {id}");
    eprintln!("max point reads per enqueue under T: {max_reads_t}");
    assert!(max_reads_t <= 12 + 8, "max point reads per enqueue: {max_reads_t}");
}

/// The precompile path: the EVM provider's entry with the header bits.
/// A lockbox deposit at block 100 queues for 100 with the T bit, for 101
/// without; a staking delegate likewise; an order keeps h + K either way.
#[test]
fn t_precompile_entry_follows_the_header_bit() {
    for (features, want) in [(0u128, 101u64), (Feature::SameBlockCoreWriter.bit(), 100)] {
        let (_dir, db) = setup();
        let trader = addr(7);
        let value = U256::from(5u128 * 10u128.pow(18));
        let mut deposit = alloy_primitives::keccak256("depositToNative(uint128)".as_bytes())[..4].to_vec();
        deposit.extend_from_slice(&value.to_be_bytes::<32>());
        let mut meter = ReadMeter::unlimited();
        execute_precompile_metered_at(
            &precompile_address(ADDR_LOCKBOX),
            &deposit,
            &trader,
            value,
            &db,
            100,
            0,
            features,
            false,
            &mut meter,
        )
        .unwrap();
        let mut delegate = alloy_primitives::keccak256("delegate(address,uint128)".as_bytes())[..4].to_vec();
        delegate.extend_from_slice(&[0u8; 12]);
        delegate.extend_from_slice(addr(9).as_slice());
        delegate.extend_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        execute_precompile_metered_at(
            &precompile_address(ADDR_CORE_WRITER_STAKING),
            &delegate,
            &trader,
            U256::ZERO,
            &db,
            100,
            0,
            features,
            false,
            &mut meter,
        )
        .unwrap();
        let mut cancel = alloy_primitives::keccak256("cancelOrder(bytes32)".as_bytes())[..4].to_vec();
        cancel.extend_from_slice(&U256::from(5u64).to_be_bytes::<32>());
        execute_precompile_metered_at(
            &precompile_address(ADDR_CORE_WRITER),
            &cancel,
            &trader,
            U256::ZERO,
            &db,
            100,
            0,
            features,
            false,
            &mut meter,
        )
        .unwrap();
        let keys: Vec<(u64, u64)> = db
            .iterate_cf(CF_CORE_WRITER_QUEUE, None)
            .unwrap()
            .into_iter()
            .map(|(k, _)| (target_of(&k), seq_of(&k)))
            .collect();
        let order_seq = if features == 0 { 0 } else { ORDER_BASE };
        assert_eq!(keys, vec![(want, 0), (want, 1), (110, order_seq)], "features {features:#x}");
    }
}

/// Point reads (`get_cf_raw` + `get_cf_len`, any CF) through a backend.
#[derive(Clone)]
struct PointReads<T> {
    inner: T,
    n: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<T> PointReads<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            n: Default::default(),
        }
    }
    fn count(&self) -> usize {
        self.n.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn bump(&self) {
        self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl<T: StateBackend> StateBackend for PointReads<T> {
    fn get_cf_raw(
        &self,
        cf: &str,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, torus_state::error::StateError> {
        self.bump();
        self.inner.get_cf_raw(cf, key)
    }
    fn get_cf_len(
        &self,
        cf: &str,
        key: &[u8],
    ) -> Result<Option<usize>, torus_state::error::StateError> {
        self.bump();
        self.inner.get_cf_len(cf, key)
    }
    fn put_cf_raw(
        &self,
        cf: &str,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), torus_state::error::StateError> {
        self.inner.put_cf_raw(cf, key, value)
    }
    fn delete_cf_raw(&self, cf: &str, key: &[u8]) -> Result<(), torus_state::error::StateError> {
        self.inner.delete_cf_raw(cf, key)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf(
        &self,
        cf: &str,
        prefix: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf(cf, prefix)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf_from(
        &self,
        cf: &str,
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf_from(cf, start, limit)
    }
    #[allow(clippy::type_complexity)]
    fn iterate_cf_prefix_from(
        &self,
        cf: &str,
        prefix: &[u8],
        start: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, torus_state::error::StateError> {
        self.inner.iterate_cf_prefix_from(cf, prefix, start, limit)
    }
    fn prefix_exists(
        &self,
        cf: &str,
        prefix: &[u8],
    ) -> Result<bool, torus_state::error::StateError> {
        self.inner.prefix_exists(cf, prefix)
    }
    fn atomic_write(
        &self,
        ops: &[torus_state::AtomicWriteOp<'_>],
    ) -> Result<(), torus_state::error::StateError> {
        self.inner.atomic_write(ops)
    }
}

/// Codex torus-adversarial on T (s138; the step 2 cursor finding, memory
/// ad5a8203c6de102f, again): non-order intake above the 200-row drain cap
/// (10M gas every two blocks: ~450 enqueues per two blocks) grows the
/// same-block backlog every block, and a walk from the queuing block that
/// reads every full target costs O(backlog) unmetered point reads per call.
/// Here 2,000 enqueues every second block (1,000 a block against a drain
/// of 200), 10% of them orders, step 2's rules before the activation `H` (its
/// backlog of full targets runs past `H`) and T's from `H`; txs of 1 to 3
/// calls in a block journal, a fifth of them reverted; each block drains
/// after its EVM part; every 7th block crashes once (its journal dropped)
/// and is replayed. Point reads per non-order T enqueue stay constant while
/// the backlog grows, every sampled row lands on the first target from its
/// block with room, and the state equals a run without crashes.
#[test]
fn t_sustained_same_block_overload_keeps_reads_per_enqueue_constant() {
    const FROM: u64 = 1_000;
    const H: u64 = 1_016;
    const TO: u64 = 1_060;
    struct Run {
        _dir: tempfile::TempDir,
        db: StateDb,
        /// Max point reads of a non-order T enqueue, per block.
        reads: Vec<(u64, usize)>,
        backlog_targets: u64,
    }
    let run = |crash: bool| -> Run {
        let (dir, db) = setup();
        let mut rng = 0xd1b5_4a32_d192_ed03u64;
        let mut id = 0u64;
        let mut reads = Vec::new();
        for block in FROM..TO {
            let t_on = block >= H;
            let attempts = if crash && block.is_multiple_of(7) { 2 } else { 1 };
            let (rng0, id0) = (rng, id);
            for attempt in 0..attempts {
                (rng, id) = (rng0, id0);
                let mut next = |n: u64| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    rng % n
                };
                let ov = NativeStateOverlay::new(db.clone());
                let mut max_reads = 0usize;
                let mut left: u64 = if block.is_multiple_of(2) { 2_000 } else { 0 };
                while left > 0 {
                    let calls = (1 + next(3)).min(left);
                    left -= calls;
                    let revert = next(5) == 0;
                    ov.begin_tx();
                    for _ in 0..calls {
                        id += 1;
                        let order = next(10) == 0;
                        let kind = if order {
                            QueuedActionKind::CancelOrder {
                                order_id: u128::from(id),
                            }
                        } else {
                            QueuedActionKind::LockboxWithdraw {
                                amount: FixedPoint::from_raw(id as i128),
                            }
                        };
                        let counted = PointReads::new(ov.clone());
                        let seq = CoreWriterQueue::enqueue_with(&counted, &qa(kind, block), t_on)
                            .unwrap();
                        if t_on && !order {
                            max_reads = max_reads.max(counted.count());
                            if id.is_multiple_of(97) {
                                // The first target from `block` with room.
                                let full = |t: u64| {
                                    ov.get_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(t, CAP - 1))
                                        .unwrap()
                                        .is_some()
                                };
                                let mut t = block;
                                while full(t) {
                                    t += 1;
                                }
                                let row_at = |t: u64| {
                                    ov.get_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(t, seq))
                                        .unwrap()
                                };
                                let mine = |t: u64| {
                                    row_at(t).is_some_and(|v| {
                                        matches!(
                                            borsh::from_slice::<QueuedAction>(&v).unwrap().kind,
                                            QueuedActionKind::LockboxWithdraw { amount }
                                                if amount.raw() == id as i128
                                        )
                                    })
                                };
                                // Either it is at `t` (room left) or it took
                                // the last slot of the target before `t`.
                                assert!(
                                    mine(t) || (seq == CAP - 1 && t > block && mine(t - 1)),
                                    "row {id} of {block} (seq {seq}): first open target {t}"
                                );
                            }
                        }
                    }
                    if revert {
                        ov.revert_tx();
                    } else {
                        ov.keep_tx();
                    }
                }
                CoreWriterQueue::drain_keyed(&ov, block).unwrap();
                if attempt + 1 < attempts {
                    drop(ov); // crash: the block's writes never land
                    continue;
                }
                ov.commit_tx(&db).unwrap();
                if t_on {
                    reads.push((block, max_reads));
                }
            }
        }
        let mut backlog_targets = 0;
        let mut t = TO;
        while db
            .get_cf_raw(CF_CORE_WRITER_QUEUE, &queue_key(t, CAP - 1))
            .unwrap()
            .is_some()
        {
            backlog_targets += 1;
            t += 1;
        }
        Run {
            _dir: dir,
            db,
            reads,
            backlog_targets,
        }
    };
    let crashed = run(true);
    let straight = run(false);
    for cf in [CF_CORE_WRITER_QUEUE, CF_FEE_CONFIG] {
        assert!(
            crashed.db.iterate_cf(cf, None).unwrap() == straight.db.iterate_cf(cf, None).unwrap(),
            "replay after crashes = the straight run ({cf})"
        );
    }
    let first = straight.reads.iter().map(|r| r.1).max().unwrap();
    let late = straight
        .reads
        .iter()
        .filter(|(b, _)| *b >= H + 20)
        .map(|r| r.1)
        .max()
        .unwrap();
    eprintln!(
        "backlog {} targets; max point reads per non-order T enqueue: all {first}, from H + 20 {late}",
        straight.backlog_targets
    );
    assert!(
        straight.backlog_targets >= 100,
        "the backlog grows: {} full targets after {TO}",
        straight.backlog_targets
    );
    // Constant: the T cursor, step 2's cursor, one probe; the walks right
    // after `H` may still cross step 2's full targets that no step 2 row
    // covers (below H - 1 + K, K = 10 here), one read each.
    // Before the fix (f7714a3c, 400 a block): 145 reads at 142 targets.
    assert!(
        late <= 3,
        "max point reads per enqueue after H + 20: {late}"
    );
    assert!(
        first <= 3 + 12,
        "max point reads per enqueue from H: {first}"
    );
}
