//! Item 7 step 2 B16 (owner decision 3): the `cf_native_bbo` row per dirty
//! market. After every block the CF holds (best bid raw, best ask raw) of each
//! market whose book changed, in every book mode and through both save paths
//! (`save_order_books`, and `save_order_books_deferred` under modes 2/3); an
//! emptied book deletes its row. The node-local `saved_bbo` skips the put when
//! the top of the book did not move. The CF is derived from the hashed book
//! rows: neither hashed nor in the native root.
//!
//! Fixtures as in `deferred_save_tests.rs` / `resident_books_tests.rs`: one
//! resident-books chain, each block on a fresh overlay that is frozen and
//! flushed (with the deferred payload's sidecar) before the next one.

#[path = "common/counting_backend.rs"]
mod counting_backend;

use alloy_primitives::Address;
use counting_backend::CountingBackend;
use torus_bridge::native_executor::{
    apply_deferred_book_save, BookMode, NativeExecContext, NativeExecutor, ResidentBooks,
};
use torus_core::position::NativeBalance;
use torus_state::cf::{CF_CONSENSUS_META, CF_NATIVE_BBO, META_NATIVE_APPLIED_HEIGHT};
use torus_state::native_trie::{build_native_trie_to_cf, NativeMemberCache, NativeTrieCache};
use torus_state::{NativeStateOverlay, StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const MARKET: MarketId = 1;

const ALL_MODES: [BookMode; 4] = [
    BookMode::Classic,
    BookMode::OrderRows,
    BookMode::LevelAuthority,
    BookMode::LevelAuthorityChunked,
];

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn gtc(is_buy: bool, price: i64) -> NativeAction {
    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id: MARKET,
        is_buy,
        price: fp(price),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// The expected row: bid raw ‖ ask raw (i128 BE), 0 for an empty side.
fn row(bid: i64, ask: i64) -> Vec<u8> {
    let raw = |p: i64| if p == 0 { 0i128 } else { fp(p).raw() };
    let mut r = raw(bid).to_be_bytes().to_vec();
    r.extend_from_slice(&raw(ask).to_be_bytes());
    r
}

/// One resident-books chain in `mode`; `deferred` saves through
/// `save_order_books_deferred` and applies the payload into the flush's
/// sidecar, as the exec pipeline's fast path does.
struct Chain {
    _dir: tempfile::TempDir,
    db: StateDb,
    books: ResidentBooks,
    mode: BookMode,
    deferred: bool,
    height: u64,
}

impl Chain {
    fn new(mode: BookMode, deferred: bool) -> Self {
        torus_state::native_trie::force_native_trie_maintenance_on_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        // Pre-testnet P0: orders need a listed market (placeholder rows,
        // tick / lot 1, as the old implicit book).
        for m in 1..=64u64 {
            db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
        }
        {
            let ctx0 = NativeExecContext::new_with_mode(
                db.clone(),
                0,
                1000,
                0,
                100,
                10,
                addr(99),
                addr(100),
                addr(101),
                mode,
                None,
            );
            for t in [addr(1), addr(2)] {
                let bal = NativeBalance {
                    available: fp(100_000_000),
                    order_margin: FixedPoint::ZERO,
                };
                ctx0.positions.put_native_balance(&t, &bal).unwrap();
            }
        }
        build_native_trie_to_cf(&db).unwrap();
        Self {
            _dir: dir,
            db,
            books: ResidentBooks::default(),
            mode,
            deferred,
            height: 0,
        }
    }

    /// Execute one block, save, flush. Returns the `cf_native_bbo` writes
    /// (puts + deletes) the block made.
    fn block(&mut self, actions: &[(Address, NativeAction)]) -> usize {
        self.height += 1;
        let height = self.height;
        let (mode, deferred) = (self.mode, self.deferred);
        let what = format!("{mode:?} deferred={deferred} h{height}");
        let overlay = NativeStateOverlay::new(self.db.clone());
        let state = CountingBackend::new(overlay.clone());
        let mut ctx = NativeExecContext::new_with_mode(
            state.clone(),
            height,
            1000 + height,
            0,
            100,
            10,
            addr(99),
            addr(100),
            addr(101),
            mode,
            Some(&mut self.books),
        );
        assert!(
            ctx.fatal_error.is_none(),
            "{what}: load {:?}",
            ctx.fatal_error
        );
        if height > 1 {
            assert!(ctx.resident_reused(), "{what}: resident books reused");
        }
        let r = NativeExecutor::execute_batch(&mut ctx, actions);
        for (i, res) in r.results.iter().enumerate() {
            assert!(res.success, "{what}: action {i}: {:?}", res.error);
        }
        let save = if deferred {
            Some(ctx.save_order_books_deferred().expect("modes 2/3 defer"))
        } else {
            ctx.save_order_books();
            None
        };
        assert!(
            ctx.fatal_error.is_none(),
            "{what}: save {:?}",
            ctx.fatal_error
        );
        ctx.stash_resident(&mut self.books);
        drop(ctx);
        let bbo_writes = state.bbo_writes();
        StateBackend::put_cf_raw(
            &overlay,
            CF_CONSENSUS_META,
            META_NATIVE_APPLIED_HEIGHT,
            &height.to_be_bytes(),
        )
        .unwrap();
        let frozen = overlay.freeze(height);
        let sidecar = save.map(|save| {
            let side = NativeStateOverlay::new(self.db.clone());
            apply_deferred_book_save(&side, None, save).expect("deferred book save");
            side.freeze(height)
        });
        frozen
            .flush_with_sidecar_native_trie_stats(
                sidecar.as_deref(),
                &self.db,
                Some(height),
                None::<&mut NativeTrieCache>,
                None::<&mut NativeMemberCache>,
            )
            .expect("flush");
        bbo_writes
    }

    fn bbo(&self) -> Option<Vec<u8>> {
        StateBackend::get_cf_raw(&self.db, CF_NATIVE_BBO, &MARKET.to_be_bytes()).unwrap()
    }
}

/// Every mode and save path: bid 99 + ask 101 -> (99, 101); the ask
/// cancelled -> (99, 0); the bid cancelled -> no row.
#[test]
fn dirty_market_bbo_row_follows_the_book_in_every_mode() {
    let mut legs: Vec<(BookMode, bool)> = ALL_MODES.iter().map(|m| (*m, false)).collect();
    legs.push((BookMode::LevelAuthority, true));
    legs.push((BookMode::LevelAuthorityChunked, true));
    for (mode, deferred) in legs {
        let what = format!("{mode:?} deferred={deferred}");
        let mut c = Chain::new(mode, deferred);
        assert_eq!(c.bbo(), None, "{what}: no row before the first order");

        // Global order ids: the bid is 1, the ask 2.
        c.block(&[(addr(1), gtc(true, 99)), (addr(2), gtc(false, 101))]);
        assert_eq!(c.bbo(), Some(row(99, 101)), "{what}: both sides");

        c.block(&[(addr(2), NativeAction::CancelOrder { order_id: 2 })]);
        assert_eq!(c.bbo(), Some(row(99, 0)), "{what}: the ask side emptied");

        c.block(&[(addr(1), NativeAction::CancelOrder { order_id: 1 })]);
        assert_eq!(c.bbo(), None, "{what}: an empty book has no row");
    }
}

/// A bid below the best leaves the top of the book where it was: the book is
/// dirty, but its bbo row is not written again. A bid above the best moves it
/// (one put).
#[test]
fn unchanged_top_of_book_writes_no_bbo_row() {
    let mut legs: Vec<(BookMode, bool)> = ALL_MODES.iter().map(|m| (*m, false)).collect();
    legs.push((BookMode::LevelAuthority, true));
    legs.push((BookMode::LevelAuthorityChunked, true));
    for (mode, deferred) in legs {
        let what = format!("{mode:?} deferred={deferred}");
        let mut c = Chain::new(mode, deferred);
        let n = c.block(&[(addr(1), gtc(true, 99)), (addr(2), gtc(false, 101))]);
        assert_eq!(n, 1, "{what}: the first save writes the row");

        let n = c.block(&[(addr(1), gtc(true, 98))]);
        assert_eq!(n, 0, "{what}: top of book unchanged, no bbo put");
        assert_eq!(c.bbo(), Some(row(99, 101)), "{what}");

        let n = c.block(&[(addr(1), gtc(true, 100))]);
        assert_eq!(n, 1, "{what}: a new best bid writes the row");
        assert_eq!(c.bbo(), Some(row(100, 101)), "{what}");
    }
}

/// The bbo CF is derived state: not in the running state hash, not in the
/// native root, registered at open.
#[test]
fn bbo_cf_is_neither_hashed_nor_in_the_native_root() {
    assert_eq!(torus_state::running_hash::hashed_cf_id(CF_NATIVE_BBO), None);
    assert_eq!(torus_state::native_trie::cf_tag(CF_NATIVE_BBO), None);
    assert!(torus_state::cf::ALL_CF_NAMES.contains(&CF_NATIVE_BBO));
    assert_eq!(CF_NATIVE_BBO, "cf_native_bbo");
}
