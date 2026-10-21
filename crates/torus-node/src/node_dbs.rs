//! Section 71.8 option A: the node's two RocksDB instances.
//!
//! * The **state DB** (`<data_dir>`): execution state, block headers / bodies,
//!   and the state meta keys of `cf_consensus_meta` (applied marker, running
//!   hash, epoch plan, ...), all written by the execution / checkpoint path.
//! * The **consensus DB** (`<data_dir>/consensus-db`): ALL HotStuff store
//!   traffic (`RocksKVStore`: blocks, PCs, locks, vote state, frontier), the
//!   DA store (`NativeDaStore`: bodies, shards) and, since T5.3, the commit
//!   manifest (`cf_commit_manifest`, the commit hook's first durable write;
//!   `TORUS_MANIFEST_IN_STATE_DB=1` keeps it in the state DB for an A/B).
//!
//! Each has its own WAL and write queue, so the consensus thread's QC / lock /
//! vote writes and the mempool's DA mirrors no longer wait behind the state
//! checkpoint `WriteBatch`. The state DB syncs the consensus DB's WAL with its
//! own ([`StateDb::attach_wal_peer`]), so every `sync_wal` durability point
//! covers both. Only power-loss ordering across the two DBs is weaker than
//! with one WAL.
//!
//! Restart rule: the node never starts with empty HotStuff state (vote state,
//! locks) over a chain that has state, and does not migrate an old-layout
//! data dir (HotStuff keys in the state DB): it refuses to start instead.

use std::path::Path;

use hotstuff_rs::block_tree::variables::COMMITTED_VALIDATOR_SET;
use torus_consensus::RocksKVStore;
use torus_state::cf::{CF_ACCOUNTS, CF_BLOCK_HEADERS, CF_CONSENSUS_META, CF_STAKING_VALIDATORS};
use torus_state::{consensus_db_path, NativeDaStore, StateBackend, StateDb};
use torus_types::book_mode::{self, BookMode};

/// The node's state DB and consensus DB, opened and checked.
pub struct NodeDbs {
    pub state: StateDb,
    pub consensus: StateDb,
}

impl NodeDbs {
    /// The HotStuff store, over the consensus DB.
    pub fn kv_store(&self) -> RocksKVStore {
        RocksKVStore::new(self.consensus.db_arc())
    }

    /// The DA body store, over the consensus DB.
    pub fn da_store(&self) -> NativeDaStore {
        NativeDaStore::new(self.consensus.clone())
    }
}

/// What a data dir holds, as the restart rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutProbe {
    /// HotStuff state in the state DB's `cf_consensus_meta` (the layout
    /// before option A).
    pub hotstuff_in_state_db: bool,
    /// HotStuff state in the consensus DB.
    pub hotstuff_in_consensus_db: bool,
    /// The state DB holds a chain (genesis accounts / validators, or
    /// committed headers).
    pub state_db_has_chain: bool,
}

/// The restart rule. `Err` is the refusal, naming the data dir.
pub fn check_layout(p: LayoutProbe, data_dir: &Path) -> Result<(), String> {
    let dir = data_dir.display();
    let cdb = consensus_db_path(data_dir);
    let cdb = cdb.display();
    if p.hotstuff_in_state_db {
        return Err(format!(
            "refusing to start: data dir {dir} has HotStuff consensus state in the state DB \
             (the layout before section 71.8 option A). This build keeps it in {cdb} and does \
             not migrate it. Start from a fresh data dir, or run the previous build."
        ));
    }
    if p.hotstuff_in_consensus_db && !p.state_db_has_chain {
        return Err(format!(
            "refusing to start: {cdb} has HotStuff consensus state but the state DB in {dir} \
             is empty; genesis would re-initialize the locks over it. Remove the whole data dir \
             to start over."
        ));
    }
    if !p.hotstuff_in_consensus_db && p.state_db_has_chain {
        return Err(format!(
            "refusing to start: the state DB in {dir} holds a chain but {cdb} has no HotStuff \
             consensus state; starting would vote with empty vote state (equivocation risk). \
             Restore {cdb} or start from a fresh data dir."
        ));
    }
    Ok(())
}

