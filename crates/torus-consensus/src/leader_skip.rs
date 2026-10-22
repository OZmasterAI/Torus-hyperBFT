//! Dead-leader skip, b1 (docs/plans/dead-leader-skip.md §3, A1): the per-block attribution
//! input, from the committed block's justify QC.
//!
//! * At commit (`on_committed_block`, consensus thread) the block's justify QC is reduced to a
//!   [`LeaderEvidence`] and carried in the height's commit manifest ([`manifest_value`], the
//!   hook's one durable write, consensus DB), then written to a NODE-LOCAL row (`leader_qc:` ‖
//!   height, in `CF_CONSENSUS_META`, outside every hashed prefix) in the block's header + body
//!   batch ([`put_leader_evidence`]) before the block is executed. A durable manifest or a
//!   durable header each imply durable evidence; a height parked from its manifest after a
//!   crash takes the evidence from it ([`split_manifest`]). The QC is part of the committed
//!   block, so every replica writes the same evidence.
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

/// The leader evidence envelope of a commit manifest, version 1:
/// `MANIFEST_EVIDENCE_V1` ‖ evidence length (u32 LE) ‖ borsh(evidence) ‖ datum.
///
/// The commit hook's only durable write is the manifest (consensus DB, the WAL of hotstuff's
/// delivered height), so the evidence rides in it: a durable manifest implies durable
/// evidence, and a parked height (boot, FIX 6 / F-1) gets its evidence from the manifest it is
/// parked from. A plain manifest is the datum verbatim (bincode of a `TorusBlock` or
/// `CompactBlock`, both starting with the header's height, u64 LE); read as that height, the
/// tag's last two bytes (`0xff 0xff`) put it at or above 2^64 - 2^48 (`0xffff01564543514c`
/// is the tag itself), so [`manifest_value`] refuses a plain manifest from
/// [`MAX_PLAIN_MANIFEST_HEIGHT`] on and the two never collide. Feature off (K = 0) writes
/// plain manifests, byte for byte as before; old manifests decode as plain.
pub const MANIFEST_EVIDENCE_V1: [u8; 8] = [b'L', b'Q', b'C', b'E', b'V', 1, 0xff, 0xff];

/// The prefix every envelope version shares (the version is byte 5).
fn is_envelope(value: &[u8]) -> bool {
    value.len() >= 8 && value[..5] == MANIFEST_EVIDENCE_V1[..5] && value[6..8] == [0xff, 0xff]
}

/// The first height a plain manifest is refused at: 2^48 (Codex P3 on 0f3f6524). Every height
/// whose LE bytes could spell an envelope prefix (`0xffff....` in the top two bytes) is at or
/// above 2^64 - 2^48, far above this bound; 2^48 heights are ~8,900 years at 1,000 blocks/s,
/// so no real chain reaches it, and a round power of two keeps the check one comparison.
pub const MAX_PLAIN_MANIFEST_HEIGHT: u64 = 1 << 48;

/// The manifest value of committed `height` for `datum`: the datum verbatim without
/// `evidence`, else the envelope. `Err` for a plain manifest at a height from
/// [`MAX_PLAIN_MANIFEST_HEIGHT`] on or a datum shaped like an envelope, which
/// [`split_manifest`] would read back as one (the caller writes no manifest, as on a failed
/// write; the envelope is unambiguous at any height).
pub fn manifest_value<'a>(
    height: u64,
    datum: &'a [u8],
    evidence: Option<&LeaderEvidence>,
) -> Result<std::borrow::Cow<'a, [u8]>, String> {
    let Some(evidence) = evidence else {
        if height >= MAX_PLAIN_MANIFEST_HEIGHT {
            return Err(format!(
                "plain commit manifest refused at height {height:#x} (>= 2^48 could read back as an evidence envelope)"
            ));
        }
        if is_envelope(datum) {
            return Err(format!(
                "plain commit manifest refused at height {height}: the datum starts with the evidence envelope tag"
            ));
        }
        return Ok(std::borrow::Cow::Borrowed(datum));
    };
    let ev = borsh::to_vec(evidence).expect("borsh into a Vec cannot fail");
    let mut out = Vec::with_capacity(12 + ev.len() + datum.len());
    out.extend_from_slice(&MANIFEST_EVIDENCE_V1);
    out.extend_from_slice(&(ev.len() as u32).to_le_bytes());
    out.extend_from_slice(&ev);
    out.extend_from_slice(datum);
    Ok(std::borrow::Cow::Owned(out))
}

