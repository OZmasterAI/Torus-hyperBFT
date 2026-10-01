//! F10: a feeder cycle against a REAL `RpcServer` + `Mempool` on a temp
//! `StateDb` holding an Active validator V and its registered hot signer S.
//! Venues are the recorded fixtures (`FakeHttp`); nothing touches the network.

use std::sync::Arc;

use alloy_primitives::{Address, U256};
use borsh::BorshSerialize;
use k256::ecdsa::SigningKey;
use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
use torus_mempool::{Mempool, MempoolConfig};
use torus_price_feeder::config::{Config, Exchange};
use torus_price_feeder::feeder::{CycleOutcome, Feeder};
use torus_price_feeder::fetch::SystemClock;
use torus_price_feeder::node::{startup_check, RpcNode};
use torus_price_feeder::price::parse_price;
use torus_price_feeder::testing::FakeHttp;
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::cf::{oracle_signer_key, CF_NATIVE_MARKETS, CF_NATIVE_ORACLE};
use torus_state::StateDb;
use torus_types::{FixedPoint, NativeAction};

const V: Address = Address::new([0x11; 20]);

fn signer_key() -> SigningKey {
    SigningKey::from_slice(&[21u8; 32]).unwrap()
}

fn signer() -> Address {
    torus_wallet::keystore::address_from_key(&signer_key())
}

/// The node's market layout (torus-rpc `store_market`, lib.rs tests).
fn store_market(state: &StateDb, market_id: u64, base: &str) {
    let mut data = Vec::new();
    BorshSerialize::serialize(&base.to_string(), &mut data).unwrap();
    BorshSerialize::serialize(&"USD".to_string(), &mut data).unwrap();
    for raw in [FixedPoint::SCALE, FixedPoint::SCALE / 100, FixedPoint::SCALE / 10] {
        BorshSerialize::serialize(&raw, &mut data).unwrap();
    }
    state.put_cf_raw(CF_NATIVE_MARKETS, &market_id.to_be_bytes(), &data).unwrap();
}

/// V Active; `register` = V.oracle_signer = S plus the "sgn" index entry.
fn chain(register: bool) -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().unwrap();
    let state = StateDb::open(dir.path()).unwrap();
    store_market(&state, 1, "BTC");
    store_market(&state, 2, "ETH");
    StakingManager::new(state.clone())
        .put_validator(
            &V,
            &ValidatorState {
                address: V,
                pubkey: [7; 32],
                commission_bps: 0,
                self_stake: MIN_SELF_DELEGATION,
                total_delegated: U256::ZERO,
                status: ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
                oracle_signer: register.then_some(signer()),
            },
        )
        .unwrap();
    if register {
        state.put_cf_raw(CF_NATIVE_ORACLE, &oracle_signer_key(&signer()), V.as_slice()).unwrap();
    }
    (dir, state)
}

fn cfg(rpc_url: &str) -> Config {
    let mut t = format!(
        "rpc_url = \"{rpc_url}\"\nsigner_key_file = \"/unused\"\nvalidator_address = \"{V:#x}\"\n"
    );
    for ex in Exchange::ALL {
        t.push_str(&format!("[exchanges.{0}]\nbase_url = \"fake://{0}\"\n", ex.name()));
    }
    t.push_str(
        "[[markets]]\nmarket_id = 1\nbase_asset = \"BTC\"\nsymbols = { binance = \"BTCUSDT\", okx = \"BTC-USDT\", bybit = \"BTCUSDT\", kraken = \"XXBTZUSD\", kucoin = \"BTC-USDT\", gate = \"BTC_USDT\", mexc = \"BTCUSDT\" }\n\
         [[markets]]\nmarket_id = 2\nbase_asset = \"ETH\"\nsymbols = { binance = \"ETHUSDT\", okx = \"ETH-USDT\", bybit = \"ETHUSDT\", kraken = \"XETHZUSD\", kucoin = \"ETH-USDT\", gate = \"ETH_USDT\", mexc = \"ETHUSDT\" }\n",
    );
    Config::parse(&t).unwrap()
}

async fn start(state: &StateDb, mcfg: MempoolConfig) -> (Arc<Mempool>, jsonrpsee::server::ServerHandle, String) {
    let mempool = Arc::new(Mempool::new(state.clone(), mcfg));
    let executor = Arc::new(torus_evm::EvmExecutor::new(torus_types::eip712::TORUS_CHAIN_ID));
    let server = RpcServer::new(
        state.clone(),
        mempool.clone(),
        executor,
        torus_types::eip712::TORUS_CHAIN_ID,
        100,
        BlockNotifier::new(),
    );
    let (handle, addr) = server.start("127.0.0.1:0".parse().unwrap()).await.unwrap();
    (mempool, handle, format!("http://{addr}"))
}

async fn one_cycle_admits_signer_submission(mcfg: MempoolConfig) {
    let (_d, state) = chain(true);
    let (mempool, handle, url) = start(&state, mcfg).await;
    let node = RpcNode::new(&url).unwrap();
    let c = cfg(&url);
    startup_check(&c, &node, signer()).await.expect("registered signer passes the startup check");

    let mut f = Feeder::new(c, Arc::new(FakeHttp::with_fixtures()), node, SystemClock, signer_key());
    let r = f.run_cycle().await;
    assert_eq!(r.outcome, CycleOutcome::Submitted { ok: 1, failed: 0 }, "{r:?}");
    assert_eq!(mempool.native_pool_size(), 1);
    let pooled = mempool.drain_native(10);
    assert_eq!(pooled.len(), 1);
    assert_eq!(pooled[0].recover_sender().unwrap(), signer(), "sender is the hot signer S");
    match &pooled[0].action {
        NativeAction::SubmitOraclePrices(sub) => assert_eq!(
            sub.prices,
            vec![(1, parse_price("83497.685").unwrap()), (2, parse_price("2683.325").unwrap())]
        ),
        other => panic!("{other:?}"),
    }
    handle.stop().unwrap();
}

#[tokio::test]
async fn feeder_cycle_is_admitted_with_the_signer_as_sender() {
    one_cycle_admits_signer_submission(MempoolConfig::default()).await;
}

/// Backlogged mempool (admission limit 0): oracle submissions from a
/// registered signer still pass the RPC busy screen.
#[tokio::test]
async fn feeder_cycle_is_admitted_under_backlog() {
    let mcfg = MempoolConfig { native_admission_horizon_ms: 1, native_admission_floor: 0, ..Default::default() };
    one_cycle_admits_signer_submission(mcfg).await;
}

#[tokio::test]
async fn unregistered_signer_fails_the_startup_check_and_sends_nothing() {
    let (_d, state) = chain(false);
    let (mempool, handle, url) = start(&state, MempoolConfig::default()).await;
    let node = RpcNode::new(&url).unwrap();
    let e = startup_check(&cfg(&url), &node, signer()).await.unwrap_err();
    assert!(e.contains("set-oracle-signer --signer"), "{e}");
    let mut f = Feeder::new(cfg(&url), Arc::new(FakeHttp::with_fixtures()), node, SystemClock, signer_key());
    assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Idle(_)));
    assert_eq!(mempool.native_pool_size(), 0);
    handle.stop().unwrap();
}
