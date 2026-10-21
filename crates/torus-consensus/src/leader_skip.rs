//! Dead-leader skip, b1 (docs/plans/dead-leader-skip.md §3, A1): the per-block attribution
//! input, from the committed block's justify QC.
//!
//! * At commit (`on_committed_block`, consensus thread) the block's justify QC is reduced to a
//!   [`LeaderEvidence`] and written to a NODE-LOCAL row (`leader_qc:` ‖ height, in
//!   `CF_CONSENSUS_META`, outside every hashed prefix) before the block is handed to execution.
//!   The QC is part of the committed block, so every replica writes the same evidence.
//! * Execution of that height ([`block_leader_rows`]) reads the row (also after a crash: the
//!   boot replay re-executes from durable rows and finds it), turns it into a
//!   [`BlockAttribution`] with leader(v) = `select_leader` over the INSTALLED set (members and
//!   exclusions from execution's own rows), and returns the hashed `leader_stats:` rows plus the
//!   delete of the evidence row, all flushed with the block's applied marker.

use std::collections::HashMap;
use std::sync::Mutex;

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::VerifyingKey;
use hotstuff_rs::hotstuff::types::{Phase, PhaseCertificate};
use hotstuff_rs::pacemaker::select_leader;
use hotstuff_rs::types::data_types::{Power, ViewNumber};
use hotstuff_rs::types::validator_set::ValidatorSet;
use torus_economics::leader_stats::{
    self, BlockAttribution, LeaderCursor, LeaderRow, MAX_ATTRIBUTED_GAP,
};
use torus_economics::StakingManager;
use torus_state::cf::CF_CONSENSUS_META;
use torus_state::StateBackend;

/// Node-local key prefix of the evidence rows (never hashed: not in `META_CONSENSUS_PREFIXES`).
pub const LEADER_QC_PREFIX: &[u8] = b"leader_qc:";

/// The evidence row's phase byte.
pub const QC_GENERIC: u8 = 0;
/// A phased-mode (validator-set update) certificate.
pub const QC_PHASED: u8 = 1;
/// The genesis QC (the first block's justify).
pub const QC_GENESIS: u8 = 2;

/// What execution needs of a committed block's justify QC.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct LeaderEvidence {
    pub view: u64,
    /// [`QC_GENERIC`], [`QC_PHASED`] or [`QC_GENESIS`].
    pub phase: u8,
    /// Per position of the signing validator set: whether the QC carries that signature.
    pub signers: Vec<bool>,
}

impl LeaderEvidence {
    /// The evidence of `justify`.
    pub fn of(justify: &PhaseCertificate) -> Self {
        let phase = if justify.is_genesis_pc() {
            QC_GENESIS
        } else if justify.phase == Phase::Generic {
            QC_GENERIC
        } else {
            QC_PHASED
        };
        LeaderEvidence {
            view: justify.view.int(),
            phase,
            signers: justify.signatures.iter().map(Option::is_some).collect(),
        }
    }
}

/// The evidence row key of `height`.
pub fn leader_qc_key(height: u64) -> Vec<u8> {
    let mut key = LEADER_QC_PREFIX.to_vec();
    key.extend_from_slice(&height.to_be_bytes());
    key
}

/// Write `height`'s evidence row (consensus thread, at commit, before dispatch).
pub fn persist_leader_evidence<B: StateBackend>(
    db: &B,
    height: u64,
    evidence: &LeaderEvidence,
) -> Result<(), String> {
    let bytes = borsh::to_vec(evidence).map_err(|e| e.to_string())?;
    db.put_cf_raw(CF_CONSENSUS_META, &leader_qc_key(height), &bytes)
        .map_err(|e| e.to_string())
}

/// Why a block's leader rows could not be computed (execution fail-stops on every one).
#[derive(Debug)]
pub enum LeaderSkipError {
    /// No evidence row for the height: never guess (another replica attributes it).
    MissingEvidence(u64),
    /// A read or decode fault.
    Fault(String),
}

impl std::fmt::Display for LeaderSkipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEvidence(h) => write!(f, "no leader evidence row for height {h}"),
            Self::Fault(e) => write!(f, "{e}"),
        }
    }
}

/// The installed set as hotstuff runs it (members at their powers, the installed exclusions),
/// cached by the raw bytes of the two rows it is built from (decompressing every key per block
/// would cost more than the block).
pub struct LeaderSet {
    key: Vec<u8>,
    set: ValidatorSet,
    address_of: HashMap<VerifyingKey, [u8; 20]>,
    by_position: Vec<[u8; 20]>,
}

/// Execution's cache of [`LeaderSet`] (one per execution context).
#[derive(Default)]
pub struct LeaderSetCache(Mutex<Option<LeaderSet>>);

/// Execution's dead-leader-skip state: the chain config (its activation hook) and the cache.
pub struct LeaderSkipExec {
    config: torus_types::ChainConfig,
    pub cache: LeaderSetCache,
}

impl LeaderSkipExec {
    pub fn new(config: &torus_types::ChainConfig) -> Self {
        LeaderSkipExec {
            config: config.clone(),
            cache: LeaderSetCache::default(),
        }
    }

