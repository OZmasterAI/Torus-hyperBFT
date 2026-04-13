//! Shared test harness for integration tests.

use std::net::SocketAddr;
use std::sync::Arc;

use alloy_primitives::Address;
use borsh::BorshSerialize;
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use revm::state::AccountInfo;
use tempfile::TempDir;

use torus_bridge::native_executor::NativeExecContext;
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::position::{NativeBalance, PositionManager};
use torus_core::precompiles::{OrderBookSnapshot, PriceLevel};
use torus_evm::{EvmExecutor, TORUS_CHAIN_ID};
use torus_mempool::{Mempool, MempoolConfig};
use torus_rpc::{BlockNotifier, RpcServer};
use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORACLE, CF_NATIVE_ORDER_BOOKS};
use torus_state::StateDb;
use torus_types::{Address as TAddress, FixedPoint, MarketId, U256};

/// Central test harness holding a temporary DB and all managers.
pub struct TestHarness {
    pub _tmp_dir: TempDir,
    pub state_db: StateDb,
    pub positions: PositionManager,
    pub executor: Arc<EvmExecutor>,
    pub mempool: Arc<Mempool>,
}

impl TestHarness {
    pub fn new() -> Self {
        let tmp_dir = TempDir::new().expect("create temp dir");
        let state_db = StateDb::open(tmp_dir.path()).expect("open StateDb");
        let positions = PositionManager::new(state_db.clone());
        let executor = Arc::new(EvmExecutor::new(TORUS_CHAIN_ID));
        let mempool = Arc::new(Mempool::new(state_db.clone(), MempoolConfig::default()));

        Self {
            _tmp_dir: tmp_dir,
            state_db,
            positions,
            executor,
            mempool,
        }
    }

    /// Fund a trader's native balance (perp-side).
    pub fn fund_native(&self, trader: &Address, amount: FixedPoint) {
        let mut bal = self.positions.get_native_balance(trader).unwrap();
        bal.available = bal.available + amount;
        self.positions.put_native_balance(trader, &bal).unwrap();
    }

    /// Fund a trader's EVM balance.
    pub fn fund_evm(&self, trader: &Address, amount: U256) {
        let existing = self.state_db.get_account(trader).unwrap();
        let mut info = existing.unwrap_or(AccountInfo {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: torus_state::db::KECCAK_EMPTY,
            code: None,
            account_id: None,
        });
        info.balance = info.balance + amount;
        self.state_db.put_account(trader, &info).unwrap();
    }

    /// Create a NativeExecContext for block-level native action execution.
    pub fn exec_context(&self, block_height: u64) -> NativeExecContext {
        NativeExecContext::new(
            self.state_db.clone(),
            block_height,
            1_700_000_000 + block_height,
            0,   // epoch
            100, // epoch_length
            100, // max_validators
            Address::ZERO,
            Address::ZERO,
            Address::ZERO,
        )
    }

    /// Register a market in CF_NATIVE_MARKETS (borsh: String + String + i128 + i128 + i128).
    pub fn register_market(&self, market_id: MarketId, base: &str, quote: &str) {
        let mut data = Vec::new();
        base.to_string().serialize(&mut data).unwrap();
        quote.to_string().serialize(&mut data).unwrap();
        // lot_size = 1.0
        (FixedPoint::ONE.raw()).serialize(&mut data).unwrap();
        // tick_size = 0.01
        (FixedPoint::from_raw(1_000_000).raw()).serialize(&mut data).unwrap();
        // initial_margin = 5.0
        (FixedPoint::from_raw(5 * FixedPoint::SCALE).raw()).serialize(&mut data).unwrap();

        self.state_db
            .put_cf_raw(CF_NATIVE_MARKETS, &market_id.to_be_bytes(), &data)
            .unwrap();
    }

    /// Persist an OrderBookSnapshot to CF_NATIVE_ORDER_BOOKS.
    pub fn persist_order_book(&self, market_id: MarketId, snapshot: &OrderBookSnapshot) {
        let data = borsh::to_vec(snapshot).unwrap();
        self.state_db
            .put_cf_raw(CF_NATIVE_ORDER_BOOKS, &market_id.to_be_bytes(), &data)
            .unwrap();
    }

    /// Write an aggregated oracle price directly to CF_NATIVE_ORACLE.
    pub fn set_oracle_price(&self, market_id: MarketId, price: FixedPoint, block_number: u64) {
        let mut key = Vec::with_capacity(11);
        key.extend_from_slice(b"agg");
        key.extend_from_slice(&market_id.to_be_bytes());

        // Format: price(i128 16 BE) + block_number(u64 8 BE) + timestamp(u64 8 BE) + count(u32 4 BE)
        let mut value = Vec::with_capacity(36);
        value.extend_from_slice(&price.raw().to_be_bytes());
        value.extend_from_slice(&block_number.to_be_bytes());
        value.extend_from_slice(&(1_700_000_000u64 + block_number).to_be_bytes());
        value.extend_from_slice(&1u32.to_be_bytes());

        self.state_db
            .put_cf_raw(CF_NATIVE_ORACLE, &key, &value)
            .unwrap();
    }

    /// Start an RPC server on a random port and return the handle + HTTP client.
    pub async fn start_rpc(&self) -> (jsonrpsee::server::ServerHandle, HttpClient) {
        let notifier = BlockNotifier::new();
        let server = RpcServer::new(
            self.state_db.clone(),
            self.mempool.clone(),
            self.executor.clone(),
            TORUS_CHAIN_ID,
            notifier,
        );
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (handle, local_addr) = server.start(addr).await.unwrap();
        let client = HttpClientBuilder::default()
            .build(&format!("http://{local_addr}"))
            .unwrap();
        (handle, client)
    }

    /// Create a deterministic test address from a seed byte.
    pub fn addr(seed: u8) -> Address {
        Address::new([seed; 20])
    }

    /// FixedPoint from whole number (e.g. `fp(100)` = 100.00000000).
    pub fn fp(whole: i64) -> FixedPoint {
        FixedPoint::from_raw(whole as i128 * FixedPoint::SCALE)
    }

    /// FixedPoint from a decimal ratio: `fp_dec(15, 1)` = 1.5.
    pub fn fp_dec(num: i64, decimal_places: u32) -> FixedPoint {
        let divisor = 10i128.pow(decimal_places);
        FixedPoint::from_raw(num as i128 * FixedPoint::SCALE / divisor)
    }

    /// Build an OrderBookSnapshot from bid/ask price levels.
    pub fn snapshot(
        bids: &[(FixedPoint, FixedPoint)],
        asks: &[(FixedPoint, FixedPoint)],
    ) -> OrderBookSnapshot {
        OrderBookSnapshot {
            bids: bids
                .iter()
                .map(|&(price, quantity)| PriceLevel { price, quantity })
                .collect(),
            asks: asks
                .iter()
                .map(|&(price, quantity)| PriceLevel { price, quantity })
                .collect(),
        }
    }
}