/// A manifest value split into its leader evidence (`None`: a plain manifest) and its datum.
/// `Err` for an envelope of an unknown version or a truncated one.
pub fn split_manifest(value: &[u8]) -> Result<(Option<LeaderEvidence>, &[u8]), String> {
    if !is_envelope(value) {
        return Ok((None, value));
    }
    if value[5] != MANIFEST_EVIDENCE_V1[5] {
        return Err(format!("manifest evidence envelope version {}", value[5]));
    }
    let len = value
        .get(8..12)
        .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
        .ok_or("manifest evidence envelope: truncated length")?;
    let ev = value
        .get(12..12 + len)
        .ok_or("manifest evidence envelope: truncated evidence")?;
    let evidence = LeaderEvidence::try_from_slice(ev)
        .map_err(|e| format!("manifest evidence envelope: {e}"))?;
    Ok((Some(evidence), &value[12 + len..]))
}

/// Put `height`'s evidence row into `batch` (the commit-persist writer's header + body batch:
/// a durable header implies durable evidence, also once the manifest is pruned).
pub fn put_leader_evidence(
    batch: &mut rocksdb::WriteBatch,
    meta_cf: &rocksdb::ColumnFamily,
    height: u64,
    evidence: &LeaderEvidence,
) {
    let bytes = borsh::to_vec(evidence).expect("borsh into a Vec cannot fail");
    batch.put_cf(meta_cf, leader_qc_key(height), bytes);
}

/// Write `height`'s evidence row on its own (tests: plant the row a committed height's header
/// batch writes).
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
    state: &T,
    evidence: &LeaderEvidence,
    cache: &LeaderSetCache,
) -> Result<BlockAttribution, LeaderSkipError> {
    let mut attr = BlockAttribution {
        cursor: Some(LeaderCursor {
            view: evidence.view,
            skip_next: evidence.phase != QC_GENERIC,
        }),
        ..BlockAttribution::default()
    };
    // The gap rule: a phased-mode QC (a validator-set update's later phases ran in views that
    // are no leader slots) or the genesis QC attributes nothing.
    if evidence.phase != QC_GENERIC {
        return Ok(attr);
    }
    let previous = leader_stats::leader_cursor(state).map_err(fault)?;
    let mut guard = cache.0.lock().unwrap_or_else(|p| p.into_inner());
    let Some(set) = LeaderSet::load(state, &mut guard)? else {
        // No installed set (a DB not built from a genesis): nobody to attribute to.
        return Ok(attr);
    };
    // A generic QC certifies a block after the last update block, so it is signed by the
    // committed set, which is the installed set (its positions are the bitmap's).
    if evidence.signers.len() == set.by_position.len() {
        attr.signers = set
            .by_position
            .iter()
            .zip(&evidence.signers)
            .filter(|(_, signed)| **signed)
            .map(|(address, _)| *address)
            .collect();
    }
    match previous {
        Some(c) if c.skip_next || evidence.view <= c.view => {}
        Some(c) => {
            let from = (c.view + 1).max(evidence.view.saturating_sub(MAX_ATTRIBUTED_GAP));
            attr.misses = (from..evidence.view).map(|v| set.leader(v)).collect();
            attr.success = Some(set.leader(evidence.view));
        }
        None => attr.success = Some(set.leader(evidence.view)),
    }
    Ok(attr)
}

fn fault(e: torus_economics::EconomicsError) -> LeaderSkipError {
    LeaderSkipError::Fault(e.to_string())
}

