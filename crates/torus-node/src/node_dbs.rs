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
use torus_genesis::GenesisDigest;
use torus_state::{consensus_db_path, NativeDaStore, StateBackend, StateDb};

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
    let (app_state, vs_state) = genesis
        .to_hotstuff_genesis()
        .map_err(|e| format!("genesis hotstuff state: {e}"))?;
    hotstuff_rs::replica::Replica::initialize(dbs.kv_store(), app_state, vs_state);
    tracing::info!("consensus replica initialized with genesis validator set");
    Ok(true)
}

/// State-DB meta key (`cf_consensus_meta`, node-local, not in the running
/// hash): the [`GenesisDigest`] of the genesis file this data dir is bound to
/// (P0.0 phase 4), in [`GenesisDigest::encode`]'s form.
pub const META_GENESIS_DIGEST: &[u8] = b"genesis_file_digest";

/// How boot step 4 bound the genesis file to the data dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisBinding {
    /// Fresh data dir: genesis seeded, its digest stored.
    Fresh,
    /// Initialized data dir without a stored digest (first boot of a build
    /// with the check): the current file's digest stored, not verified
    /// against the genesis the dir was initialized with.
    Legacy,
    /// The file matches the stored digest.
    Matched,
}

/// The stored genesis digest, if any.
pub fn read_genesis_digest(state: &StateDb) -> Result<Option<GenesisDigest>, String> {
    state
        .get_cf_raw(CF_CONSENSUS_META, META_GENESIS_DIGEST)
        .map_err(|e| format!("read the stored genesis digest: {e}"))?
        .map(|b| GenesisDigest::decode(&b))
        .transpose()
}

fn store_genesis_digest(state: &StateDb, digest: &GenesisDigest) -> Result<(), String> {
    state
        .put_cf_raw(CF_CONSENSUS_META, META_GENESIS_DIGEST, &digest.encode())
        .and_then(|()| state.sync_wal())
        .map_err(|e| format!("store the genesis digest: {e}"))
}

/// The refusal for a genesis file whose digest is not the stored one.
pub fn genesis_changed_error(
    stored: &GenesisDigest,
    file: &GenesisDigest,
    genesis_path: &Path,
    data_dir: &Path,
) -> String {
    let changed = stored.changed_fields(file);
    let changed = if changed.is_empty() {
        "no single field identified".to_string()
    } else {
        changed.join(", ")
    };
    format!(
        "refusing to start: genesis file {path} differs from the genesis data dir {dir} was \
         initialized with (stored digest {stored}, file digest {file}); changed: {changed}. \
         The chain config (activations, timeouts, chain id, ...) is read from this file at every \
         boot but genesis is applied only once, so an edited file would change consensus \
         parameters on this node alone. Restore the original genesis file (formatting and key \
         order may differ), or start from a fresh data dir to run the new genesis.",
        path = genesis_path.display(),
        dir = data_dir.display(),
        stored = stored.digest,
        file = file.digest,
    )
}

/// What boot step 4 does with a genesis file, decided before anything is
/// written ([`check_genesis_binding`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenesisPlan {
    /// Fresh data dir: seed genesis, store the digest.
    Fresh,
    /// Initialized dir without a stored digest: store the file's.
    Legacy,
    /// The file matches the stored digest: nothing to write.
    Matched,
}

/// Boot step 4, the read-only half: an initialized data dir with a stored
/// digest refuses a file that digests differently. Writes nothing, so a
/// refused start leaves the data dir as it was (Codex review of phase 4:
/// every boot-time write waits for this check).
pub fn check_genesis_binding(
    dbs: &NodeDbs,
    file_digest: &GenesisDigest,
    genesis_path: &Path,
    data_dir: &Path,
) -> Result<GenesisPlan, String> {
    if !genesis_applied(dbs)? {
        return Ok(GenesisPlan::Fresh);
    }
    match read_genesis_digest(&dbs.state)? {
        Some(stored) if stored.digest == file_digest.digest => Ok(GenesisPlan::Matched),
        Some(stored) => Err(genesis_changed_error(&stored, file_digest, genesis_path, data_dir)),
        None => Ok(GenesisPlan::Legacy),
    }
}