/// HotStuff state is present: `Replica::initialize` always writes the
/// committed validator set (a 1-byte key no state meta key can equal).
fn has_hotstuff_state(db: &StateDb) -> Result<bool, String> {
    db.get_cf_raw(CF_CONSENSUS_META, &COMMITTED_VALIDATOR_SET)
        .map(|v| v.is_some())
        .map_err(|e| format!("probe HotStuff state: {e}"))
}

/// The state DB holds a chain: genesis accounts (what the genesis step
/// checks), genesis validators (always seeded), or committed headers. One
/// bounded seek per CF.
fn state_has_chain(db: &StateDb) -> Result<bool, String> {
    for cf in [CF_ACCOUNTS, CF_STAKING_VALIDATORS, CF_BLOCK_HEADERS] {
        let first = StateBackend::iterate_cf_from(db, cf, &[], 1)
            .map_err(|e| format!("probe {cf}: {e}"))?;
        if !first.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Genesis has already been applied to this data dir. One point read and one
/// bounded seek, never a scan (bench queue 8f: the old
/// `all_accounts().is_empty()` loaded every account on every boot).
///
/// * HotStuff state in the consensus DB: `Replica::initialize`, the LAST
///   genesis step, writes `COMMITTED_VALIDATOR_SET` in one batch, so it is
///   present once genesis completed, also for a genesis without EVM accounts
///   (which the old check re-seeded on every boot, resetting the HotStuff
///   locks and view).
/// * Any row in `CF_ACCOUNTS`: the old check, kept so every data dir it
///   called initialized still is.
///
/// A crash inside genesis: after the first account or validator row, the
/// restart rule in [`open_node_dbs`] refuses the dir (chain without HotStuff
/// state) before this runs; before it, nothing marks the dir and genesis runs
/// again (its writes are idempotent puts). Both as before.
pub fn genesis_applied(dbs: &NodeDbs) -> Result<bool, String> {
    if has_hotstuff_state(&dbs.consensus)? {
        return Ok(true);
    }
    StateBackend::prefix_exists(&dbs.state, CF_ACCOUNTS, &[])
        .map_err(|e| format!("probe {CF_ACCOUNTS}: {e}"))
}

/// Boot step 4: seed `genesis` (state, then the HotStuff replica) unless
/// [`genesis_applied`]. Returns whether it seeded.
pub fn apply_genesis_if_fresh(
    dbs: &NodeDbs,
    genesis: &torus_genesis::Genesis,
) -> Result<bool, String> {
    if genesis_applied(dbs)? {
        tracing::info!("database already initialized, skipping genesis");
        return Ok(false);
    }
    tracing::info!("initializing from genesis");
    let state_root = genesis
        .initialize(&dbs.state)
        .map_err(|e| format!("genesis initialize: {e}"))?;
    tracing::info!(%state_root, "genesis state seeded");
    // s131: the DB names the chain's book mode from genesis on, so a restart
    // (with or without `--genesis`) can never pick another mode, even before
    // the first block wrote the marker.
    torus_bridge::native_executor::write_book_mode_marker(&dbs.state, genesis.consensus.book_mode)?;
    let (app_state, vs_state) = genesis
        .to_hotstuff_genesis()
        .map_err(|e| format!("genesis hotstuff state: {e}"))?;
    hotstuff_rs::replica::Replica::initialize(dbs.kv_store(), app_state, vs_state);
    tracing::info!("consensus replica initialized with genesis validator set");
    Ok(true)
}

/// s131: the node's order-book storage mode. The chain's mode is `genesis`
/// (`consensus.book_mode`); without `--genesis` (`None`) it is the DB's
/// `__book_mode__` marker (written when genesis was applied), else Classic.
/// `env` (`TORUS_BOOK_ROWS`) may only repeat it (deprecated, warned); a value
/// that disagrees or is not a mode refuses to start, and so does a DB marker
/// that names another mode. Runs before genesis is applied.
pub fn node_book_mode(
    state: &StateDb,
    genesis: Option<BookMode>,
    env: Option<&str>,
) -> Result<BookMode, String> {
    let refuse = |e: String| format!("refusing to start: {e}");
    let marker = torus_bridge::native_executor::read_book_mode_marker(state).map_err(refuse)?;
    let (chain, source) = match genesis {
        Some(m) => (m, "genesis consensus.book_mode"),
        None => match marker {
            Some(b) => (
                BookMode::from_marker_byte(b).ok_or_else(|| {
                    refuse(format!("the DB's book-mode marker {b} is not a known mode"))
                })?,
                "the DB's book-mode marker; no --genesis",
            ),
            None => (
                BookMode::Classic,
                "default; no --genesis and no book-mode marker",
            ),
        },
    };
    let resolved = book_mode::resolve_node_book_mode(chain, env).map_err(refuse)?;
    if resolved.deprecated_env {
        tracing::warn!(
            "TORUS_BOOK_ROWS is deprecated: the book mode comes from genesis consensus.book_mode \
             (\"{}\" on this chain); the value agrees, so the node starts. Unset it",
            chain.name()
        );
    }
    book_mode::check_book_mode_marker(marker, chain).map_err(refuse)?;
    tracing::info!(
        book_mode = chain.name(),
        "book mode: {} ({source})",
        chain.describe()
    );
    Ok(chain)
}

/// Open the state DB and the consensus DB under `data_dir`, apply the
/// restart rule, and attach the consensus DB's WAL to the state DB's
/// `sync_wal`.
pub fn open_node_dbs(data_dir: &Path) -> Result<NodeDbs, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("create {}: {e}", data_dir.display()))?;
    let state = StateDb::open(data_dir).map_err(|e| format!("open state DB: {e}"))?;
    let hotstuff_in_state_db = has_hotstuff_state(&state)?;
    let state_db_has_chain = state_has_chain(&state)?;
    if hotstuff_in_state_db {
        // Refuse before creating `consensus-db` (`check_layout` always
        // refuses this): an old-layout dir is left as it was, so the previous
        // build still runs on it.
        let probe = LayoutProbe {
            hotstuff_in_state_db,
            hotstuff_in_consensus_db: false,
            state_db_has_chain,
        };
        check_layout(probe, data_dir)?;
    }
    let consensus = StateDb::open_consensus(&consensus_db_path(data_dir))
        .map_err(|e| format!("open consensus DB: {e}"))?;
    check_layout(
        LayoutProbe {
            hotstuff_in_state_db,
            hotstuff_in_consensus_db: has_hotstuff_state(&consensus)?,
            state_db_has_chain,
        },
        data_dir,
    )?;
    state
        .attach_wal_peer(&consensus)
        .map_err(|e| format!("attach consensus DB WAL: {e}"))?;
    Ok(NodeDbs { state, consensus })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotstuff_rs::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use hotstuff_rs::block_tree::variables::{
        APP_FED_BLOCK_HEIGHT, HIGHEST_PC, HIGHEST_VIEW_ENTERED, HIGHEST_VIEW_PHASE_VOTED, LOCKED_PC,
    };
    use hotstuff_rs::replica::Replica;
    use torus_consensus::kv_store::RocksWriteBatch;
    use torus_state::cf::{CF_NATIVE_PENDING, META_NATIVE_APPLIED_HEIGHT};

    fn genesis() -> torus_genesis::Genesis {
        torus_genesis::Genesis::from_json(include_str!("../../../devnet/genesis.json")).unwrap()
    }

    fn genesis_with_book_mode(mode: &str) -> torus_genesis::Genesis {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("../../../devnet/genesis.json")).unwrap();
        v["consensus"]["book_mode"] = serde_json::json!(mode);
        torus_genesis::Genesis::from_json(&v.to_string()).unwrap()
    }

    fn marker(db: &StateDb) -> Option<u8> {
        torus_bridge::native_executor::read_book_mode_marker(db).unwrap()
    }

    /// s131: applying genesis writes the chain's book-mode marker, so the DB
    /// names the mode before the first block; a restart leaves it.
    #[test]
    fn fresh_genesis_writes_its_book_mode_marker() {
        for (name, byte) in [
            ("classic", 0u8),
            ("order_rows", 1),
            ("level_authority_chunked", 3),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let g = genesis_with_book_mode(name);
            {
                let dbs = open_node_dbs(dir.path()).unwrap();
                assert_eq!(marker(&dbs.state), None, "{name}: fresh");
                assert!(apply_genesis_if_fresh(&dbs, &g).unwrap());
                assert_eq!(marker(&dbs.state), Some(byte), "{name}");
            }
            let dbs = open_node_dbs(dir.path()).unwrap();
            assert!(
                !apply_genesis_if_fresh(&dbs, &genesis()).unwrap(),
                "{name}: restart"
            );
            assert_eq!(
                marker(&dbs.state),
                Some(byte),
                "{name}: restart keeps the marker"
            );
        }
        // A genesis without the field writes Classic.
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        assert!(apply_genesis_if_fresh(&dbs, &genesis()).unwrap());
        assert_eq!(marker(&dbs.state), Some(0));
    }

    /// s131: genesis gives the mode; `TORUS_BOOK_ROWS` may only repeat it; a
    /// DB marker naming another mode refuses; without `--genesis` the marker
    /// gives it.
    #[test]
    fn node_book_mode_follows_genesis_env_and_marker() {
        use torus_types::book_mode::BookMode::{Classic, LevelAuthorityChunked as M3, OrderRows};
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        let db = &dbs.state;
        // fresh DB
        assert_eq!(node_book_mode(db, Some(M3), None), Ok(M3));
        assert_eq!(node_book_mode(db, Some(M3), Some("")), Ok(M3));
        assert_eq!(node_book_mode(db, Some(M3), Some("3")), Ok(M3));
        assert_eq!(node_book_mode(db, Some(Classic), Some("0")), Ok(Classic));
        assert_eq!(
            node_book_mode(db, None, None),
            Ok(Classic),
            "no genesis, no marker"
        );
        for (g, env) in [
            (Some(M3), "0"),
            (Some(Classic), "3"),
            (Some(M3), "x"),
            (None, "3"),
        ] {
            let err = node_book_mode(db, g, Some(env)).unwrap_err();
            assert!(
                err.starts_with("refusing to start: ") && err.contains("TORUS_BOOK_ROWS"),
                "{g:?} {env}: {err}"
            );
        }
        // a DB written under mode 3
        torus_bridge::native_executor::write_book_mode_marker(db, M3).unwrap();
        assert_eq!(node_book_mode(db, Some(M3), None), Ok(M3));
        assert_eq!(
            node_book_mode(db, None, None),
            Ok(M3),
            "restart without --genesis: the marker"
        );
        assert_eq!(node_book_mode(db, None, Some("3")), Ok(M3));
        assert!(node_book_mode(db, None, Some("0"))
            .unwrap_err()
            .contains("TORUS_BOOK_ROWS"));
        for g in [Classic, OrderRows] {
            let err = node_book_mode(db, Some(g), None).unwrap_err();
            assert!(
                err.starts_with("refusing to start: book-mode marker mismatch"),
                "{err}"
            );
        }
        // a marker byte that is no mode
        db.put_cf_raw(torus_state::cf::CF_NATIVE_MARKETS, b"__book_mode__", &[7])
            .unwrap();
        assert!(node_book_mode(db, None, None)
            .unwrap_err()
            .contains("not a known mode"));
        assert!(node_book_mode(db, Some(M3), None)
            .unwrap_err()
            .contains("book-mode marker mismatch"));
    }

    /// What `run` does on a first start: genesis state, then the replica.
    fn init_chain(dbs: &NodeDbs) {
        let g = genesis();
        g.initialize(&dbs.state).unwrap();
        let (app_state, vs_state) = g.to_hotstuff_genesis().unwrap();
        Replica::initialize(dbs.kv_store(), app_state, vs_state);
    }

    fn da_action(nonce: u64) -> torus_types::SignedNativeAction {
        use torus_types::{ActionSignature, NativeAction, Signature};
        torus_types::SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    fn meta_keys(db: &StateDb) -> Vec<Vec<u8>> {
        StateBackend::iterate_cf(db, CF_CONSENSUS_META, None)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    /// HotStuff keys are a 1-byte variable prefix (0..=23); state meta keys
    /// are ASCII.
    fn is_hotstuff_key(key: &[u8]) -> bool {
        key.first().is_some_and(|b| *b <= APP_FED_BLOCK_HEIGHT[0])
    }

    const HOTSTUFF_INIT_KEYS: [[u8; 1]; 4] = [
        COMMITTED_VALIDATOR_SET,
        LOCKED_PC,
        HIGHEST_VIEW_ENTERED,
        HIGHEST_PC,
    ];

    #[test]
    fn fresh_data_dir_opens_both_dbs_and_attaches_the_wal_peer() {
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        assert!(dir.path().join("consensus-db").is_dir());
        let before = dbs
            .consensus
            .wal_sync_count()
            .expect("statistics on by default");
        dbs.state.sync_wal().unwrap();
        assert!(
            dbs.consensus.wal_sync_count().unwrap() > before,
            "state sync_wal did not sync the consensus DB's WAL"
        );
    }

    /// Every HotStuff write (genesis init, vote state) lands in the consensus
    /// DB; the state DB's `cf_consensus_meta` keeps only state meta keys.
    #[test]
    fn hotstuff_writes_land_in_the_consensus_db_not_the_state_db() {
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        init_chain(&dbs);
        let mut kv = dbs.kv_store();
        let mut wb = RocksWriteBatch::new();
        wb.set(&HIGHEST_VIEW_PHASE_VOTED, &7u64.to_le_bytes());
        kv.write(wb);
        // A state meta key, written as the checkpoint batch writes it.
        dbs.state
            .put_cf_raw(
                CF_CONSENSUS_META,
                META_NATIVE_APPLIED_HEIGHT,
                &3u64.to_be_bytes(),
            )
            .unwrap();

        for key in HOTSTUFF_INIT_KEYS.iter().chain([&HIGHEST_VIEW_PHASE_VOTED]) {
            assert!(
                dbs.consensus
                    .get_cf_raw(CF_CONSENSUS_META, key)
                    .unwrap()
                    .is_some(),
                "HotStuff key {key:?} missing in the consensus DB"
            );
            assert!(
                dbs.state
                    .get_cf_raw(CF_CONSENSUS_META, key)
                    .unwrap()
                    .is_none(),
                "HotStuff key {key:?} reached the state DB"
            );
        }
        let state_keys = meta_keys(&dbs.state);
        assert!(
            state_keys.iter().all(|k| !is_hotstuff_key(k)),
            "{state_keys:?}"
        );
        assert!(state_keys.iter().any(|k| k == META_NATIVE_APPLIED_HEIGHT));
        let consensus_keys = meta_keys(&dbs.consensus);
        assert!(
            consensus_keys.iter().all(|k| is_hotstuff_key(k)),
            "{consensus_keys:?}"
        );
    }

    #[test]
    fn da_writes_land_in_the_consensus_db_not_the_state_db() {
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        let a = da_action(1);
        dbs.da_store().put_batch(std::slice::from_ref(&a)).unwrap();
        let h = torus_types::compute_action_hash(&a);
        assert!(dbs
            .consensus
            .get_cf_raw(CF_NATIVE_PENDING, h.as_slice())
            .unwrap()
            .is_some());
        assert!(dbs
            .state
            .get_cf_raw(CF_NATIVE_PENDING, h.as_slice())
            .unwrap()
            .is_none());
    }

    /// A restart reopens both DBs, passes the restart rule, and finds the
    /// HotStuff state (vote state included) and the DA bodies.
    #[test]
    fn reopen_recovers_hotstuff_state_and_da_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let a = da_action(9);
        let (vset, voted) = {
            let dbs = open_node_dbs(dir.path()).unwrap();
            init_chain(&dbs);
            let mut kv = dbs.kv_store();
            let mut wb = RocksWriteBatch::new();
            wb.set(&HIGHEST_VIEW_PHASE_VOTED, &3u64.to_le_bytes());
            kv.write(wb);
            dbs.da_store().put(&a).unwrap();
            (
                kv.get(&COMMITTED_VALIDATOR_SET).unwrap(),
                kv.get(&HIGHEST_VIEW_PHASE_VOTED).unwrap(),
            )
        };
        let dbs = open_node_dbs(dir.path()).expect("restart on the new layout");
        let kv = dbs.kv_store();
        assert_eq!(kv.get(&COMMITTED_VALIDATOR_SET), Some(vset));
        assert_eq!(kv.get(&HIGHEST_VIEW_PHASE_VOTED), Some(voted));
        assert!(dbs
            .da_store()
            .get(&torus_types::compute_action_hash(&a))
            .unwrap()
            .is_some());
    }

    /// Requirement 4: an old-layout data dir (HotStuff in the state DB, no
    /// consensus DB) is refused, and `consensus-db` is not created.
    #[test]
    fn old_layout_data_dir_is_refused_and_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        {
            // The wiring before option A: one DB for state and HotStuff.
            let state = StateDb::open(dir.path()).unwrap();
            let g = genesis();
            g.initialize(&state).unwrap();
            let (app_state, vs_state) = g.to_hotstuff_genesis().unwrap();
            Replica::initialize(RocksKVStore::new(state.db_arc()), app_state, vs_state);
        }
        let err = open_node_dbs(dir.path())
            .err()
            .expect("old layout must be refused");
        assert!(err.contains("refusing to start"), "{err}");
        assert!(err.contains("before section 71.8 option A"), "{err}");
        assert!(err.contains(&dir.path().display().to_string()), "{err}");
        assert!(
            !dir.path().join("consensus-db").exists(),
            "old-layout dir was changed"
        );
    }

    /// A chain without its consensus DB (deleted, or a restored state
    /// snapshot) is refused: never vote with empty vote state.
    #[test]
    fn chain_without_hotstuff_state_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        {
            let dbs = open_node_dbs(dir.path()).unwrap();
            init_chain(&dbs);
        }
        std::fs::remove_dir_all(dir.path().join("consensus-db")).unwrap();
        let err = open_node_dbs(dir.path()).err().expect("must refuse");
        assert!(err.contains("equivocation risk"), "{err}");
    }

    /// HotStuff state over an empty state DB is refused: genesis would reset
    /// the locks over it.
    #[test]
    fn hotstuff_state_over_an_empty_state_db_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        {
            let dbs = open_node_dbs(dir.path()).unwrap();
            let (app_state, vs_state) = genesis().to_hotstuff_genesis().unwrap();
            Replica::initialize(dbs.kv_store(), app_state, vs_state);
        }
        let err = open_node_dbs(dir.path()).err().expect("must refuse");
        assert!(err.contains("state DB"), "{err}");
        assert!(err.contains("is empty"), "{err}");
    }

    // ---- Boot step 4: genesis applied once, cheap probe (bench queue 8f) ----

    use torus_state::cf::{CF_FEE_CONFIG, CF_STAKING_VALIDATORS as CF_VALIDATORS};

    fn genesis_without_accounts() -> torus_genesis::Genesis {
        let mut g: serde_json::Value =
            serde_json::from_str(include_str!("../../../devnet/genesis.json")).unwrap();
        g["accounts"] = serde_json::json!([]);
        g["evm_alloc"] = serde_json::json!({});
        torus_genesis::Genesis::from_json(&g.to_string()).unwrap()
    }

    type Rows = Vec<(Vec<u8>, Vec<u8>)>;

    /// Every row genesis writes that a restart must leave alone.
    fn chain_rows(dbs: &NodeDbs) -> Vec<(&'static str, Rows)> {
        let mut rows: Vec<_> = [CF_ACCOUNTS, CF_VALIDATORS, CF_FEE_CONFIG]
            .into_iter()
            .map(|cf| (cf, StateBackend::iterate_cf(&dbs.state, cf, None).unwrap()))
            .collect();
        rows.push((
            "consensus-db",
            StateBackend::iterate_cf(&dbs.consensus, CF_CONSENSUS_META, None).unwrap(),
        ));
        rows
    }

    fn advance_hotstuff_view(dbs: &NodeDbs, view: u64) {
        let mut kv = dbs.kv_store();
        let mut wb = RocksWriteBatch::new();
        wb.set(&HIGHEST_VIEW_ENTERED, &view.to_le_bytes());
        kv.write(wb);
    }

    /// A fresh data dir is seeded once; a restart skips genesis and leaves
    /// state and HotStuff state as they were.
    #[test]
    fn genesis_is_applied_once_and_a_restart_skips_it() {
        let dir = tempfile::tempdir().unwrap();
        let g = genesis();
        let before = {
            let dbs = open_node_dbs(dir.path()).unwrap();
            assert!(!genesis_applied(&dbs).unwrap());
            assert!(apply_genesis_if_fresh(&dbs, &g).unwrap(), "fresh dir seeded");
            assert!(genesis_applied(&dbs).unwrap());
            assert!(!StateBackend::iterate_cf(&dbs.state, CF_ACCOUNTS, None)
                .unwrap()
                .is_empty());
            // A chain that moved on after genesis.
            advance_hotstuff_view(&dbs, 42);
            dbs.state
                .put_cf_raw(CF_FEE_CONFIG, b"gov_params", b"changed by governance")
                .unwrap();
            chain_rows(&dbs)
        };
        let dbs = open_node_dbs(dir.path()).expect("restart");
        assert!(!apply_genesis_if_fresh(&dbs, &g).unwrap(), "restart re-seeded genesis");
        assert_eq!(chain_rows(&dbs), before, "restart changed state");
    }

    /// A chain whose genesis has no EVM accounts (validators only) is still
    /// initialized: a restart must not re-seed it or reset the HotStuff
    /// locks / view. Before: `all_accounts().is_empty()` re-ran genesis and
    /// `Replica::initialize` on every boot.
    #[test]
    fn zero_account_chain_restart_skips_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let g = genesis_without_accounts();
        let before = {
            let dbs = open_node_dbs(dir.path()).unwrap();
            assert!(apply_genesis_if_fresh(&dbs, &g).unwrap());
            assert!(StateBackend::iterate_cf(&dbs.state, CF_ACCOUNTS, None)
                .unwrap()
                .is_empty());
            advance_hotstuff_view(&dbs, 42);
            chain_rows(&dbs)
        };
        let dbs = open_node_dbs(dir.path()).expect("restart");
        assert!(genesis_applied(&dbs).unwrap(), "zero-account chain seen as fresh");
        assert!(!apply_genesis_if_fresh(&dbs, &g).unwrap());
        assert_eq!(chain_rows(&dbs), before, "restart reset state / HotStuff view");
    }

    /// Equivalence with the old check (`!all_accounts().is_empty()`) on every
    /// case it distinguishes, across HotStuff state present / absent (the
    /// probe is called with the DBs opened directly, so the restart rule does
    /// not filter any combination out). The one new verdict: no accounts but
    /// HotStuff initialized is "applied".
    #[test]
    fn probe_matches_the_old_all_accounts_check() {
        for (accounts, hotstuff) in [(false, false), (true, false), (false, true), (true, true)] {
            let dir = tempfile::tempdir().unwrap();
            let dbs = NodeDbs {
                state: StateDb::open(dir.path()).unwrap(),
                consensus: StateDb::open_consensus(&consensus_db_path(dir.path())).unwrap(),
            };
            if accounts {
                dbs.state
                    .put_cf_raw(CF_ACCOUNTS, &[7u8; 20], &[0u8; 72])
                    .unwrap();
            }
            if hotstuff {
                let (app_state, vs_state) = genesis().to_hotstuff_genesis().unwrap();
                Replica::initialize(dbs.kv_store(), app_state, vs_state);
            }
            let old = !dbs.state.all_accounts().unwrap().is_empty();
            let new = genesis_applied(&dbs).unwrap();
            if accounts {
                assert_eq!(new, old, "accounts={accounts} hotstuff={hotstuff}");
            } else {
                assert!(!old);
                assert_eq!(new, hotstuff, "accounts=false hotstuff={hotstuff}");
            }
        }
    }

    /// Crash inside genesis init before any account / validator row (only
    /// contract code written): the restart rule lets it through and genesis
    /// runs again, as before. A crash after the first account or validator
    /// row is refused at open (`chain_without_hotstuff_state_is_refused`).
    #[test]
    fn crash_before_the_first_genesis_row_reseeds() {
        let dir = tempfile::tempdir().unwrap();
        {
            let dbs = open_node_dbs(dir.path()).unwrap();
            dbs.state
                .put_code(&alloy_primitives::B256::repeat_byte(1), b"\x60\x00")
                .unwrap();
        }
        let dbs = open_node_dbs(dir.path()).expect("no chain rows yet");
        assert!(!genesis_applied(&dbs).unwrap());
        assert!(apply_genesis_if_fresh(&dbs, &genesis()).unwrap());
        drop(dbs);
        let dbs = open_node_dbs(dir.path()).expect("restart after the reseed");
        assert!(genesis_applied(&dbs).unwrap());
    }

    /// The probe reads at most one account row. Many accounts are seeded
    /// plus a malformed row sorting last: a scan (the old `all_accounts`)
    /// reaches it and errors; the probe never does.
    #[test]
    fn probe_does_not_scan_the_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        init_chain(&dbs);
        for i in 0u32..20_000 {
            let mut a = [0u8; 20];
            a[..4].copy_from_slice(&i.to_be_bytes());
            // An encoded account row (72 bytes: balance, nonce, code hash).
            dbs.state.put_cf_raw(CF_ACCOUNTS, &a, &[0u8; 72]).unwrap();
        }
        dbs.state
            .put_cf_raw(CF_ACCOUNTS, &[0xff; 21], b"not an account")
            .unwrap();
        assert!(dbs.state.all_accounts().is_err(), "a scan reaches the bad row");
        assert!(genesis_applied(&dbs).expect("probe read past the first row"));
    }

    #[test]
    fn restart_rule_table() {
        let d = Path::new("/data");
        let p = |s, c, chain| LayoutProbe {
            hotstuff_in_state_db: s,
            hotstuff_in_consensus_db: c,
            state_db_has_chain: chain,
        };
        // Fresh dir, and a restart on the new layout.
        assert_eq!(check_layout(p(false, false, false), d), Ok(()));
        assert_eq!(check_layout(p(false, true, true), d), Ok(()));
        // Everything else refuses.
        for probe in [
            p(true, false, true),
            p(true, true, true),
            p(true, false, false),
            p(false, true, false),
            p(false, false, true),
        ] {
            let err = check_layout(probe, d).expect_err(&format!("{probe:?} must refuse"));
            assert!(err.starts_with("refusing to start"), "{err}");
        }
    }
}
