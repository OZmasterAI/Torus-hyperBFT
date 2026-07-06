use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS indexer_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS blocks (
    height              INTEGER PRIMARY KEY,
    hash                TEXT NOT NULL UNIQUE,
    parent_hash         TEXT NOT NULL,
    timestamp           INTEGER NOT NULL,
    proposer            TEXT NOT NULL,
    gas_used            INTEGER NOT NULL,
    gas_limit           INTEGER NOT NULL,
    base_fee            INTEGER NOT NULL,
    tx_count            INTEGER NOT NULL,
    native_action_count INTEGER NOT NULL,
    epoch               INTEGER NOT NULL,
    validator_set_hash  TEXT NOT NULL,
    state_root          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS transactions (
    hash             TEXT PRIMARY KEY,
    block_height     INTEGER NOT NULL REFERENCES blocks(height),
    tx_index         INTEGER NOT NULL,
    from_addr        TEXT NOT NULL,
    to_addr          TEXT,
    value            TEXT NOT NULL,
    gas_limit        INTEGER NOT NULL,
    gas_used         INTEGER NOT NULL,
    gas_price        TEXT NOT NULL,
    input_data       TEXT NOT NULL,
    nonce            INTEGER NOT NULL,
    status           INTEGER NOT NULL,
    contract_address TEXT,
    tx_type          INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS logs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    block_height INTEGER NOT NULL REFERENCES blocks(height),
    tx_hash      TEXT NOT NULL,
    log_index    INTEGER NOT NULL,
    address      TEXT NOT NULL,
    topic0       TEXT,
    topic1       TEXT,
    topic2       TEXT,
    topic3       TEXT,
    data         TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS native_actions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    block_height INTEGER NOT NULL REFERENCES blocks(height),
    action_index INTEGER NOT NULL,
    action_type  TEXT NOT NULL,
    market_id    INTEGER,
    order_id     TEXT,
    validator    TEXT,
    target       TEXT,
    amount       TEXT,
    proposal_id  INTEGER,
    payload      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS validator_snapshots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    block_height   INTEGER NOT NULL,
    address        TEXT NOT NULL,
    pubkey         TEXT NOT NULL,
    power          INTEGER NOT NULL,
    commission_bps INTEGER NOT NULL,
    status         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_blocks_proposer  ON blocks(proposer);
CREATE INDEX IF NOT EXISTS idx_blocks_epoch     ON blocks(epoch);
CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks(timestamp);
CREATE INDEX IF NOT EXISTS idx_blocks_hash      ON blocks(hash);

CREATE INDEX IF NOT EXISTS idx_tx_from  ON transactions(from_addr);
CREATE INDEX IF NOT EXISTS idx_tx_to    ON transactions(to_addr);
CREATE INDEX IF NOT EXISTS idx_tx_block ON transactions(block_height);

CREATE INDEX IF NOT EXISTS idx_logs_address ON logs(address);
CREATE INDEX IF NOT EXISTS idx_logs_topic0  ON logs(topic0);
CREATE INDEX IF NOT EXISTS idx_logs_block   ON logs(block_height);
CREATE INDEX IF NOT EXISTS idx_logs_tx      ON logs(tx_hash);

CREATE INDEX IF NOT EXISTS idx_na_type      ON native_actions(action_type);
CREATE INDEX IF NOT EXISTS idx_na_block     ON native_actions(block_height);
CREATE INDEX IF NOT EXISTS idx_na_validator ON native_actions(validator);
CREATE INDEX IF NOT EXISTS idx_na_target    ON native_actions(target);

CREATE INDEX IF NOT EXISTS idx_vs_address ON validator_snapshots(address);
CREATE INDEX IF NOT EXISTS idx_vs_block   ON validator_snapshots(block_height);

CREATE TABLE IF NOT EXISTS candles (
    market_id   INTEGER NOT NULL,
    interval    TEXT NOT NULL,
    open_time   INTEGER NOT NULL,
    open        INTEGER NOT NULL,
    high        INTEGER NOT NULL,
    low         INTEGER NOT NULL,
    close       INTEGER NOT NULL,
    volume      INTEGER NOT NULL,
    trade_count INTEGER NOT NULL,
    PRIMARY KEY (market_id, interval, open_time)
);
CREATE INDEX IF NOT EXISTS idx_candles_market_interval ON candles(market_id, interval, open_time);
";

// ============================================================================
// Row types
// ============================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct BlockRow {
    pub height: i64,
    pub hash: String,
    pub parent_hash: String,
    pub timestamp: i64,
    pub proposer: String,
    pub gas_used: i64,
    pub gas_limit: i64,
    pub base_fee: i64,
    pub tx_count: i32,
    pub native_action_count: i32,
    pub epoch: i64,
    pub validator_set_hash: String,
    pub state_root: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TxRow {
    pub hash: String,
    pub block_height: i64,
    pub tx_index: i32,
    pub from_addr: String,
    pub to_addr: Option<String>,
    pub value: String,
    pub gas_limit: i64,
    pub gas_used: i64,
    pub gas_price: String,
    pub input_data: String,
    pub nonce: i64,
    pub status: bool,
    pub contract_address: Option<String>,
    pub tx_type: i32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LogRow {
    pub block_height: i64,
    pub tx_hash: String,
    pub log_index: i32,
    pub address: String,
    pub topic0: Option<String>,
    pub topic1: Option<String>,
    pub topic2: Option<String>,
    pub topic3: Option<String>,
    pub data: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct NativeActionRow {
    pub block_height: i64,
    pub action_index: i32,
    pub action_type: String,
    pub market_id: Option<i64>,
    pub order_id: Option<String>,
    pub validator: Option<String>,
    pub target: Option<String>,
    pub amount: Option<String>,
    pub proposal_id: Option<i64>,
    pub payload: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ValidatorSnapshotRow {
    pub block_height: i64,
    pub address: String,
    pub pubkey: String,
    pub power: i64,
    pub commission_bps: i32,
    pub status: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CandleRow {
    pub market_id: i64,
    pub interval: String,
    pub open_time: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
    pub trade_count: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Stats {
    pub total_blocks: i64,
    pub total_txs: i64,
    pub total_native_actions: i64,
    pub avg_block_time: f64,
    pub active_validators: i64,
    pub latest_block: i64,
    pub latest_epoch: i64,
}

// ============================================================================
// Database
// ============================================================================

#[derive(Clone)]
pub struct ExplorerDb {
    conn: Arc<Mutex<Connection>>,
}

impl ExplorerDb {
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // --- Indexer state ---

    pub fn get_indexer_state(&self, key: &str) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM indexer_state WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_indexer_state(&self, key: &str, value: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO indexer_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_last_indexed_height(&self) -> Result<Option<i64>, rusqlite::Error> {
        self.get_indexer_state("last_indexed_height")
            .map(|v| v.and_then(|s| s.parse().ok()))
    }

    // --- Write methods ---

    pub fn insert_block(&self, b: &BlockRow) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO blocks
             (height, hash, parent_hash, timestamp, proposer, gas_used, gas_limit,
              base_fee, tx_count, native_action_count, epoch, validator_set_hash, state_root)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                b.height,
                b.hash,
                b.parent_hash,
                b.timestamp,
                b.proposer,
                b.gas_used,
                b.gas_limit,
                b.base_fee,
                b.tx_count,
                b.native_action_count,
                b.epoch,
                b.validator_set_hash,
                b.state_root,
            ],
        )?;
        Ok(())
    }

    pub fn insert_transaction(&self, t: &TxRow) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO transactions
             (hash, block_height, tx_index, from_addr, to_addr, value, gas_limit,
              gas_used, gas_price, input_data, nonce, status, contract_address, tx_type)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                t.hash,
                t.block_height,
                t.tx_index,
                t.from_addr,
                t.to_addr,
                t.value,
                t.gas_limit,
                t.gas_used,
                t.gas_price,
                t.input_data,
                t.nonce,
                t.status as i32,
                t.contract_address,
                t.tx_type,
            ],
        )?;
        Ok(())
    }

    pub fn insert_log(&self, l: &LogRow) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO logs
             (block_height, tx_hash, log_index, address, topic0, topic1, topic2, topic3, data)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                l.block_height,
                l.tx_hash,
                l.log_index,
                l.address,
                l.topic0,
                l.topic1,
                l.topic2,
                l.topic3,
                l.data,
            ],
        )?;
        Ok(())
    }

    pub fn insert_native_action(&self, a: &NativeActionRow) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO native_actions
             (block_height, action_index, action_type, market_id, order_id,
              validator, target, amount, proposal_id, payload)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                a.block_height,
                a.action_index,
                a.action_type,
                a.market_id,
                a.order_id,
                a.validator,
                a.target,
                a.amount,
                a.proposal_id,
                a.payload,
            ],
        )?;
        Ok(())
    }

    pub fn insert_validator_snapshot(
        &self,
        v: &ValidatorSnapshotRow,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO validator_snapshots
             (block_height, address, pubkey, power, commission_bps, status)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                v.block_height,
                v.address,
                v.pubkey,
                v.power,
                v.commission_bps,
                v.status
            ],
        )?;
        Ok(())
    }

    /// Upsert a single trade into candle(s) across all intervals.
    /// `price_raw` and `qty_raw` are FixedPoint raw i128 values (8 decimal places).
    pub fn upsert_candle(
        &self,
        market_id: i64,
        timestamp: i64,
        price_raw: i64,
        qty_raw: i64,
    ) -> Result<(), rusqlite::Error> {
        const INTERVALS: &[(&str, i64)] = &[("1m", 60), ("5m", 300), ("15m", 900), ("1h", 3600)];
        let conn = self.conn.lock().unwrap();
        for &(interval, secs) in INTERVALS {
            let open_time = (timestamp / secs) * secs;
            conn.execute(
                "INSERT INTO candles (market_id, interval, open_time, open, high, low, close, volume, trade_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)
                 ON CONFLICT(market_id, interval, open_time) DO UPDATE SET
                     high = MAX(candles.high, excluded.high),
                     low = MIN(candles.low, excluded.low),
                     close = excluded.close,
                     volume = candles.volume + excluded.volume,
                     trade_count = candles.trade_count + 1",
                params![market_id, interval, open_time, price_raw, price_raw, price_raw, price_raw, qty_raw.abs()],
            )?;
        }
        Ok(())
    }

    /// Query candles for a market and interval within a time range.
    pub fn get_candles(
        &self,
        market_id: i64,
        interval: &str,
        from: Option<i64>,
        to: Option<i64>,
        limit: u32,
    ) -> Result<Vec<CandleRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from(
            "SELECT market_id, interval, open_time, open, high, low, close, volume, trade_count
             FROM candles WHERE market_id = ?1 AND interval = ?2",
        );
        let mut bind_idx = 3;
        if from.is_some() {
            sql.push_str(&format!(" AND open_time >= ?{bind_idx}"));
            bind_idx += 1;
        }
        if to.is_some() {
            sql.push_str(&format!(" AND open_time <= ?{bind_idx}"));
        }
        sql.push_str(" ORDER BY open_time ASC LIMIT ?99");

        let mut stmt = conn.prepare(&sql)?;

        // Bind parameters dynamically
        let mut idx = 1;
        stmt.raw_bind_parameter(idx, market_id)?;
        idx += 1;
        stmt.raw_bind_parameter(idx, interval)?;
        idx += 1;
        if let Some(f) = from {
            stmt.raw_bind_parameter(idx, f)?;
            idx += 1;
        }
        if let Some(t) = to {
            stmt.raw_bind_parameter(idx, t)?;
        }
        stmt.raw_bind_parameter(99, limit as i64)?;

        let mut rows_iter = stmt.raw_query();
        let mut result = Vec::new();
        while let Some(row) = rows_iter.next()? {
            result.push(CandleRow {
                market_id: row.get(0)?,
                interval: row.get(1)?,
                open_time: row.get(2)?,
                open: row.get(3)?,
                high: row.get(4)?,
                low: row.get(5)?,
                close: row.get(6)?,
                volume: row.get(7)?,
                trade_count: row.get(8)?,
            });
        }
        Ok(result)
    }

    pub fn delete_block_data(&self, height: i64) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM logs WHERE block_height = ?1", params![height])?;
        conn.execute(
            "DELETE FROM transactions WHERE block_height = ?1",
            params![height],
        )?;
        conn.execute(
            "DELETE FROM native_actions WHERE block_height = ?1",
            params![height],
        )?;
        conn.execute(
            "DELETE FROM validator_snapshots WHERE block_height = ?1",
            params![height],
        )?;
        conn.execute("DELETE FROM blocks WHERE height = ?1", params![height])?;
        Ok(())
    }

    // --- Read methods ---

    pub fn get_block(&self, height: i64) -> Result<Option<BlockRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT height,hash,parent_hash,timestamp,proposer,gas_used,gas_limit,
                    base_fee,tx_count,native_action_count,epoch,validator_set_hash,state_root
             FROM blocks WHERE height = ?1",
        )?;
        let mut rows = stmt.query(params![height])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_block(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_block_by_hash(&self, hash: &str) -> Result<Option<BlockRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT height,hash,parent_hash,timestamp,proposer,gas_used,gas_limit,
                    base_fee,tx_count,native_action_count,epoch,validator_set_hash,state_root
             FROM blocks WHERE hash = ?1",
        )?;
        let mut rows = stmt.query(params![hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_block(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_blocks(
        &self,
        page: u32,
        limit: u32,
    ) -> Result<(Vec<BlockRow>, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))?;
        let offset = (page.saturating_sub(1) * limit) as i64;
        let mut stmt = conn.prepare(
            "SELECT height,hash,parent_hash,timestamp,proposer,gas_used,gas_limit,
                    base_fee,tx_count,native_action_count,epoch,validator_set_hash,state_root
             FROM blocks ORDER BY height DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset], row_to_block)?;
        let blocks: Vec<BlockRow> = rows.filter_map(|r| r.ok()).collect();
        Ok((blocks, total))
    }

    pub fn get_block_transactions(&self, height: i64) -> Result<Vec<TxRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT hash,block_height,tx_index,from_addr,to_addr,value,gas_limit,
                    gas_used,gas_price,input_data,nonce,status,contract_address,tx_type
             FROM transactions WHERE block_height = ?1 ORDER BY tx_index",
        )?;
        let rows = stmt.query_map(params![height], row_to_tx)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_transaction(&self, hash: &str) -> Result<Option<TxRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT hash,block_height,tx_index,from_addr,to_addr,value,gas_limit,
                    gas_used,gas_price,input_data,nonce,status,contract_address,tx_type
             FROM transactions WHERE hash = ?1",
        )?;
        let mut rows = stmt.query(params![hash])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_tx(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_transaction_logs(&self, tx_hash: &str) -> Result<Vec<LogRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT block_height,tx_hash,log_index,address,topic0,topic1,topic2,topic3,data
             FROM logs WHERE tx_hash = ?1 ORDER BY log_index",
        )?;
        let rows = stmt.query_map(params![tx_hash], row_to_log)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_block_native_actions(
        &self,
        height: i64,
    ) -> Result<Vec<NativeActionRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT block_height,action_index,action_type,market_id,order_id,
                    validator,target,amount,proposal_id,payload
             FROM native_actions WHERE block_height = ?1 ORDER BY action_index",
        )?;
        let rows = stmt.query_map(params![height], row_to_native_action)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_address_transactions(
        &self,
        addr: &str,
        page: u32,
        limit: u32,
    ) -> Result<(Vec<TxRow>, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM transactions WHERE from_addr = ?1 OR to_addr = ?1",
            params![addr],
            |r| r.get(0),
        )?;
        let offset = (page.saturating_sub(1) * limit) as i64;
        let mut stmt = conn.prepare(
            "SELECT hash,block_height,tx_index,from_addr,to_addr,value,gas_limit,
                    gas_used,gas_price,input_data,nonce,status,contract_address,tx_type
             FROM transactions WHERE from_addr = ?1 OR to_addr = ?1
             ORDER BY block_height DESC, tx_index DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![addr, limit as i64, offset], row_to_tx)?;
        Ok((rows.filter_map(|r| r.ok()).collect(), total))
    }

    pub fn get_address_actions(
        &self,
        addr: &str,
        page: u32,
        limit: u32,
    ) -> Result<(Vec<NativeActionRow>, i64), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM native_actions WHERE validator = ?1 OR target = ?1",
            params![addr],
            |r| r.get(0),
        )?;
        let offset = (page.saturating_sub(1) * limit) as i64;
        let mut stmt = conn.prepare(
            "SELECT block_height,action_index,action_type,market_id,order_id,
                    validator,target,amount,proposal_id,payload
             FROM native_actions WHERE validator = ?1 OR target = ?1
             ORDER BY block_height DESC, action_index DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![addr, limit as i64, offset], row_to_native_action)?;
        Ok((rows.filter_map(|r| r.ok()).collect(), total))
    }

    pub fn get_address_tx_count(&self, addr: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM transactions WHERE from_addr = ?1 OR to_addr = ?1",
            params![addr],
            |r| r.get(0),
        )
    }

    pub fn get_address_action_count(&self, addr: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM native_actions WHERE validator = ?1 OR target = ?1",
            params![addr],
            |r| r.get(0),
        )
    }

    pub fn get_latest_validators(&self) -> Result<Vec<ValidatorSnapshotRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let max_height: Option<i64> = conn
            .query_row(
                "SELECT MAX(block_height) FROM validator_snapshots",
                [],
                |r| r.get(0),
            )
            .ok();
        let height = match max_height {
            Some(h) => h,
            None => return Ok(vec![]),
        };
        let mut stmt = conn.prepare(
            "SELECT block_height,address,pubkey,power,commission_bps,status
             FROM validator_snapshots WHERE block_height = ?1 ORDER BY power DESC",
        )?;
        let rows = stmt.query_map(params![height], row_to_validator_snapshot)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_validator_detail(
        &self,
        addr: &str,
    ) -> Result<Option<ValidatorSnapshotRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT block_height,address,pubkey,power,commission_bps,status
             FROM validator_snapshots WHERE address = ?1
             ORDER BY block_height DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![addr])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_validator_snapshot(row)?)),
            None => Ok(None),
        }
    }

    pub fn get_validator_blocks_proposed(&self, addr: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM blocks WHERE proposer = ?1",
            params![addr],
            |r| r.get(0),
        )
    }

    pub fn get_stats(&self) -> Result<Stats, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let total_blocks: i64 = conn.query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))?;
        let total_txs: i64 =
            conn.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
        let total_native_actions: i64 =
            conn.query_row("SELECT COUNT(*) FROM native_actions", [], |r| r.get(0))?;

        let avg_block_time: f64 =
            if total_blocks > 1 {
                conn.query_row(
                "SELECT CAST(MAX(timestamp) - MIN(timestamp) AS REAL) / (COUNT(*) - 1) FROM blocks",
                [], |r| r.get(0),
            ).unwrap_or(0.0)
            } else {
                0.0
            };

        let active_validators: i64 = {
            let max_height: Option<i64> = conn
                .query_row(
                    "SELECT MAX(block_height) FROM validator_snapshots",
                    [],
                    |r| r.get(0),
                )
                .ok();
            match max_height {
                Some(h) => conn.query_row(
                    "SELECT COUNT(*) FROM validator_snapshots WHERE block_height = ?1 AND status = 'active'",
                    params![h], |r| r.get(0),
                ).unwrap_or(0),
                None => 0,
            }
        };

        let (latest_block, latest_epoch) = conn
            .query_row(
                "SELECT height, epoch FROM blocks ORDER BY height DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap_or((0, 0));

        Ok(Stats {
            total_blocks,
            total_txs,
            total_native_actions,
            avg_block_time,
            active_validators,
            latest_block,
            latest_epoch,
        })
    }
}