/// Boot step 4, the writing half, after [`check_genesis_binding`] passed.
pub fn write_genesis_binding(
    dbs: &NodeDbs,
    genesis: &torus_genesis::Genesis,
    file_digest: &GenesisDigest,
    plan: GenesisPlan,
) -> Result<GenesisBinding, String> {
    match plan {
        GenesisPlan::Matched => Ok(GenesisBinding::Matched),
        GenesisPlan::Legacy => {
            store_genesis_digest(&dbs.state, file_digest)?;
            Ok(GenesisBinding::Legacy)
        }
        GenesisPlan::Fresh => {
            apply_genesis_if_fresh(dbs, genesis)?;
            test_crash_point()?;
            store_genesis_digest(&dbs.state, file_digest)?;
            test_crash_point()?;
            Ok(GenesisBinding::Fresh)
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: the boot "crashes" (returns `Err`) after this many more
    /// genesis write steps.
    static CRASH_AFTER_STEPS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Test seam: one genesis write step is done (see `CRASH_AFTER_STEPS`).
fn test_crash_point() -> Result<(), String> {
    #[cfg(test)]
    if let Some(n) = CRASH_AFTER_STEPS.get() {
        if n <= 1 {
            CRASH_AFTER_STEPS.set(None);
            return Err("test crash".into());
        }
        CRASH_AFTER_STEPS.set(Some(n - 1));
    }
    Ok(())
}

/// Boot step 4 with a genesis file (P0.0 phase 4): an initialized data dir
/// with a stored digest refuses a file that digests differently (before
/// anything is written); one without a stored digest stores the file's
/// ([`GenesisBinding::Legacy`]); a fresh one is seeded
/// ([`apply_genesis_if_fresh`]) and then stores it.
pub fn init_genesis(
    dbs: &NodeDbs,
    genesis: &torus_genesis::Genesis,
    file_digest: &GenesisDigest,
    genesis_path: &Path,
    data_dir: &Path,
) -> Result<GenesisBinding, String> {
    let plan = check_genesis_binding(dbs, file_digest, genesis_path, data_dir)?;
    write_genesis_binding(dbs, genesis, file_digest, plan)
}

/// The `--genesis` file as boot step 4 reads it.
pub struct GenesisFile<'a> {
    pub genesis: &'a torus_genesis::Genesis,
    pub digest: &'a GenesisDigest,
    pub path: &'a Path,
}

/// Boot steps 3b and 4: the genesis binding check first
/// ([`check_genesis_binding`], or [`check_no_genesis_file`] without a genesis
/// file), then the history-retention rule ([`enforce_history_retention`],
/// which records the retention mode), then the genesis writes
/// ([`write_genesis_binding`]). A start refused for its genesis writes
/// nothing (Codex review of phase 4, P2: the retention marker written before
/// the refusal let the next default start prune an archive dir). `Ok(None)`:
/// no genesis file.
pub fn bind_genesis_and_retention(
    dbs: &NodeDbs,
    file: Option<GenesisFile<'_>>,
    retention: Option<u64>,
    explicit: bool,
    data_dir: &Path,
) -> Result<Option<GenesisBinding>, String> {
    let plan = match &file {
        Some(f) => Some(check_genesis_binding(dbs, f.digest, f.path, data_dir)?),
        None => {
            check_no_genesis_file(dbs, data_dir)?;
            None
        }
    };
    enforce_history_retention(&dbs.state, retention, explicit, data_dir)?;
    match (file, plan) {
        (Some(f), Some(plan)) => write_genesis_binding(dbs, f.genesis, f.digest, plan).map(Some),
        _ => Ok(None),
    }
}

/// Boot step 4 without a genesis file: a data dir bound to a genesis file
/// refuses (the default chain config would replace that genesis' config).
pub fn check_no_genesis_file(dbs: &NodeDbs, data_dir: &Path) -> Result<(), String> {
    match read_genesis_digest(&dbs.state)? {
        Some(stored) => Err(format!(
            "refusing to start: data dir {dir} was initialized from a genesis file (digest \
             {digest}) but no --genesis was given; the default chain config would replace that \
             genesis' config. Start with the same genesis file.",
            dir = data_dir.display(),
            digest = stored.digest,
        )),
        None => Ok(()),
    }
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

/// State-DB meta key (`cf_consensus_meta`, ASCII like every state meta key):
/// this data dir last started with history retention on. Written at every
/// start with retention on (before any pruner runs), deleted at every archive
/// start. Covers a pruning dir whose tip crossed N before the first pruner
/// pass saved a cutoff (claude-review P1, s131).
pub const META_HISTORY_RETENTION: &[u8] = b"history_retention_on";

/// What the archive-upgrade rule reads from a data dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionProbe {
    /// Highest committed header in the state DB (0 = fresh dir).
    pub tip: u64,
    /// The saved state-pruner cutoff (0 = never pruned).
    pub pruned_up_to: u64,
    /// [`META_HISTORY_RETENTION`] is set: the last start ran with retention on.
    pub ran_with_retention: bool,
}

/// Read the [`RetentionProbe`] of a state DB. The tip comes from the
/// fallible [`torus_rpc::try_find_latest_height`]: a header-read failure is
/// an `Err` (startup aborts), never tip 0, which would pass the upgrade rule
/// and record [`META_HISTORY_RETENTION`] on an unpruned archive dir (Codex
/// P2, s131).
pub fn probe_retention(state: &StateDb) -> Result<RetentionProbe, String> {
    probe_retention_with_tip(state, torus_rpc::try_find_latest_height(state))
}

/// [`probe_retention`] over an already-read tip (the seam the tests inject a
/// header-read error through).
fn probe_retention_with_tip(
    state: &StateDb,
    tip: Result<u64, String>,
) -> Result<RetentionProbe, String> {
    Ok(RetentionProbe {
        tip: tip.map_err(|e| {
            format!("cannot read the block-header tip for the history-retention check: {e}")
        })?,
        pruned_up_to: torus_state::read_pruned_up_to(state)
            .map_err(|e| format!("read the saved prune cutoff: {e}"))?,
        ran_with_retention: state
            .get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION)
            .map_err(|e| format!("probe {}: {e}", String::from_utf8_lossy(META_HISTORY_RETENTION)))?
            .is_some(),
    })
}

/// The archive-upgrade rule (claude-review P1, s131). Prune by default made a
/// flagless start keep the last N blocks; on an existing archive data dir the
/// first state-pruner pass would delete every block below `tip - N`, the
/// block tree would follow, and old DA bodies would stay for good (the DA GC
/// starts as on a fresh store): irreversible, and nobody chose it. So a start
/// with the DEFAULT retention (`explicit == false`) on a dir that has no saved
/// prune cutoff, never ran with retention, and whose tip is past N refuses,
/// naming both opt-outs. `--archive` / `--retention-blocks N` (or the config
/// keys) are an explicit choice and always start. `Err` is the refusal.
pub fn check_retention_upgrade(
    p: RetentionProbe,
    retention: Option<u64>,
    explicit: bool,
    data_dir: &Path,
) -> Result<(), String> {
    let Some(n) = retention else {
        return Ok(());
    };
    if explicit || p.pruned_up_to > 0 || p.ran_with_retention || p.tip <= n {
        return Ok(());
    }
    let cut = p.tip - n;
    Err(format!(
        "refusing to start: data dir {dir} holds unpruned history up to height {tip} (no prune \
         cutoff saved, never run with history retention), and this build prunes by default \
         (keep the last {n} blocks): the first pruner pass would delete every block below \
         {cut}, irreversibly. Choose explicitly: --archive (or `archive = true` in the config \
         file) keeps every block, as before; --retention-blocks N (or `retention-blocks = N`) \
         keeps the last N blocks (--retention-blocks {n} accepts the default). See \
         docs/pruning-guide.md, \"Upgrading an archive node\".",
        dir = data_dir.display(),
        tip = p.tip,
    ))
}

/// Boot step 3b: apply [`check_retention_upgrade`] to the state DB, then
/// record the retention mode ([`META_HISTORY_RETENTION`]: set with retention
/// on, deleted for an archive start) before any pruner runs.
pub fn enforce_history_retention(
    state: &StateDb,
    retention: Option<u64>,
    explicit: bool,
    data_dir: &Path,
) -> Result<(), String> {
    enforce_with_probe(state, probe_retention(state), retention, explicit, data_dir)
}

/// [`enforce_history_retention`] over an already-read probe. A failed probe
/// returns before anything is written.
fn enforce_with_probe(
    state: &StateDb,
    probe: Result<RetentionProbe, String>,
    retention: Option<u64>,
    explicit: bool,
    data_dir: &Path,
) -> Result<(), String> {
    check_retention_upgrade(probe?, retention, explicit, data_dir)?;
    match retention {
        Some(_) => state.put_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION, &[1]),
        None => state.delete_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION),
    }
    .map_err(|e| format!("record the history-retention mode: {e}"))
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

    // ---- Archive-upgrade rule (claude-review P1, s131) ----

    /// A state DB with headers, bodies and receipts at heights 0..=tip, all
    /// applied: what an archive node's data dir holds.
    fn history_db(dir: &Path, tip: u64) -> StateDb {
        use torus_state::cf::{CF_BLOCK_BODIES, META_NATIVE_APPLIED_HEIGHT};
        let db = StateDb::open(dir).unwrap();
        for h in 0..=tip {
            let k = h.to_be_bytes();
            db.put_cf_raw(CF_BLOCK_HEADERS, &k, b"header").unwrap();
            db.put_cf_raw(CF_BLOCK_BODIES, &k, b"body").unwrap();
        }
        db.put_cf_raw(CF_CONSENSUS_META, META_NATIVE_APPLIED_HEIGHT, &tip.to_be_bytes())
            .unwrap();
        db
    }

    fn body_exists(db: &StateDb, h: u64) -> bool {
        db.get_cf_raw(torus_state::cf::CF_BLOCK_BODIES, &h.to_be_bytes())
            .unwrap()
            .is_some()
    }

    /// An archive data dir (no prune cutoff saved, never run with retention)
    /// whose tip is past N, restarted on the prune-by-default build with no
    /// flag: refused, naming both opt-outs, and nothing is written or pruned.
    #[test]
    fn archive_data_dir_past_n_without_a_flag_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 20);
        let err = enforce_history_retention(&db, Some(10), false, dir.path())
            .expect_err("an archive data dir past N must be refused");
        assert!(err.starts_with("refusing to start"), "{err}");
        assert!(err.contains("--archive"), "{err}");
        assert!(err.contains("--retention-blocks"), "{err}");
        assert!(err.contains("archive = true"), "{err}");
        assert!(err.contains(&dir.path().display().to_string()), "{err}");
        assert!(
            db.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_none(),
            "a refused start records nothing"
        );
        assert!(body_exists(&db, 0));
    }

    /// The same dir with `--archive` (retention `None`) starts and stays an
    /// archive dir: no retention marker, so a later flagless start is still
    /// refused.
    #[test]
    fn archive_data_dir_with_archive_flag_starts() {
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 20);
        enforce_history_retention(&db, None, true, dir.path()).expect("--archive starts");
        assert!(db.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_none());
        assert!(enforce_history_retention(&db, Some(10), false, dir.path()).is_err());
    }

    /// The same dir with an explicit `--retention-blocks N` starts, records
    /// the marker, and the state pruner then prunes below tip - N.
    #[test]
    fn archive_data_dir_with_explicit_retention_starts_and_prunes() {
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 20);
        enforce_history_retention(&db, Some(10), true, dir.path())
            .expect("an explicit --retention-blocks N starts");
        assert!(db.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_some());
        let config = torus_state::PrunerConfig {
            retention_blocks: 10,
            prune_interval_blocks: 1,
            ..torus_state::PrunerConfig::default()
        };
        let mut pruner = torus_state::StatePruner::new(db.clone(), config, Arc::new(AtomicU64::new(0)));
        pruner.maybe_prune(20).unwrap();
        assert_eq!(pruner.pruned_height(), 10);
        assert!(!body_exists(&db, 0), "pruned below tip - N");
        assert!(body_exists(&db, 10));
        // Restart with no flag: the cutoff (and the marker) say it already prunes.
        enforce_history_retention(&db, Some(10), false, dir.path())
            .expect("a pruning data dir restarts on the default");
    }

    /// A data dir that already ran with retention on starts on the default
    /// even before its first prune pass saved a cutoff (tip just crossed N).
    #[test]
    fn data_dir_that_ran_with_retention_restarts_on_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 5);
        enforce_history_retention(&db, Some(10), false, dir.path()).expect("tip below N");
        assert!(db.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_some());
        drop(db);
        let db = history_db(dir.path(), 20);
        enforce_history_retention(&db, Some(10), false, dir.path())
            .expect("the marker says this dir already ran with retention on");
    }

    /// Codex P2 (s131): a header-read error in the upgrade probe must abort
    /// the start, never read as tip 0. An archive dir past N whose tip read
    /// fails is refused with a clear error, and the retention marker is NOT
    /// written (it would let a later healthy start skip the safeguard).
    #[test]
    fn header_read_error_in_the_upgrade_probe_aborts_and_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 20);
        let probe = probe_retention_with_tip(&db, Err("injected iterator error".into()));
        let err = enforce_with_probe(&db, probe, Some(10), false, dir.path())
            .expect_err("an unreadable tip must abort the start");
        assert!(err.contains("block-header tip"), "names what failed: {err}");
        assert!(err.contains("injected iterator error"), "carries the cause: {err}");
        assert!(
            db.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_none(),
            "an aborted start records nothing"
        );
        // The same error aborts on a young dir too (the tip is unknown, not 0),
        // and an explicit choice does not paper over it either.
        let young = tempfile::tempdir().unwrap();
        let ydb = history_db(young.path(), 3);
        let probe = probe_retention_with_tip(&ydb, Err("injected".into()));
        assert!(enforce_with_probe(&ydb, probe, Some(10), false, young.path()).is_err());
        let probe = probe_retention_with_tip(&ydb, Err("injected".into()));
        assert!(enforce_with_probe(&ydb, probe, Some(10), true, young.path()).is_err());
        assert!(ydb.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_none());
        // Healthy probe afterwards: the safeguard still refuses the archive dir.
        assert!(enforce_history_retention(&db, Some(10), false, dir.path()).is_err());
    }

    /// The healthy fallible probe reads the real tip (past the pruner's meta
    /// key) and a fresh / young dir starts and is marked.
    #[test]
    fn healthy_probe_reads_the_tip_and_young_dirs_start_marked() {
        let dir = tempfile::tempdir().unwrap();
        let db = history_db(dir.path(), 20);
        db.put_cf_raw(CF_BLOCK_HEADERS, b"__prune_meta__", b"x").unwrap();
        assert_eq!(probe_retention(&db).unwrap().tip, 20);
        let fresh = tempfile::tempdir().unwrap();
        let fdb = StateDb::open(fresh.path()).unwrap();
        assert_eq!(probe_retention(&fdb).unwrap().tip, 0);
        enforce_history_retention(&fdb, Some(10), false, fresh.path()).expect("fresh dir starts");
        assert!(fdb.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_some());
        let young = tempfile::tempdir().unwrap();
        let ydb = history_db(young.path(), 10);
        enforce_history_retention(&ydb, Some(10), false, young.path()).expect("tip == N starts");
        assert!(ydb.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap().is_some());
    }

    #[test]
    fn retention_upgrade_rule_table() {
        let d = Path::new("/data");
        let p = |tip, pruned_up_to, ran_with_retention| RetentionProbe {
            tip,
            pruned_up_to,
            ran_with_retention,
        };
        // Fresh / short chains, archive, explicit retention, already pruning.
        assert_eq!(check_retention_upgrade(p(0, 0, false), Some(10), false, d), Ok(()));
        assert_eq!(check_retention_upgrade(p(10, 0, false), Some(10), false, d), Ok(()));
        assert_eq!(check_retention_upgrade(p(50, 0, false), None, false, d), Ok(()));
        assert_eq!(check_retention_upgrade(p(50, 0, false), Some(10), true, d), Ok(()));
        assert_eq!(check_retention_upgrade(p(50, 7, false), Some(10), false, d), Ok(()));
        assert_eq!(check_retention_upgrade(p(50, 0, true), Some(10), false, d), Ok(()));
        // Past N, never pruned, never ran with retention, defaulted: refused.
        let err = check_retention_upgrade(p(11, 0, false), Some(10), false, d).unwrap_err();
        assert!(err.contains("below 1"), "names the first cutoff: {err}");
    }

    // ---- P0.0 phase 4: genesis digest binding ----

    const DEVNET_GENESIS: &str = include_str!("../../../devnet/genesis.json");

    /// A genesis file text: parsed genesis + digest, as `run` loads it.
    fn genesis_file(text: &str) -> (torus_genesis::Genesis, GenesisDigest) {
        (
            torus_genesis::Genesis::from_json(text).unwrap(),
            GenesisDigest::of_json(text).unwrap(),
        )
    }

    fn devnet_with(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(DEVNET_GENESIS).unwrap();
        edit(&mut v);
        serde_json::to_string_pretty(&v).unwrap()
    }

    fn boot(dir: &Path, text: &str) -> Result<GenesisBinding, String> {
        let dbs = open_node_dbs(dir).unwrap();
        let (g, d) = genesis_file(text);
        init_genesis(&dbs, &g, &d, Path::new("/etc/torus/genesis.json"), dir)
    }

    fn stored(dir: &Path) -> Option<GenesisDigest> {
        read_genesis_digest(&StateDb::open(dir).unwrap()).unwrap()
    }

    #[test]
    fn fresh_boot_stores_the_genesis_digest() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Fresh));
        let dbs = open_node_dbs(dir.path()).unwrap();
        assert!(genesis_applied(&dbs).unwrap(), "fresh boot seeded genesis");
        assert_eq!(
            read_genesis_digest(&dbs.state).unwrap(),
            Some(GenesisDigest::of_json(DEVNET_GENESIS).unwrap())
        );
    }

    #[test]
    fn reboot_with_the_same_genesis_starts() {
        let dir = tempfile::tempdir().unwrap();
        boot(dir.path(), DEVNET_GENESIS).unwrap();
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Matched));
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Matched));
    }

    #[test]
    fn changed_consensus_field_refuses_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        boot(dir.path(), DEVNET_GENESIS).unwrap();
        let before = stored(dir.path());
        let edited = devnet_with(|v| {
            v["consensus"]["activations"] = serde_json::json!({ "running_state_hash": 500 });
        });
        let err = boot(dir.path(), &edited).unwrap_err();
        assert!(
            err.starts_with("refusing to start: genesis file /etc/torus/genesis.json"),
            "{err}"
        );
        assert!(err.contains("changed: consensus.activations."), "names the field: {err}");
        assert!(err.contains(&dir.path().display().to_string()), "names the data dir: {err}");
        let t = devnet_with(|v| {
            let t = v["consensus"]["timeout_base_ms"].as_u64().unwrap();
            v["consensus"]["timeout_base_ms"] = (t + 1).into();
            v["chain_id"] = 9999u64.into();
        });
        let err = boot(dir.path(), &t).unwrap_err();
        assert!(err.contains("changed: chain_id, consensus.timeout_base_ms."), "{err}");
        assert_eq!(stored(dir.path()), before, "a refused boot rewrote the digest");
        // The original file still starts.
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Matched));
    }

    #[test]
    fn formatting_only_change_still_starts() {
        let dir = tempfile::tempdir().unwrap();
        boot(dir.path(), DEVNET_GENESIS).unwrap();
        let v: serde_json::Value = serde_json::from_str(DEVNET_GENESIS).unwrap();
        let compact = serde_json::to_string(&v).unwrap();
        assert_ne!(compact, DEVNET_GENESIS);
        assert_eq!(boot(dir.path(), &compact), Ok(GenesisBinding::Matched));
        let pretty = serde_json::to_string_pretty(&v).unwrap();
        let indented = format!("\n\n{}\n", pretty.replace("  ", "\t"));
        assert_eq!(boot(dir.path(), &indented), Ok(GenesisBinding::Matched));
    }

    #[test]
    fn legacy_data_dir_without_a_digest_stores_one_and_starts() {
        let dir = tempfile::tempdir().unwrap();
        {
            // What the previous build left: genesis applied, no digest.
            let dbs = open_node_dbs(dir.path()).unwrap();
            assert!(apply_genesis_if_fresh(&dbs, &genesis()).unwrap());
        }
        assert_eq!(stored(dir.path()), None);
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Legacy));
        assert_eq!(
            stored(dir.path()),
            Some(GenesisDigest::of_json(DEVNET_GENESIS).unwrap())
        );
        // From now on the dir is bound.
        assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Matched));
        let edited = devnet_with(|v| v["chain_name"] = "edited".into());
        assert!(boot(dir.path(), &edited).unwrap_err().contains("changed: chain_name."));
    }

    /// Codex review of phase 4 (P2): a start refused for a changed genesis
    /// writes nothing, in particular not the history-retention marker. An
    /// explicit-retention start with the wrong genesis on an archive dir
    /// must not let the next default start (which would prune) through.
    #[test]
    fn a_changed_genesis_refusal_writes_no_retention_marker() {
        let dir = tempfile::tempdir().unwrap();
        boot(dir.path(), DEVNET_GENESIS).unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        for h in 0..=50u64 {
            dbs.state.put_cf_raw(CF_BLOCK_HEADERS, &h.to_be_bytes(), b"header").unwrap();
        }
        let marker = |dbs: &NodeDbs| {
            dbs.state.get_cf_raw(CF_CONSENSUS_META, META_HISTORY_RETENTION).unwrap()
        };
        assert_eq!(marker(&dbs), None, "an archive dir");
        let edited_text = devnet_with(|v| v["chain_name"] = "edited".into());
        let (edited, edited_digest) = genesis_file(&edited_text);
        let path = Path::new("/etc/torus/genesis.json");
        let file = |g, d| Some(GenesisFile { genesis: g, digest: d, path });
        let err = bind_genesis_and_retention(
            &dbs,
            file(&edited, &edited_digest),
            Some(10),
            true,
            dir.path(),
        )
        .unwrap_err();
        assert!(err.contains("changed: chain_name."), "{err}");
        assert_eq!(marker(&dbs), None, "the refused start recorded the retention mode");
        // The archive protection still holds for a defaulted start.
        let (original, digest) = genesis_file(DEVNET_GENESIS);
        let err = bind_genesis_and_retention(&dbs, file(&original, &digest), Some(10), false, dir.path())
            .unwrap_err();
        assert!(err.contains("holds unpruned history"), "{err}");
        assert_eq!(marker(&dbs), None);
        // The no-genesis refusal of a bound dir writes nothing either.
        assert!(bind_genesis_and_retention(&dbs, None, Some(10), true, dir.path()).is_err());
        assert_eq!(marker(&dbs), None);
    }

    /// Codex review of phase 4 (P2): a fresh boot that crashes between its
    /// genesis writes leaves a data dir that is bound to the file it started
    /// with: a restart with an edited file refuses (it was accepted as a
    /// legacy dir when the genesis was seeded before the digest was stored),
    /// and the original file finishes the boot.
    #[test]
    fn a_crash_inside_the_fresh_genesis_boot_keeps_the_binding() {
        let edited = devnet_with(|v| v["chain_name"] = "edited".into());
        for steps in 1..=2 {
            let dir = tempfile::tempdir().unwrap();
            CRASH_AFTER_STEPS.set(Some(steps));
            assert_eq!(boot(dir.path(), DEVNET_GENESIS), Err("test crash".into()), "{steps}");
            let err = boot(dir.path(), &edited).expect_err("an edited file after the crash");
            assert!(err.contains("changed: chain_name."), "crash after {steps} steps: {err}");
            let done = boot(dir.path(), DEVNET_GENESIS).expect("the original file starts");
            assert!(
                matches!(done, GenesisBinding::Fresh | GenesisBinding::Matched),
                "{steps}: {done:?}"
            );
            assert_eq!(boot(dir.path(), DEVNET_GENESIS), Ok(GenesisBinding::Matched));
            assert!(genesis_applied(&open_node_dbs(dir.path()).unwrap()).unwrap());
        }
    }

    #[test]
    fn no_genesis_file_on_a_bound_dir_refuses() {
        let dir = tempfile::tempdir().unwrap();
        {
            let dbs = open_node_dbs(dir.path()).unwrap();
            check_no_genesis_file(&dbs, dir.path()).expect("fresh dir without genesis starts");
        }
        boot(dir.path(), DEVNET_GENESIS).unwrap();
        let dbs = open_node_dbs(dir.path()).unwrap();
        let err = check_no_genesis_file(&dbs, dir.path()).unwrap_err();
        assert!(err.contains("no --genesis was given"), "{err}");
    }
}
