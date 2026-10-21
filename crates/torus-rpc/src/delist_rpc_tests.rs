//! Delist follow-up (node-local, no consensus change): `torus_getMarkets`
//! reports each market's governance delist and its settlement: the delist
//! height (flag row value), the grace end (`close_at` of the settlement row),
//! the settlement status (`none` / `grace` / `settling` / `settled`), the
//! fixed close price once the settlement step chose it (and, with the grace
//! end, after the walk from the kept record `0x0A`, owner s134), a governance close
//! price waiting to be consumed, and the height the settlement completed
//! (oracle-closed marker).

use std::sync::Arc;

use tempfile::TempDir;
use torus_core::liquidation::{put_delist_settled, put_delist_settlement, DelistSettled, DelistSettlement};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_state::cf::{CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS};
use torus_state::StateDb;
use torus_types::{FixedPoint, MarketId};

use crate::torus::TorusApiServer;
use crate::types::{dec_fp, hex_u64, RpcMarketInfo};
use crate::{BlockNotifier, RpcServer};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn list(state: &StateDb, mid: MarketId) {
    use borsh::BorshSerialize;
    let mut row = Vec::new();
    "BTC".to_string().serialize(&mut row).unwrap();
    "USD".to_string().serialize(&mut row).unwrap();
    FixedPoint::SCALE.serialize(&mut row).unwrap();
    FixedPoint::SCALE.serialize(&mut row).unwrap();
    (5 * FixedPoint::SCALE).serialize(&mut row).unwrap();
    state.put_cf_raw(CF_NATIVE_MARKETS, &mid.to_be_bytes(), &row).unwrap();
}

fn delist(state: &StateDb, mid: MarketId, height: u64) {
    state
        .put_cf_raw(CF_NATIVE_MARKETS, &torus_types::market_delisted_key(mid), &height.to_be_bytes())
        .unwrap();
}

fn row(state: &StateDb, mid: MarketId, close_at: u64, price: Option<FixedPoint>) {
    put_delist_settlement(state, &DelistSettlement { market: mid, close_at, price, cursor: None }).unwrap();
}

async fn markets_at(state: &StateDb, latest: u64) -> Vec<RpcMarketInfo> {
    let s = RpcServer::new(
        state.clone(),
        Arc::new(Mempool::new(state.clone(), MempoolConfig::default())),
        Arc::new(EvmExecutor::new(TORUS_CHAIN_ID)),
        TORUS_CHAIN_ID,
        100,
        BlockNotifier::new(),
    );
    crate::set_latest_height(s.state(), latest);
    s.state().get_markets(None, None, None).await.unwrap()
}

#[tokio::test]
async fn get_markets_reports_delist_and_settlement() {
    let dir = TempDir::new().unwrap();
    let state = StateDb::open(dir.path()).unwrap();
    for m in 1..=6 {
        list(&state, m);
    }
    // 2: in the grace period (close_at 200 > tip 150), a governance close
    // price waiting.
    delist(&state, 2, 100);
    row(&state, 2, 200, None);
    state
        .put_cf_raw(CF_NATIVE_LIQUIDATION, &torus_types::delist_close_price_key(2), &fp(42).raw().to_be_bytes())
        .unwrap();
    // 3: past close_at, no price yet (the step waits): settling.
    delist(&state, 3, 90);
    row(&state, 3, 120, None);
    // 4: settling at a fixed price.
    delist(&state, 4, 80);
    row(&state, 4, 110, Some(fp(50)));
    // 5: settled (no row, oracle-closed marker at 140).
    delist(&state, 5, 70);
    state
        .put_cf_raw(CF_NATIVE_MARKETS, &torus_types::market_oracle_closed_key(5), &140u64.to_be_bytes())
        .unwrap();
    // 5 keeps its settlement record (owner s134): close_at 120, price 77,
    // walk ended at 140.
    put_delist_settled(&state, &DelistSettled { market: 5, close_at: 120, price: fp(77), settled_height: 140 })
        .unwrap();
    // 6: walk done, ADL obligations still draining (no row, no marker; the
    // kept record exists from the walk's end).
    delist(&state, 6, 60);
    put_delist_settled(&state, &DelistSettled { market: 6, close_at: 100, price: fp(66), settled_height: 130 })
        .unwrap();

    let ms = markets_at(&state, 150).await;
    assert_eq!(ms.len(), 6, "flag / marker rows are not markets");
    let by = |m: u64| ms.iter().find(|x| x.market_id == hex_u64(m)).unwrap().clone();

    let m1 = by(1);
    assert_eq!(m1.status, "active");
    assert!(!m1.delisted);
    assert_eq!(m1.settlement, "none");
    assert_eq!((m1.delist_height, m1.close_at, m1.close_price), (None, None, None));

    let m2 = by(2);
    assert_eq!(m2.status, "delisted");
    assert!(m2.delisted);
    assert_eq!(m2.delist_height.as_deref(), Some(hex_u64(100).as_str()));
    assert_eq!(m2.close_at.as_deref(), Some(hex_u64(200).as_str()));
    assert_eq!(m2.settlement, "grace");
    assert_eq!(m2.close_price, None);
    assert_eq!(m2.governance_close_price, Some(dec_fp(fp(42))));

    let m3 = by(3);
    assert_eq!(m3.settlement, "settling", "past close_at, waiting for a price");
    assert_eq!(m3.close_at.as_deref(), Some(hex_u64(120).as_str()));
    assert_eq!(m3.close_price, None);

    let m4 = by(4);
    assert_eq!(m4.settlement, "settling");
    assert_eq!(m4.close_price, Some(dec_fp(fp(50))));

    let m5 = by(5);
    assert_eq!(m5.settlement, "settled");
    assert_eq!(m5.settled_height.as_deref(), Some(hex_u64(140).as_str()));
    assert_eq!(m5.delist_height.as_deref(), Some(hex_u64(70).as_str()));
    assert_eq!(m5.close_price, Some(dec_fp(fp(77))), "kept after the walk");
    assert_eq!(m5.close_at.as_deref(), Some(hex_u64(120).as_str()), "kept after the walk");

    let m6 = by(6);
    assert_eq!(m6.settlement, "settling", "ADL drain pending");
    assert_eq!(m6.settled_height, None);
    assert_eq!(m6.close_price, Some(dec_fp(fp(66))), "fixed price kept while the ADL drains");
    assert_eq!(m6.close_at.as_deref(), Some(hex_u64(100).as_str()));

    // At the grace end the step runs: no longer "grace".
    assert_eq!(markets_at(&state, 200).await.iter().find(|x| x.market_id == hex_u64(2)).unwrap().settlement, "settling");
}

/// Old clients: the new fields are additive, a reply without them decodes.
#[test]
fn market_info_without_delist_fields_decodes() {
    let v = serde_json::json!({"marketId": "0x1", "baseAsset": "BTC", "quoteAsset": "USD",
        "lotSize": "1", "tickSize": "1", "status": "active"});
    let m: RpcMarketInfo = serde_json::from_value(v).unwrap();
    assert!(!m.delisted);
}