impl LeaderSet {
    /// The installed set from `state` (cached by the bytes of its two rows); `None` without one.
    fn load<'a, T: StateBackend>(
        state: &T,
        slot: &'a mut Option<LeaderSet>,
    ) -> Result<Option<&'a LeaderSet>, LeaderSkipError> {
        let Some(members_raw) =
            torus_economics::epoch_plan::installed_set_raw(state).map_err(fault)?
        else {
            return Ok(None);
        };
        let excluded_raw = leader_stats::installed_leader_exclusions_raw(state).map_err(fault)?;
        let mut key = (members_raw.len() as u64).to_be_bytes().to_vec();
        key.extend_from_slice(&members_raw);
        key.extend_from_slice(excluded_raw.as_deref().unwrap_or_default());
        if slot.as_ref().is_none_or(|s| s.key != key) {
            let members =
                torus_economics::epoch_plan::decode_plan_members(&members_raw).map_err(fault)?;
            let excluded = leader_stats::installed_leader_exclusions(state).map_err(fault)?;
            let mut set = ValidatorSet::new();
            let mut address_of = HashMap::new();
            // As consensus builds the hotstuff updates from a plan: a key that does not decode
            // is in neither.
            for m in &members {
                if let Ok(vk) = VerifyingKey::from_bytes(&m.pubkey) {
                    set.put(&vk, Power::new(m.power));
                    address_of.insert(vk, m.address);
                }
            }
            for m in &excluded {
                if let Ok(vk) = VerifyingKey::from_bytes(&m.pubkey) {
                    set.exclude_leader(&vk);
                }
            }
            let by_position = set.validators().map(|vk| address_of[vk]).collect();
            *slot = Some(LeaderSet {
                key,
                set,
                address_of,
                by_position,
            });
        }
        let set = slot.as_ref().expect("filled above");
        if set.set.is_empty() || set.set.total_power().int() == 0 {
            return Ok(None);
        }
        Ok(Some(set))
    }

    /// leader(view): `select_leader` over the installed set, exclusions included.
    fn leader(&self, view: u64) -> [u8; 20] {
        self.address_of[&select_leader(ViewNumber::new(view), &self.set)]
    }
}