    /// The parameters in force for `header` ([`torus_types::ChainConfig::dead_leader_skip`]).
    pub fn params(
        &self,
        header: &torus_types::TorusBlockHeader,
    ) -> Option<torus_types::DeadLeaderParams> {
        self.config.dead_leader_skip(header)
    }
}

/// A1: the attribution of a block whose justify QC is `evidence`, against `state` (the block's
/// pre-state).
pub fn attribute<T: StateBackend>(
    _state: &T,
    _evidence: &LeaderEvidence,
    _cache: &LeaderSetCache,
) -> Result<BlockAttribution, LeaderSkipError> {
    // RED stub (phase 2 test commit).
    Ok(BlockAttribution::default())
}

/// The leader-statistics rows of block `height` (A1) read through `state`, plus the delete of
/// its evidence row, read from `evidence_db` (the node-local row, written at commit).
pub fn block_leader_rows<T: StateBackend, E: StateBackend>(
    _state: &T,
    _evidence_db: &E,
    _height: u64,
    _epoch_length: u64,
    _cache: &LeaderSetCache,
) -> Result<Vec<LeaderRow>, LeaderSkipError> {
    // RED stub (phase 2 test commit).
    Ok(Vec::new())
}

#[allow(dead_code)]
fn _unused(_: LeaderCursor, _: Power, _: ViewNumber, _: u64) -> u64 {
    let _ = select_leader;
    let _ = leader_stats::leader_cursor::<torus_state::StateDb>;
    let _: Option<StakingManager> = None;
    MAX_ATTRIBUTED_GAP
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use hotstuff_rs::types::data_types::{BlockHeight, ChainID, CryptoHash, SignatureSet};
    use torus_state::StateDb;
    use torus_types::{PublicKey, ValidatorInfo};

    fn vk(n: u8) -> VerifyingKey {
        ed25519_dalek::SigningKey::from_bytes(&[n; 32]).verifying_key()
    }

    fn addr(n: u8) -> [u8; 20] {
        [n; 20]
    }

    /// A DB whose installed set is validators 1..=4 (equal power 100).
    fn db_with_set() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let set = torus_types::ValidatorSet {
            validators: (1..=4)
                .map(|n| ValidatorInfo {
                    address: Address::new(addr(n)),
                    pubkey: PublicKey(vk(n).to_bytes()),
                    power: 100,
                    commission_bps: 0,
                })
                .collect(),
            epoch: 0,
        };
        StakingManager::new(db.clone())
            .record_genesis_epoch_set(&set)
            .unwrap();
        (dir, db)
    }

    /// The hotstuff set and address map the attribution must use.
    fn hs_set() -> (ValidatorSet, HashMap<VerifyingKey, [u8; 20]>) {
        let mut set = ValidatorSet::new();
        let mut map = HashMap::new();
        for n in 1..=4 {
            set.put(&vk(n), Power::new(100));
            map.insert(vk(n), addr(n));
        }
        (set, map)
    }

    fn leader(view: u64) -> [u8; 20] {
        let (set, map) = hs_set();
        map[&select_leader(ViewNumber::new(view), &set)]
    }

    fn set_cursor(db: &StateDb, view: u64, skip_next: bool) {
        db.put_cf_raw(
            CF_CONSENSUS_META,
            b"leader_stats:cursor",
            &borsh::to_vec(&LeaderCursor { view, skip_next }).unwrap(),
        )
        .unwrap();
    }

    fn ev(view: u64, phase: u8, signers: Vec<bool>) -> LeaderEvidence {
        LeaderEvidence {
            view,
            phase,
            signers,
        }
    }

    /// The evidence of a PC: view, phase class and the signature bitmap.
    #[test]
    fn evidence_of_a_justify_qc() {
        let mut sigs = SignatureSet::new(4);
        let _ = &mut sigs;
        let pc = PhaseCertificate {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(9),
            block: CryptoHash::new([1; 32]),
            phase: Phase::Generic,
            signatures: SignatureSet::new(4),
        };
        assert_eq!(LeaderEvidence::of(&pc), ev(9, QC_GENERIC, vec![false; 4]));
        let decide = PhaseCertificate {
            phase: Phase::Decide,
            ..pc.clone()
        };
        assert_eq!(LeaderEvidence::of(&decide).phase, QC_PHASED);
        assert_eq!(
            LeaderEvidence::of(&PhaseCertificate::genesis_pc()).phase,
            QC_GENESIS
        );
        let _ = BlockHeight::new(0);
    }

    /// A1: every view strictly between the cursor and the QC view is a miss for its leader
    /// (leader(v) = select_leader over the installed set), the QC view is a success, the
    /// signature positions map to addresses in the set's order, the cursor moves.
    #[test]
    fn generic_qc_attributes_the_gap_and_the_success() {
        let (_d, db) = db_with_set();
        set_cursor(&db, 10, false);
        let (set, map) = hs_set();
        let signers: Vec<bool> = vec![true, false, true, true];
        let want_signers: Vec<[u8; 20]> = set
            .validators()
            .zip(&signers)
            .filter(|(_, s)| **s)
            .map(|(v, _)| map[v])
            .collect();
        let cache = LeaderSetCache::default();
        let attr = attribute(&db, &ev(14, QC_GENERIC, signers), &cache).unwrap();
        assert_eq!(attr.misses, vec![leader(11), leader(12), leader(13)]);
        assert_eq!(attr.success, Some(leader(14)));
        assert_eq!(attr.signers, want_signers);
        assert_eq!(
            attr.cursor,
            Some(LeaderCursor {
                view: 14,
                skip_next: false
            })
        );
    }

    /// The gap rule around validator-set updates: a phased-mode QC (Decide of an update block)
    /// attributes nothing and marks the next block's gap as skipped; the next (generic) QC then
    /// attributes no gap or success either, but its signatures count. The genesis QC attributes
    /// nothing. No cursor (first block): no gap.
    #[test]
    fn phased_and_genesis_qcs_skip_attribution() {
        let (_d, db) = db_with_set();
        let cache = LeaderSetCache::default();
        set_cursor(&db, 10, false);
        let a = attribute(&db, &ev(15, QC_PHASED, vec![true; 4]), &cache).unwrap();
        assert!(a.misses.is_empty() && a.success.is_none() && a.signers.is_empty());
        assert_eq!(
            a.cursor,
            Some(LeaderCursor {
                view: 15,
                skip_next: true
            })
        );
        set_cursor(&db, 15, true);
        let b = attribute(&db, &ev(18, QC_GENERIC, vec![true; 4]), &cache).unwrap();
        assert!(b.misses.is_empty() && b.success.is_none());
        assert_eq!(b.signers.len(), 4, "signatures of the first generic QC count");
        assert_eq!(b.cursor.unwrap().skip_next, false);

        let (_d2, fresh) = db_with_set();
        let g = attribute(&fresh, &ev(0, QC_GENESIS, vec![]), &cache).unwrap();
        assert!(g.misses.is_empty() && g.success.is_none());
        assert_eq!(g.cursor.unwrap().skip_next, true, "boot views are no slots");
        let first = attribute(&fresh, &ev(7, QC_GENERIC, vec![true; 4]), &cache).unwrap();
        assert!(first.misses.is_empty(), "no cursor: no gap");
        assert_eq!(first.success, Some(leader(7)));
    }

    /// A gap longer than MAX_ATTRIBUTED_GAP attributes only its last MAX_ATTRIBUTED_GAP views.
    #[test]
    fn long_gaps_are_capped() {
        let (_d, db) = db_with_set();
        set_cursor(&db, 5, false);
        let to = 5 + MAX_ATTRIBUTED_GAP + 100;
        let a = attribute(&db, &ev(to, QC_GENERIC, vec![]), &LeaderSetCache::default()).unwrap();
        assert_eq!(a.misses.len() as u64, MAX_ATTRIBUTED_GAP);
        assert_eq!(a.misses.last(), Some(&leader(to - 1)));
        assert_eq!(a.misses.first(), Some(&leader(to - MAX_ATTRIBUTED_GAP)));
        assert!(a.signers.is_empty(), "a bitmap of the wrong length maps nobody");
    }

    /// leader(v) honours the INSTALLED exclusions: an excluded validator is never a missed or
    /// successful leader while the eligible power is at least a quorum.
    #[test]
    fn installed_exclusions_change_the_attributed_leaders() {
        let (_d, db) = db_with_set();
        let x = leader(11);
        let excl = leader_stats::ExcludedMember {
            address: x,
            pubkey: (1..=4u8).find(|n| addr(*n) == x).map(|n| vk(n).to_bytes()).unwrap(),
        };
        db.put_cf_raw(
            CF_CONSENSUS_META,
            b"epoch_vset:lx_current",
            &borsh::to_vec(&vec![excl]).unwrap(),
        )
        .unwrap();
        set_cursor(&db, 10, false);
        let a = attribute(&db, &ev(30, QC_GENERIC, vec![]), &LeaderSetCache::default()).unwrap();
        assert_eq!(a.misses.len(), 19);
        assert!(!a.misses.contains(&x), "excluded: never a leader");
        assert_ne!(a.success, Some(x));
    }

    /// Execution's row set: the evidence row is required (missing = error, never a guess), and
    /// its delete rides the block's rows.
    #[test]
    fn block_rows_need_the_evidence_and_delete_it() {
        let (_d, db) = db_with_set();
        let cache = LeaderSetCache::default();
        assert!(matches!(
            block_leader_rows(&db, &db, 5, 4, &cache),
            Err(LeaderSkipError::MissingEvidence(5))
        ));
        persist_leader_evidence(&db, 5, &ev(3, QC_GENERIC, vec![true; 4])).unwrap();
        let rows = block_leader_rows(&db, &db, 5, 4, &cache).unwrap();
        assert!(rows.contains(&(leader_qc_key(5), None)), "{rows:?}");
        assert!(
            rows.iter().any(|(k, v)| k == b"leader_stats:cursor" && v.is_some()),
            "{rows:?}"
        );
    }
}