// ============================================================================
// Row mapping helpers
// ============================================================================

fn row_to_block(row: &rusqlite::Row) -> Result<BlockRow, rusqlite::Error> {
    Ok(BlockRow {
        height: row.get(0)?,
        hash: row.get(1)?,
        parent_hash: row.get(2)?,
        timestamp: row.get(3)?,
        proposer: row.get(4)?,
        gas_used: row.get(5)?,
        gas_limit: row.get(6)?,
        base_fee: row.get(7)?,
        tx_count: row.get(8)?,
        native_action_count: row.get(9)?,
        epoch: row.get(10)?,
        validator_set_hash: row.get(11)?,
        state_root: row.get(12)?,
    })
}

fn row_to_tx(row: &rusqlite::Row) -> Result<TxRow, rusqlite::Error> {
    Ok(TxRow {
        hash: row.get(0)?,
        block_height: row.get(1)?,
        tx_index: row.get(2)?,
        from_addr: row.get(3)?,
        to_addr: row.get(4)?,
        value: row.get(5)?,
        gas_limit: row.get(6)?,
        gas_used: row.get(7)?,
        gas_price: row.get(8)?,
        input_data: row.get(9)?,
        nonce: row.get(10)?,
        status: row.get::<_, i32>(11)? != 0,
        contract_address: row.get(12)?,
        tx_type: row.get(13)?,
    })
}

