//! Per-user open-order limit: the limit function and the `cvlm` volume row.

use torus_core::position::{
    cum_volume_key, open_order_limit, NativeBalance, PositionManager, OPEN_ORDER_MAX_LIMIT,
};
use torus_state::cf::CF_NATIVE_BALANCES;
use torus_state::StateDb;
use torus_types::{Address, FixedPoint};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

#[test]
fn open_order_limit_grows_one_per_5m_and_caps_at_5000() {
    assert_eq!(open_order_limit(FixedPoint::ZERO), 1000);
    assert_eq!(open_order_limit(fp(4_999_999)), 1000);
    assert_eq!(
        open_order_limit(fp(5_000_000) - FixedPoint::from_raw(1)),
        1000
    );
    assert_eq!(open_order_limit(fp(5_000_000)), 1001);
    assert_eq!(open_order_limit(fp(20_000_000_000)), 5000);
    assert_eq!(open_order_limit(FixedPoint::from_raw(i128::MAX)), 5000);
    assert_eq!(OPEN_ORDER_MAX_LIMIT, 5000);
}

#[test]
fn cum_volume_row_round_trips_and_missing_reads_zero() {
    let (_dir, pm) = setup();
    let a = Address::new([7; 20]);
    assert_eq!(pm.get_cum_volume(&a).unwrap(), FixedPoint::ZERO);

    let v = fp(60_000) + FixedPoint::from_raw(3);
    pm.put_cum_volume(&a, v).unwrap();
    assert_eq!(pm.get_cum_volume(&a).unwrap(), v);

    // Own 24-byte key (`cvlm` ‖ address), raw 16-byte BE value; the
    // trader's NativeBalance row is untouched.
    let key = cum_volume_key(&a);
    assert_eq!(&key[..4], b"cvlm");
    assert_eq!(&key[4..], a.as_slice());
    let raw = pm.state().get_cf_raw(CF_NATIVE_BALANCES, &key).unwrap();
    assert_eq!(raw, Some(v.raw().to_be_bytes().to_vec()));
    assert!(pm
        .state()
        .get_cf_raw(CF_NATIVE_BALANCES, a.as_slice())
        .unwrap()
        .is_none());
    let bal = pm.get_native_balance(&a).unwrap();
    assert_eq!(bal.available, NativeBalance::default().available);
}
