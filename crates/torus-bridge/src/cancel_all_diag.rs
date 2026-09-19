//! Default-off attribution of cancel-all work; no change to cancellation policy.
use std::time::Instant;

pub(super) fn enabled() -> bool {
    #[cfg(test)]
    if let Some(value) = tests::OVERRIDE.with(|v| v.get()) {
        return value;
    }
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| parse(std::env::var("TORUS_CANCEL_ALL_DIAG").ok().as_deref()))
}

fn parse(value: Option<&str>) -> bool {
    value == Some("1")
}

#[derive(Debug, Default)]
pub(super) struct Diagnostic {
    pub book_ns: u128,
    pub margin_ns: u128,
    pub balance_ns: u128,
    pub markets: u64,
    pub orders: u64,
    pub balance_read_errors: u64,
    pub balance_write_errors: u64,
}

impl Diagnostic {
    pub fn emit(self, started: Instant, height: u64, market: Option<torus_types::MarketId>) {
        let elapsed_ns = started.elapsed().as_nanos();
        let attributed = self.book_ns + self.margin_ns + self.balance_ns;
        tracing::info!(
            target: "torus_bridge::cancel_all_diag",
            schema = 1, block_height = height, market_id = ?market,
            elapsed_ns, book_ns = self.book_ns, margin_ns = self.margin_ns,
            balance_ns = self.balance_ns,
            residual_ns = elapsed_ns.saturating_sub(attributed),
            timing_valid = attributed <= elapsed_ns,
            markets = self.markets, orders = self.orders,
            balance_read_errors = self.balance_read_errors,
            balance_write_errors = self.balance_write_errors,
            "cancel-all attribution"
        );
        #[cfg(test)]
        tests::REPORTS.with(|r| r.borrow_mut().push(self));
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    thread_local! {
        pub static OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
        pub static REPORTS: RefCell<Vec<Diagnostic>> = const { RefCell::new(Vec::new()) };
    }
    pub fn capture<R>(value: bool, run: impl FnOnce() -> R) -> (R, Vec<Diagnostic>) {
        struct Restore(Option<bool>);
        impl Drop for Restore {
            fn drop(&mut self) {
                OVERRIDE.with(|v| v.set(self.0));
            }
        }
        let _restore = Restore(OVERRIDE.with(|v| v.replace(Some(value))));
        REPORTS.with(|r| r.borrow_mut().clear());
        let result = run();
        (
            result,
            REPORTS.with(|r| std::mem::take(&mut *r.borrow_mut())),
        )
    }
    #[test]
    fn cancel_diag_flag_exact() {
        assert!(parse(Some("1")));
        for value in [
            None,
            Some("0"),
            Some("true"),
            Some("01"),
            Some(" 1"),
            Some("1 "),
        ] {
            assert!(!parse(value));
        }
    }

    use super::super::{BookMode, NativeExecContext, NativeExecutor};
    use alloy_primitives::Address;
    use torus_core::{order_book::OrderBook, position::NativeBalance};
    use torus_state::{StateBackend, StateDb};
    use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

    fn fp(value: i128) -> FixedPoint {
        FixedPoint::from_raw(value * FixedPoint::SCALE)
    }
    fn fixture(clamp: bool, corrupt: bool) -> (tempfile::TempDir, NativeExecContext) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let mut ctx = NativeExecContext::new_with_mode(
            db,
            7,
            1000,
            0,
            100,
            4,
            Address::from([99; 20]),
            Address::from([100; 20]),
            Address::from([101; 20]),
            BookMode::LevelAuthorityChunked,
            None,
        );
        for who in [1, 2] {
            ctx.positions
                .put_native_balance(
                    &Address::from([who; 20]),
                    &NativeBalance {
                        available: fp(1_000_000),
                        order_margin: FixedPoint::ZERO,
                    },
                )
                .unwrap();
        }
        for mid in [1, 2] {
            let mut book = OrderBook::new(mid, FixedPoint::ONE, FixedPoint::ONE);
            book.set_level_hash_chunked(true);
            ctx.order_books.insert(mid, book);
            for who in [1, 2] {
                let actions: Vec<_> = (0..40)
                    .map(|_| {
                        (
                            Address::from([who; 20]),
                            NativeAction::PlaceOrder(PlaceOrderParams {
                                market_id: mid,
                                is_buy: true,
                                price: fp(100),
                                quantity: fp(1),
                                order_type: OrderType::Limit,
                                time_in_force: TimeInForce::GTC,
                                reduce_only: false,
                                client_order_id: None,
                            }),
                        )
                    })
                    .collect();
                let result = NativeExecutor::execute_batch_settle_mode(&mut ctx, &actions, false);
                assert!(result.results.iter().all(|r| r.success));
            }
        }
        if clamp {
            ctx.positions
                .put_native_balance(
                    &Address::from([1; 20]),
                    &NativeBalance {
                        available: fp(10),
                        order_margin: FixedPoint::from_raw(7),
                    },
                )
                .unwrap();
        }
        if corrupt {
            ctx.state
                .put_cf_raw(
                    torus_state::cf::CF_NATIVE_BALANCES,
                    Address::from([1; 20]).as_slice(),
                    &[255],
                )
                .unwrap();
        }
        for book in ctx.order_books.values_mut() {
            let _ = book.full_level_ops();
            let _ = book.take_row_ops();
            let _ = book.take_level_ops();
        }
        ctx.dirty_books.clear();
        (dir, ctx)
    }