fn row_to_log(row: &rusqlite::Row) -> Result<LogRow, rusqlite::Error> {
    Ok(LogRow {
        block_height: row.get(0)?,
        tx_hash: row.get(1)?,
        log_index: row.get(2)?,
        address: row.get(3)?,
        topic0: row.get(4)?,
        topic1: row.get(5)?,
        topic2: row.get(6)?,
        topic3: row.get(7)?,
        data: row.get(8)?,
    })
}

fn row_to_native_action(row: &rusqlite::Row) -> Result<NativeActionRow, rusqlite::Error> {
    Ok(NativeActionRow {
        block_height: row.get(0)?,
        action_index: row.get(1)?,
        action_type: row.get(2)?,
        market_id: row.get(3)?,
        order_id: row.get(4)?,
        validator: row.get(5)?,
        target: row.get(6)?,
        amount: row.get(7)?,
        proposal_id: row.get(8)?,
        payload: row.get(9)?,
    })
}

fn row_to_validator_snapshot(row: &rusqlite::Row) -> Result<ValidatorSnapshotRow, rusqlite::Error> {
    Ok(ValidatorSnapshotRow {
        block_height: row.get(0)?,
        address: row.get(1)?,
        pubkey: row.get(2)?,
        power: row.get(3)?,
        commission_bps: row.get(4)?,
        status: row.get(5)?,
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_block(height: i64) -> BlockRow {
        BlockRow {
            height,
            hash: format!("0x{:064x}", height),
            parent_hash: format!("0x{:064x}", height.saturating_sub(1)),
            timestamp: 1_700_000_000 + height,
            proposer: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            gas_used: 21000,
            gas_limit: 30_000_000,
            base_fee: 1_000_000_000,
            tx_count: 1,
            native_action_count: 2,
            epoch: height / 100,
            validator_set_hash: format!("0x{:064x}", 0),
            state_root: format!("0x{:064x}", 1),
        }
    }

    fn test_tx(hash: &str, height: i64) -> TxRow {
        TxRow {
            hash: hash.into(),
            block_height: height,
            tx_index: 0,
            from_addr: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            to_addr: Some("0xcccccccccccccccccccccccccccccccccccccccc".into()),
            value: "0x1000".into(),
            gas_limit: 21000,
            gas_used: 21000,
            gas_price: "0x3b9aca00".into(),
            input_data: "0x".into(),
            nonce: 0,
            status: true,
            contract_address: None,
            tx_type: 2,
        }
    }

    fn test_native_action(height: i64, idx: i32) -> NativeActionRow {
        NativeActionRow {
            block_height: height,
            action_index: idx,
            action_type: "Delegate".into(),
            market_id: None,
            order_id: None,
            validator: Some("0xdddddddddddddddddddddddddddddddddddddd".into()),
            target: None,
            amount: Some("0x1000".into()),
            proposal_id: None,
            payload: r#"{"Delegate":{"validator":"0xdd","amount":"0x1000"}}"#.into(),
        }
    }

    #[test]
    fn schema_creation() {
        let db = ExplorerDb::open_in_memory().unwrap();
        assert!(db.get_last_indexed_height().unwrap().is_none());
    }

    #[test]
    fn indexer_state_roundtrip() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.set_indexer_state("last_indexed_height", "42").unwrap();
        assert_eq!(db.get_last_indexed_height().unwrap(), Some(42));
    }

    #[test]
    fn insert_and_query_blocks() {
        let db = ExplorerDb::open_in_memory().unwrap();
        for h in 0..5 {
            db.insert_block(&test_block(h)).unwrap();
        }
        let (blocks, total) = db.get_blocks(1, 3).unwrap();
        assert_eq!(total, 5);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].height, 4);
        assert_eq!(blocks[2].height, 2);
    }

    #[test]
    fn insert_and_query_tx() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(1)).unwrap();
        let tx = test_tx("0xabc", 1);
        db.insert_transaction(&tx).unwrap();
        let found = db.get_transaction("0xabc").unwrap().unwrap();
        assert_eq!(found.from_addr, tx.from_addr);
        assert!(found.status);
    }

    #[test]
    fn address_transactions() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(1)).unwrap();
        db.insert_transaction(&test_tx("0xabc", 1)).unwrap();
        let (txs, total) = db
            .get_address_transactions("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 1, 10)
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(txs[0].hash, "0xabc");
        let (txs2, _) = db
            .get_address_transactions("0xcccccccccccccccccccccccccccccccccccccccc", 1, 10)
            .unwrap();
        assert_eq!(txs2.len(), 1);
    }

    #[test]
    fn native_actions() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(1)).unwrap();
        db.insert_native_action(&test_native_action(1, 0)).unwrap();
        db.insert_native_action(&test_native_action(1, 1)).unwrap();
        let (actions, total) = db
            .get_address_actions("0xdddddddddddddddddddddddddddddddddddddd", 1, 10)
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(actions[0].action_type, "Delegate");
    }

    #[test]
    fn reorg_deletes_old_data() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(5)).unwrap();
        db.insert_transaction(&test_tx("0xold", 5)).unwrap();
        db.insert_native_action(&test_native_action(5, 0)).unwrap();
        assert!(db.get_block(5).unwrap().is_some());
        assert!(db.get_transaction("0xold").unwrap().is_some());
        db.delete_block_data(5).unwrap();
        assert!(db.get_block(5).unwrap().is_none());
        assert!(db.get_transaction("0xold").unwrap().is_none());
        let mut new_block = test_block(5);
        new_block.hash = "0xnew_hash".into();
        db.insert_block(&new_block).unwrap();
        assert_eq!(db.get_block(5).unwrap().unwrap().hash, "0xnew_hash");
    }

    #[test]
    fn stats() {
        let db = ExplorerDb::open_in_memory().unwrap();
        for h in 0..10 {
            db.insert_block(&test_block(h)).unwrap();
        }
        let stats = db.get_stats().unwrap();
        assert_eq!(stats.total_blocks, 10);
        assert_eq!(stats.latest_block, 9);
    }

    #[test]
    fn backfill_resume() {
        let db = ExplorerDb::open_in_memory().unwrap();
        for h in 1..=10 {
            db.insert_block(&test_block(h)).unwrap();
            db.set_indexer_state("last_indexed_height", &h.to_string())
                .unwrap();
        }
        assert_eq!(db.get_last_indexed_height().unwrap(), Some(10));
        assert!(db.get_block(1).unwrap().is_some());
        assert!(db.get_block(10).unwrap().is_some());
        assert!(db.get_block(11).unwrap().is_none());
    }

    #[test]
    fn search_by_hash() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(42)).unwrap();
        let block = db
            .get_block_by_hash(&format!("0x{:064x}", 42))
            .unwrap()
            .unwrap();
        assert_eq!(block.height, 42);
    }

    #[test]
    fn logs_insert_and_query() {
        let db = ExplorerDb::open_in_memory().unwrap();
        db.insert_block(&test_block(1)).unwrap();
        db.insert_transaction(&test_tx("0xabc", 1)).unwrap();
        db.insert_log(&LogRow {
            block_height: 1,
            tx_hash: "0xabc".into(),
            log_index: 0,
            address: "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".into(),
            topic0: Some("0xddf252ad".into()),
            topic1: None,
            topic2: None,
            topic3: None,
            data: "0x1234".into(),
        })
        .unwrap();
        let logs = db.get_transaction_logs("0xabc").unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].address,
            "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
        );
    }
}