/// The leader-statistics rows of block `height` (A1) read through `state`, plus the delete of
/// its evidence row, read from `evidence_db` (the node-local row, written at commit).
pub fn block_leader_rows<T: StateBackend, E: StateBackend>(
    state: &T,
    evidence_db: &E,
    height: u64,
    epoch_length: u64,
    cache: &LeaderSetCache,
) -> Result<Vec<LeaderRow>, LeaderSkipError> {
    let key = leader_qc_key(height);
    let raw = evidence_db
        .get_cf_raw(CF_CONSENSUS_META, &key)
        .map_err(|e| LeaderSkipError::Fault(e.to_string()))?
        .ok_or(LeaderSkipError::MissingEvidence(height))?;
    let evidence = LeaderEvidence::try_from_slice(&raw)
        .map_err(|e| LeaderSkipError::Fault(format!("leader evidence row {height}: {e}")))?;
    let attr = attribute(state, &evidence, cache)?;
    let mut rows =
        leader_stats::attribution_rows(state, height, epoch_length, &attr).map_err(fault)?;
    rows.push((key, None));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use hotstuff_rs::types::data_types::{ChainID, CryptoHash, SignatureSet};
    use torus_economics::StakingManager;
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
        assert_eq!(
            b.signers.len(),
            4,
            "signatures of the first generic QC count"
        );
        assert!(!b.cursor.unwrap().skip_next);

        let (_d2, fresh) = db_with_set();
        let g = attribute(&fresh, &ev(0, QC_GENESIS, vec![]), &cache).unwrap();
        assert!(g.misses.is_empty() && g.success.is_none());
        assert!(g.cursor.unwrap().skip_next, "boot views are no slots");
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
        assert!(
            a.signers.is_empty(),
            "a bitmap of the wrong length maps nobody"
        );
    }

    /// leader(v) honours the INSTALLED exclusions: an excluded validator is never a missed or
    /// successful leader while the eligible power is at least a quorum.
    #[test]
    fn installed_exclusions_change_the_attributed_leaders() {
        let (_d, db) = db_with_set();
        let x = leader(11);
        let excl = leader_stats::ExcludedMember {
            address: x,
            pubkey: (1..=4u8)
                .find(|n| addr(*n) == x)
                .map(|n| vk(n).to_bytes())
                .unwrap(),
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

    /// The manifest envelope: with evidence it round-trips (evidence + datum); without, the
    /// manifest is the datum verbatim, and a plain datum (bincode of a block, height first)
    /// splits as no evidence + itself, at any height, `u64::MAX - 2^48` included (the highest
    /// height whose bytes come nearest the tag). Unknown versions and truncations are errors.
    #[test]
    fn manifest_envelope_round_trips_and_plain_manifests_pass_through() {
        let e = ev(9, QC_GENERIC, vec![true, false, true]);
        let datum = b"some committed datum".to_vec();
        let off = manifest_value(5, &datum, None).unwrap();
        assert!(matches!(off, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*off, datum.as_slice());
        let on = manifest_value(5, &datum, Some(&e)).unwrap();
        assert_eq!(on[..8], MANIFEST_EVIDENCE_V1);
        assert_eq!(
            split_manifest(&on).unwrap(),
            (Some(e.clone()), datum.as_slice())
        );
        assert_eq!(split_manifest(&datum).unwrap(), (None, datum.as_slice()));
        assert_eq!(split_manifest(&[]).unwrap(), (None, &[][..]));

        // A plain datum: bincode (fixint, LE) of a block, its header's height first. Heights
        // from 2^48 on are refused on the write side
        // (`plain_manifest_refuses_heights_that_collide_with_the_envelope_tag`).
        for height in [0u64, 1, 5, 1 << 40, u64::MAX - (1 << 48)] {
            let mut plain = bincode::serialize(&height).unwrap();
            plain.extend_from_slice(b"rest of the block");
            assert_eq!(
                split_manifest(&plain).unwrap(),
                (None, plain.as_slice()),
                "height {height}"
            );
        }

        let mut v2 = on.to_vec();
        v2[5] = 2;
        assert!(split_manifest(&v2).is_err(), "unknown version");
        assert!(split_manifest(&on[..10]).is_err(), "truncated length");
        assert!(split_manifest(&on[..14]).is_err(), "truncated evidence");
    }

    /// Codex P3 on 0f3f6524: a plain manifest's first 8 bytes are its height (u64 LE), so the
    /// heights whose bytes spell the tag would be read back as an envelope: 0xffff01564543514c
    /// splits as a version 1 envelope, 0xffff02564543514c is refused as version 2. The plain
    /// path refuses every height from 2^48 on (all colliding heights are >= 2^64 - 2^48), and
    /// any datum shaped like an envelope; heights below 2^48 pass, and with evidence the
    /// envelope is unambiguous at any height.
    #[test]
    fn plain_manifest_refuses_heights_that_collide_with_the_envelope_tag() {
        let plain_at = |height: u64| {
            let mut datum = bincode::serialize(&height).unwrap();
            datum.extend_from_slice(b"rest of the block");
            datum
        };
        let v1 = u64::from_le_bytes(MANIFEST_EVIDENCE_V1);
        assert_eq!(v1, 0xffff_0156_4543_514c, "fixture: the tag read as a height");
        let v2 = 0xffff_0256_4543_514c_u64;
        // The collision the guard prevents: written plain, these heights do not read back plain.
        assert!(
            !matches!(split_manifest(&plain_at(v1)), Ok((None, _))),
            "fixture: the v1 tag height is not read back as a plain manifest"
        );
        assert!(split_manifest(&plain_at(v2)).is_err(), "fixture: the v2 height reads as version 2");

        for height in [v1, v2, MAX_PLAIN_MANIFEST_HEIGHT, MAX_PLAIN_MANIFEST_HEIGHT + 1, u64::MAX] {
            assert!(
                manifest_value(height, &plain_at(height), None).is_err(),
                "plain manifest at height {height:#x} refused"
            );
        }
        let below = MAX_PLAIN_MANIFEST_HEIGHT - 1;
        let datum = plain_at(below);
        let value = manifest_value(below, &datum, None).unwrap();
        assert_eq!(&*value, datum.as_slice(), "below 2^48: the datum verbatim");
        assert_eq!(split_manifest(&value).unwrap(), (None, datum.as_slice()));

        // A datum shaped like an envelope is refused whatever height it claims.
        assert!(manifest_value(7, &plain_at(v1), None).is_err());

        // With evidence: the envelope, at the colliding heights too.
        let e = ev(9, QC_GENERIC, vec![true, false, true]);
        for height in [v1, v2, u64::MAX] {
            let datum = plain_at(height);
            let on = manifest_value(height, &datum, Some(&e)).unwrap();
            assert_eq!(split_manifest(&on).unwrap(), (Some(e.clone()), datum.as_slice()));
        }
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
            rows.iter()
                .any(|(k, v)| k == b"leader_stats:cursor" && v.is_some()),
            "{rows:?}"
        );
    }
}