    #[test]
    fn cancel_diag_on_off_preserves_state_journals_clamp_and_bad_balance() {
        for market in [Some(1), Some(999), None] {
            for (clamp, corrupt) in [(false, false), (true, false), (false, true)] {
                let (_a_dir, mut a) = fixture(clamp, corrupt);
                let (_b_dir, mut b) = fixture(clamp, corrupt);
                for repeat in 0..2 {
                    let action = NativeAction::CancelAllOrders { market_id: market };
                    let sender = Address::from([1; 20]);
                    let (off, off_reports) =
                        capture(false, || NativeExecutor::execute(&mut a, &sender, &action));
                    let (on, on_reports) =
                        capture(true, || NativeExecutor::execute(&mut b, &sender, &action));
                    assert!(off_reports.is_empty());
                    assert_eq!(format!("{off:?}"), format!("{on:?}"));
                    assert_eq!(on_reports.len(), 1);
                    let report = &on_reports[0];
                    let orders = if repeat > 0 || market == Some(999) {
                        0
                    } else if market.is_none() {
                        80
                    } else {
                        40
                    };
                    assert_eq!(report.orders, orders);
                    assert_eq!(
                        report.markets,
                        if market == Some(999) {
                            0
                        } else if market.is_none() {
                            2
                        } else {
                            1
                        }
                    );
                    assert_eq!(report.balance_read_errors, u64::from(corrupt && orders > 0));
                    assert_eq!(report.balance_write_errors, 0);
                    assert_eq!(a.dirty_books, b.dirty_books);
                    for cf in torus_state::cf::ALL_CF_NAMES {
                        assert_eq!(
                            a.state.iterate_cf(cf, None).unwrap(),
                            b.state.iterate_cf(cf, None).unwrap(),
                            "{cf}"
                        );
                    }
                    for mid in [1, 2] {
                        let left = a.order_books.get_mut(&mid).unwrap();
                        let right = b.order_books.get_mut(&mid).unwrap();
                        assert_eq!(left.full_row_ops(), right.full_row_ops());
                        assert_eq!(left.take_row_ops(), right.take_row_ops());
                        let encode = |book: &mut OrderBook| {
                            book.take_level_ops()
                                .into_iter()
                                .map(|(key, row)| (key, row.map(|r| r.encode().to_vec())))
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(encode(left), encode(right));
                        let full = |book: &mut OrderBook| {
                            book.full_level_ops()
                                .into_iter()
                                .map(|(key, row)| (key, row.encode().to_vec()))
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(full(left), full(right));
                    }
                }
            }
        }
    }
}
