//! Dead-leader skip, b1 (docs/plans/dead-leader-skip.md §3): leader statistics and leader
//! exclusion through the epoch plan, decided at EXECUTION from committed blocks only.
//!
//! * A1 (per committed block, [`attribution_rows`]): the caller (torus-consensus) derives the
//!   block's [`BlockAttribution`] from its justify QC and the installed set; this module turns it
//!   into `leader_stats:` rows (missed-slot runs, successes, re-admission evidence, the cursor).
//! * A2 / A3 (per boundary, [`EpochManager::execute_leader_exclusions`]): the execution of
//!   boundary `H` installs the exclusion plan for `H` and writes the one for `H + L`
//!   ([`LeaderExclusionPlan`]), which consensus at `H + L` turns into the exclusion delta of the
//!   hotstuff validator-set update, exactly as [`crate::epoch_plan`] does for members.
//!
//! Every row is consensus state in `CF_CONSENSUS_META`, hashed by the running hash
//! (`leader_stats:` and `epoch_vset:` are in `META_CONSENSUS_PREFIXES`). No input is local:
//! no clock, no locally observed event. Voting power and quorums are never touched (jailing is
//! not part of b1).

use std::collections::{BTreeMap, BTreeSet};

use borsh::{BorshDeserialize, BorshSerialize};
use torus_state::cf::CF_CONSENSUS_META;
use torus_state::StateBackend;
use torus_types::DeadLeaderParams;

use crate::epoch::EpochManager;
use crate::epoch_plan::PlanMember;
use crate::error::EconomicsError;
use crate::staking::StakingManager;

type Result<T> = std::result::Result<T, EconomicsError>;

/// Key prefix of the per-block leader statistics rows (consensus state, hashed).
pub const LEADER_STATS_PREFIX: &[u8] = b"leader_stats:";
/// The QC view of the last executed block and whether its successor's gap is skipped.
const CURSOR_KEY: &[u8] = b"leader_stats:cursor";
/// Sorted addresses in the exclusion lifecycle (planned or installed, not yet re-admitted).
const EXCLUDED_KEY: &[u8] = b"leader_stats:excluded";
/// `leader_stats:v:` ‖ address: one validator's [`LeaderStats`].
const VALIDATOR_PREFIX: &[u8] = b"leader_stats:v:";
/// `epoch_vset:lx_plan:` ‖ boundary (BE): the [`LeaderExclusionPlan`] taking effect there.
const LX_PLAN_PREFIX: &[u8] = b"epoch_vset:lx_plan:";
/// The exclusions installed in hotstuff by the last applied boundary.
const LX_CURRENT_KEY: &[u8] = b"epoch_vset:lx_current";

/// A1: at most this many views of one gap are attributed (the last ones). A stall of thousands
/// of views attributes a bounded amount of work; a run of K misses needs far fewer.
pub const MAX_ATTRIBUTED_GAP: u64 = 4096;

/// One row of a block's leader-statistics write set: a put (`Some`) or a delete (`None`) in
/// `CF_CONSENSUS_META`.
pub type LeaderRow = (Vec<u8>, Option<Vec<u8>>);

/// The QC view the last executed block carried.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaderCursor {
    pub view: u64,
    /// The next block's gap and success are not attributed: this QC was a phased-mode
    /// (validator-set update) certificate or the genesis QC, so the views up to the next QC are
    /// not one leader slot each (design A1, the gap rule around validator-set updates).
    pub skip_next: bool,
}

/// One validator's leader statistics.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LeaderStats {
    /// Consecutive missed leader slots (A1).
    pub miss_run: u32,
    /// The exclusion count j, capped; reset by a success.
    pub j: u32,
    /// The boundary from which the current exclusion is in force (`None`: not excluded).
    pub excluded_at: Option<u64>,
    /// A3: the first boundary at which a re-admission may take effect (`excluded_at + L * 2^j`).
    pub readmit_after: u64,
    /// A3: the last height (at most one per epoch) whose committed QC carried this validator's
    /// signature while it was excluded.
    pub seen_at: Option<u64>,
}

/// A validator in an exclusion set, by address and consensus key.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExcludedMember {
    pub address: [u8; 20],
    pub pubkey: [u8; 32],
}

/// The leader exclusions that take effect at `boundary`, computed by the execution of the
/// previous boundary. Written only when the hotstuff exclusion delta is non-empty.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct LeaderExclusionPlan {
    pub boundary: u64,
    /// The full excluded set from `boundary` on (address order).
    pub excluded: Vec<ExcludedMember>,
    /// The hotstuff delta: keys to exclude (sorted).
    pub exclude: Vec<[u8; 32]>,
    /// The hotstuff delta: keys to re-admit (sorted; members of the set at `boundary` only).
    pub readmit: Vec<[u8; 32]>,
}

/// What the execution of one committed block attributes (A1), derived by the caller from the
/// block's justify QC and the installed set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockAttribution {
    /// The cursor after this block (always written).
    pub cursor: Option<LeaderCursor>,
    /// The leader of every missed view of the gap, in view order (repeats allowed).
    pub misses: Vec<[u8; 20]>,
    /// The leader of the certified view.
    pub success: Option<[u8; 20]>,
    /// The validators whose signature the QC carries.
    pub signers: Vec<[u8; 20]>,
}

fn validator_key(address: &[u8; 20]) -> Vec<u8> {
    let mut key = VALIDATOR_PREFIX.to_vec();
    key.extend_from_slice(address);
    key
}

fn lx_plan_key(boundary: u64) -> Vec<u8> {
    let mut key = LX_PLAN_PREFIX.to_vec();
    key.extend_from_slice(&boundary.to_be_bytes());
    key
}

fn encode<B: BorshSerialize>(value: &B) -> Result<Vec<u8>> {
    borsh::to_vec(value).map_err(|e| EconomicsError::Borsh(e.to_string()))
}

fn decode<B: BorshDeserialize>(bytes: &[u8]) -> Result<B> {
    B::try_from_slice(bytes).map_err(|e| EconomicsError::Borsh(e.to_string()))
}

fn get<T: StateBackend, B: BorshDeserialize>(state: &T, key: &[u8]) -> Result<Option<B>> {
    match state.get_cf_raw(CF_CONSENSUS_META, key)? {
        Some(bytes) => Ok(Some(decode(&bytes)?)),
        None => Ok(None),
    }
}

/// The cursor (`None` before the first attributed block).
pub fn leader_cursor<T: StateBackend>(state: &T) -> Result<Option<LeaderCursor>> {
    get(state, CURSOR_KEY)
}

/// One validator's statistics (default when it has no row).
pub fn leader_stats<T: StateBackend>(state: &T, address: &[u8; 20]) -> Result<LeaderStats> {
    Ok(get(state, &validator_key(address))?.unwrap_or_default())
}

/// The addresses in the exclusion lifecycle (planned or installed, not re-admitted), sorted.
pub fn leader_excluded_index<T: StateBackend>(state: &T) -> Result<Vec<[u8; 20]>> {
    Ok(get(state, EXCLUDED_KEY)?.unwrap_or_default())
}

/// The exclusions installed in hotstuff by the last applied boundary (address order).
pub fn installed_leader_exclusions<T: StateBackend>(state: &T) -> Result<Vec<ExcludedMember>> {
    Ok(get(state, LX_CURRENT_KEY)?.unwrap_or_default())
}

/// The raw bytes of the installed exclusions row (a cache key for callers).
pub fn installed_leader_exclusions_raw<T: StateBackend>(state: &T) -> Result<Option<Vec<u8>>> {
    Ok(state.get_cf_raw(CF_CONSENSUS_META, LX_CURRENT_KEY)?)
}

/// The exclusion plan that takes effect at `boundary`, if one was written.
pub fn leader_exclusion_plan<T: StateBackend>(
    state: &T,
    boundary: u64,
) -> Result<Option<LeaderExclusionPlan>> {
    get(state, &lx_plan_key(boundary))
}

/// Store `plan` (the plan for its boundary).
pub fn put_leader_exclusion_plan<T: StateBackend>(
    state: &T,
    plan: &LeaderExclusionPlan,
) -> Result<()> {
    state.put_cf_raw(
        CF_CONSENSUS_META,
        &lx_plan_key(plan.boundary),
        &encode(plan)?,
    )?;
    Ok(())
}

/// The hotstuff quorum of `total` power (`total * 2 / 3 + 1`, hotstuff_rs `ValidatorSet::quorum`).
pub fn quorum_power(total: u128) -> u128 {
    total * 2 / 3 + 1
}

/// A1: the rows the execution of block `height` writes for `attr`, read against `state` (the
/// block's pre-state). Misses add to the missed-slot run of their leader, then the success
/// resets its leader's run and j; a signer in the exclusion lifecycle records `seen_at` (at most
/// once per epoch window `(H - L, H]`); the cursor is written. Rows that would not change are not
/// returned.
pub fn attribution_rows<T: StateBackend>(
    state: &T,
    height: u64,
    epoch_length: u64,
    attr: &BlockAttribution,
) -> Result<Vec<LeaderRow>> {
    // (pre-state, post-state) of every validator this block touches, in address order.
    let mut touched: BTreeMap<[u8; 20], (LeaderStats, LeaderStats)> = BTreeMap::new();
    fn entry<'a, T: StateBackend>(
        touched: &'a mut BTreeMap<[u8; 20], (LeaderStats, LeaderStats)>,
        state: &T,
        address: &[u8; 20],
    ) -> Result<&'a mut LeaderStats> {
        if !touched.contains_key(address) {
            let st = leader_stats(state, address)?;
            touched.insert(*address, (st.clone(), st));
        }
        Ok(&mut touched.get_mut(address).expect("inserted").1)
    }
    // The gap's views come before the certified one: misses first, then the success.
    for address in &attr.misses {
        let st = entry(&mut touched, state, address)?;
        st.miss_run = st.miss_run.saturating_add(1);
    }
    if let Some(address) = &attr.success {
        let st = entry(&mut touched, state, address)?;
        st.miss_run = 0;
        st.j = 0;
    }
    if !attr.signers.is_empty() {
        let index = leader_excluded_index(state)?;
        if !index.is_empty() {
            // A3 reads "seen in (H - L, H]" at boundary H: one record per such window.
            let window_start = match epoch_length {
                0 => 0,
                l => (height.saturating_sub(1) / l) * l,
            };
            for address in attr
                .signers
                .iter()
                .filter(|a| index.binary_search(a).is_ok())
            {
                let st = entry(&mut touched, state, address)?;
                if st.seen_at.is_none_or(|seen| seen <= window_start) {
                    st.seen_at = Some(height);
                }
            }
        }
    }
    let mut rows = Vec::new();
    for (address, (before, after)) in touched {
        if before != after {
            rows.push((validator_key(&address), Some(encode(&after)?)));
        }
    }
    if let Some(cursor) = attr.cursor {
        if leader_cursor(state)? != Some(cursor) {
            rows.push((CURSOR_KEY.to_vec(), Some(encode(&cursor)?)));
        }
    }
    Ok(rows)
}

/// Apply `rows` to `state` (puts and deletes in `CF_CONSENSUS_META`).
pub fn apply_leader_rows<T: StateBackend>(state: &T, rows: &[LeaderRow]) -> Result<()> {
    for (key, value) in rows {
        match value {
            Some(v) => state.put_cf_raw(CF_CONSENSUS_META, key, v)?,
            None => state.delete_cf_raw(CF_CONSENSUS_META, key)?,
        }
    }
    Ok(())
}

impl EpochManager {
    /// Execution of boundary `height`, after [`EpochManager::execute_planned_rotation`] (the
    /// member plan for `height` applied, the one for `height + epoch_length` written): prune the
    /// previous boundary's exclusion plan, install the one for `height` (or keep the installed
    /// exclusions of members), then with `params` (the feature on) write the plan for
    /// `height + epoch_length`: A3 re-admissions and A2 exclusions.
    pub fn execute_leader_exclusions<T: StateBackend>(
        staking: &StakingManager<T>,
        height: u64,
        epoch_length: u64,
        params: Option<DeadLeaderParams>,
    ) -> Result<()> {
        let state = staking.state();
        // Kept until the next boundary executes, as the member plan: consensus may re-read
        // plan(height) until then.
        if height >= epoch_length {
            let previous = lx_plan_key(height - epoch_length);
            if state.get_cf_raw(CF_CONSENSUS_META, &previous)?.is_some() {
                state.delete_cf_raw(CF_CONSENSUS_META, &previous)?;
            }
        }

        // Install: the plan for `height`, else the installed exclusions of the members that
        // stay (hotstuff drops a deleted key's exclusion with the key).
        let members_now = staking.current_epoch_set()?.unwrap_or_default();
        let before = installed_leader_exclusions(state)?;
        let installed = match leader_exclusion_plan(state, height)? {
            Some(plan) => plan.excluded,
            None => before
                .iter()
                .filter(|m| {
                    members_now
                        .iter()
                        .any(|x| x.address == m.address && x.pubkey == m.pubkey)
                })
                .copied()
                .collect(),
        };
        if installed != before {
            if installed.is_empty() {
                state.delete_cf_raw(CF_CONSENSUS_META, LX_CURRENT_KEY)?;
            } else {
                state.put_cf_raw(CF_CONSENSUS_META, LX_CURRENT_KEY, &encode(&installed)?)?;
            }
        }

        let Some(params) = params else {
            return Ok(());
        };
        let next = height.saturating_add(epoch_length);
        let Some(next_plan) = staking.epoch_rotation_plan(next)? else {
            return Ok(());
        };
        let members = &next_plan.members;
        let index = leader_excluded_index(state)?;
        let mut lifecycle: BTreeSet<[u8; 20]> = index.iter().copied().collect();
        let mut writes: BTreeMap<[u8; 20], LeaderStats> = BTreeMap::new();
        let mut kept: Vec<&PlanMember> = Vec::new();
        let mut candidates: Vec<(LeaderStats, &PlanMember)> = Vec::new();
        let window_floor = height.saturating_sub(epoch_length);
        for m in members {
            let st = leader_stats(state, &m.address)?;
            if st.excluded_at.is_some() {
                // A3: a committed QC of (H - L, H] carried its signature, and the floor
                // `excluded_at + L * 2^j` is reached when the re-admission takes effect.
                let seen = st.seen_at.is_some_and(|seen| seen > window_floor);
                if seen && next >= st.readmit_after {
                    writes.insert(
                        m.address,
                        LeaderStats {
                            excluded_at: None,
                            readmit_after: 0,
                            seen_at: None,
                            ..st
                        },
                    );
                    lifecycle.remove(&m.address);
                } else {
                    kept.push(m);
                }
            } else if st.miss_run >= params.miss_threshold {
                candidates.push((st, m));
            }
        }
        // A validator leaving the set leaves the exclusion lifecycle (hotstuff drops its
        // exclusion with its key).
        for address in &index {
            if !members.iter().any(|m| m.address == *address) {
                let st = leader_stats(state, address)?;
                writes.insert(
                    *address,
                    LeaderStats {
                        excluded_at: None,
                        readmit_after: 0,
                        seen_at: None,
                        ..st
                    },
                );
                lifecycle.remove(address);
            }
        }

        // A2: most misses first, then by address, while the eligible power stays >= quorum.
        candidates.sort_by(|(a, ma), (b, mb)| {
            b.miss_run
                .cmp(&a.miss_run)
                .then_with(|| ma.address.cmp(&mb.address))
        });
        let total: u128 = members.iter().map(|m| m.power as u128).sum();
        let quorum = quorum_power(total);
        let mut excluded_power: u128 = kept.iter().map(|m| m.power as u128).sum();
        let mut desired: Vec<ExcludedMember> = kept
            .iter()
            .map(|m| ExcludedMember {
                address: m.address,
                pubkey: m.pubkey,
            })
            .collect();
        for (st, m) in candidates {
            let after = excluded_power + m.power as u128;
            if total.saturating_sub(after) < quorum {
                continue;
            }
            excluded_power = after;
            let exponent = st.j.min(params.exclusion_cap);
            let floor = epoch_length.saturating_mul(1u64 << exponent.min(63));
            writes.insert(
                m.address,
                LeaderStats {
                    miss_run: 0,
                    j: st.j.saturating_add(1).min(params.exclusion_cap),
                    excluded_at: Some(next),
                    readmit_after: next.saturating_add(floor),
                    seen_at: None,
                },
            );
            lifecycle.insert(m.address);
            desired.push(ExcludedMember {
                address: m.address,
                pubkey: m.pubkey,
            });
        }
        desired.sort();

        for (address, st) in &writes {
            state.put_cf_raw(CF_CONSENSUS_META, &validator_key(address), &encode(st)?)?;
        }
        let lifecycle: Vec<[u8; 20]> = lifecycle.into_iter().collect();
        if lifecycle != index {
            if lifecycle.is_empty() {
                state.delete_cf_raw(CF_CONSENSUS_META, EXCLUDED_KEY)?;
            } else {
                state.put_cf_raw(CF_CONSENSUS_META, EXCLUDED_KEY, &encode(&lifecycle)?)?;
            }
        }

        // The hotstuff delta from the set installed now to `desired` at `next`.
        let installed_keys: BTreeSet<[u8; 32]> = installed.iter().map(|m| m.pubkey).collect();
        let member_keys: BTreeSet<[u8; 32]> = members.iter().map(|m| m.pubkey).collect();
        let desired_keys: BTreeSet<[u8; 32]> = desired.iter().map(|m| m.pubkey).collect();
        let exclude: Vec<[u8; 32]> = desired_keys.difference(&installed_keys).copied().collect();
        let readmit: Vec<[u8; 32]> = installed_keys
            .intersection(&member_keys)
            .filter(|k| !desired_keys.contains(*k))
            .copied()
            .collect();
        if !exclude.is_empty() || !readmit.is_empty() {
            put_leader_exclusion_plan(
                state,
                &LeaderExclusionPlan {
                    boundary: next,
                    excluded: desired,
                    exclude,
                    readmit,
                },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ValidatorState, ValidatorStatus};
    use alloy_primitives::{Address, U256};
    use torus_state::StateDb;

    const L: u64 = 4;
    const P: DeadLeaderParams = DeadLeaderParams {
        miss_threshold: 3,
        exclusion_cap: 6,
    };

    fn addr(n: u8) -> [u8; 20] {
        [n; 20]
    }

    fn pk(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// A DB whose installed (genesis) set is validators `1..=n` with `powers[i]` (whole TRS).
    fn setup(powers: &[u64]) -> (tempfile::TempDir, StakingManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = StakingManager::new(StateDb::open(dir.path()).unwrap());
        for (i, p) in powers.iter().enumerate() {
            let n = i as u8 + 1;
            let row = ValidatorState {
                address: Address::new(addr(n)),
                pubkey: pk(n),
                commission_bps: 500,
                self_stake: U256::from(*p) * U256::from(10u64).pow(U256::from(18u64)),
                total_delegated: U256::ZERO,
                status: ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
                oracle_signer: None,
            };
            mgr.put_validator(&Address::new(addr(n)), &row).unwrap();
        }
        let genesis = EpochManager::compute_new_validator_set(&mgr, u32::MAX, 0).unwrap();
        mgr.record_genesis_epoch_set(&genesis).unwrap();
        (dir, mgr)
    }

    /// Execute block `height`'s attribution (A1) and, at a boundary, the plans.
    fn exec(mgr: &StakingManager, height: u64, attr: &BlockAttribution) {
        let rows = attribution_rows(mgr.state(), height, L, attr).unwrap();
        apply_leader_rows(mgr.state(), &rows).unwrap();
        if EpochManager::is_epoch_boundary(height, L) {
            EpochManager::execute_planned_rotation(mgr, 16, height, L, None).unwrap();
            EpochManager::execute_leader_exclusions(mgr, height, L, Some(P)).unwrap();
        }
    }

    fn cursor(view: u64) -> Option<LeaderCursor> {
        Some(LeaderCursor {
            view,
            skip_next: false,
        })
    }

    fn missed(v: &[u8], success: u8, view: u64) -> BlockAttribution {
        BlockAttribution {
            cursor: cursor(view),
            misses: v.iter().map(|n| addr(*n)).collect(),
            success: Some(addr(success)),
            signers: Vec::new(),
        }
    }

    fn ok(success: u8, view: u64) -> BlockAttribution {
        missed(&[], success, view)
    }

    fn signed(success: u8, view: u64, signers: &[u8]) -> BlockAttribution {
        BlockAttribution {
            signers: signers.iter().map(|n| addr(*n)).collect(),
            ..ok(success, view)
        }
    }

    fn plan(mgr: &StakingManager, boundary: u64) -> Option<LeaderExclusionPlan> {
        leader_exclusion_plan(mgr.state(), boundary).unwrap()
    }

    fn excluded(p: &LeaderExclusionPlan) -> Vec<[u8; 20]> {
        p.excluded.iter().map(|m| m.address).collect()
    }

    /// Run heights `from..=to` with every block a success of validator 1 (no misses).
    fn quiet(mgr: &StakingManager, from: u64, to: u64, view: &mut u64) {
        for h in from..=to {
            *view += 1;
            exec(mgr, h, &ok(1, *view));
        }
    }

    /// The rows are consensus state: hashed by the running hash.
    #[test]
    fn leader_rows_are_hashed_consensus_meta_keys() {
        use torus_state::running_hash::{hashed_cf_id, key_is_hashed, META_CONSENSUS_PREFIXES};
        assert!(META_CONSENSUS_PREFIXES.contains(&LEADER_STATS_PREFIX));
        let meta = hashed_cf_id(CF_CONSENSUS_META).unwrap();
        for key in [
            CURSOR_KEY.to_vec(),
            EXCLUDED_KEY.to_vec(),
            validator_key(&addr(1)),
            lx_plan_key(8),
            LX_CURRENT_KEY.to_vec(),
        ] {
            assert!(key_is_hashed(meta, &key), "{key:?}");
        }
    }

    /// A1 + A2: misses add up per leader, the cursor is written; two misses in a row are below
    /// K = 3 (no plan), the third gets the validator excluded by the plan the NEXT boundary
    /// writes (for the boundary after it, W = L), with the matching hotstuff delta, and that plan
    /// is installed when its boundary executes.
    #[test]
    fn third_consecutive_miss_excludes_at_the_next_plan() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        exec(&mgr, 1, &missed(&[4], 1, 2));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 1);
        assert_eq!(leader_cursor(mgr.state()).unwrap(), cursor(2));
        exec(&mgr, 2, &missed(&[4], 1, 4));
        exec(&mgr, 3, &ok(2, 5));
        exec(&mgr, 4, &ok(3, 6));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 2);
        assert_eq!(plan(&mgr, 8), None, "two misses: below K");

        exec(&mgr, 5, &missed(&[4], 1, 8));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 3);
        exec(&mgr, 6, &ok(1, 9));
        exec(&mgr, 7, &ok(2, 10));
        exec(&mgr, 8, &ok(3, 11));
        let p = plan(&mgr, 12).expect("third miss: excluded by the plan boundary 8 writes for 12");
        assert_eq!(excluded(&p), vec![addr(4)]);
        assert_eq!(p.exclude, vec![pk(4)]);
        assert!(p.readmit.is_empty());
        assert!(
            installed_leader_exclusions(mgr.state()).unwrap().is_empty(),
            "not installed before boundary 12"
        );
        let mut view = 11;
        quiet(&mgr, 9, 12, &mut view);
        assert_eq!(
            installed_leader_exclusions(mgr.state()).unwrap(),
            vec![ExcludedMember {
                address: addr(4),
                pubkey: pk(4)
            }]
        );
        assert_eq!(plan(&mgr, 16), None, "no change: no new plan");
        let st = leader_stats(mgr.state(), &addr(4)).unwrap();
        assert_eq!(st.excluded_at, Some(12));
        assert_eq!(st.j, 1);
        assert_eq!(st.miss_run, 0, "the run starts over at the exclusion");
    }

    /// A1: a success resets its leader's run: miss, miss, success, miss, miss never reaches K.
    #[test]
    fn success_resets_the_miss_run() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        exec(&mgr, 1, &missed(&[4], 1, 2));
        exec(&mgr, 2, &missed(&[4], 1, 4));
        exec(&mgr, 3, &ok(4, 5));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 0);
        exec(&mgr, 4, &missed(&[4], 1, 7));
        exec(&mgr, 5, &missed(&[4], 1, 9));
        let mut view = 9;
        quiet(&mgr, 6, 8, &mut view);
        assert_eq!(plan(&mgr, 12), None, "the run never reached K");
        // Misses inside one gap count each; the success after them resets the run.
        exec(&mgr, 9, &missed(&[4, 4, 4], 4, 20));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 0);
    }

    /// A2 cap: with 4 equal stakes two dead validators: only one is excluded (eligible power
    /// must stay >= quorum), the one with more misses; on a tie the lower address.
    #[test]
    fn exclusion_keeps_eligible_power_at_quorum_most_misses_first() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        exec(&mgr, 1, &missed(&[3, 4, 3, 4, 3, 4, 3], 1, 9));
        let mut view = 9;
        quiet(&mgr, 2, 4, &mut view);
        let p = plan(&mgr, 8).expect("plan for 8");
        assert_eq!(excluded(&p), vec![addr(3)], "4 misses beat 3");

        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        exec(&mgr, 1, &missed(&[4, 3, 4, 3, 4, 3], 1, 9));
        let mut view = 9;
        quiet(&mgr, 2, 4, &mut view);
        assert_eq!(
            excluded(&plan(&mgr, 8).unwrap()),
            vec![addr(3)],
            "tie: lower address"
        );
    }

    /// A2 cap, bench stakes 2/2/1: the dead stake-1 node can be excluded (4 of 5 >= quorum),
    /// a dead stake-2 node cannot (3 of 5 < quorum): no plan, hotstuff's schedule unchanged.
    #[test]
    fn exclusion_never_takes_eligible_power_below_quorum() {
        let (_d, mgr) = setup(&[2_000_000, 2_000_000, 1_000_000]);
        exec(&mgr, 1, &missed(&[3, 3, 3], 1, 4));
        let mut view = 4;
        quiet(&mgr, 2, 4, &mut view);
        assert_eq!(excluded(&plan(&mgr, 8).unwrap()), vec![addr(3)]);

        let (_d, mgr) = setup(&[2_000_000, 2_000_000, 1_000_000]);
        exec(&mgr, 1, &missed(&[2, 2, 2], 1, 4));
        let mut view = 4;
        quiet(&mgr, 2, 4, &mut view);
        assert_eq!(
            leader_stats(mgr.state(), &addr(2)).unwrap().miss_run,
            3,
            "K reached"
        );
        assert_eq!(plan(&mgr, 8), None, "eligible 3M < quorum 3,333,334");
        assert_eq!(quorum_power(5_000_000), 3_333_334);
    }

    /// Exclude validator 4 by the plan for 8 (installed at 8): K misses in block 1.
    fn exclude_4(mgr: &StakingManager) -> u64 {
        exec(mgr, 1, &missed(&[4, 4, 4], 1, 4));
        let mut view = 4;
        quiet(mgr, 2, 8, &mut view);
        assert_eq!(installed_leader_exclusions(mgr.state()).unwrap().len(), 1);
        view
    }

    /// A3: re-admission needs a committed QC carrying the validator's signature within the last
    /// L heights AND the floor `excluded_at + L * 2^j` (j before this exclusion = 0: L). No
    /// signature: stays excluded. A signature too early (before the floor's window): stays.
    #[test]
    fn readmission_needs_signature_evidence_and_the_floor() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        let mut view = exclude_4(&mgr);
        let st = leader_stats(mgr.state(), &addr(4)).unwrap();
        assert_eq!((st.excluded_at, st.readmit_after), (Some(8), 12));
        // No signature in (8, 12]: the plan for 16 keeps it.
        quiet(&mgr, 9, 12, &mut view);
        assert_eq!(plan(&mgr, 16), None, "dead node: never re-admitted");
        // Signature at 14, within (12, 16]: boundary 16 re-admits it at 20 (floor 12 passed).
        quiet(&mgr, 13, 13, &mut view);
        view += 1;
        exec(&mgr, 14, &signed(1, view, &[1, 2, 4]));
        assert_eq!(
            leader_stats(mgr.state(), &addr(4)).unwrap().seen_at,
            Some(14)
        );
        quiet(&mgr, 15, 16, &mut view);
        let p = plan(&mgr, 20).expect("re-admission plan for 20");
        assert!(p.excluded.is_empty());
        assert_eq!(
            (p.exclude.clone(), p.readmit.clone()),
            (vec![], vec![pk(4)])
        );
        quiet(&mgr, 17, 20, &mut view);
        assert!(installed_leader_exclusions(mgr.state()).unwrap().is_empty());
        assert_eq!(
            leader_excluded_index(mgr.state()).unwrap(),
            Vec::<[u8; 20]>::new()
        );
        assert_eq!(
            leader_stats(mgr.state(), &addr(4)).unwrap().j,
            1,
            "j kept until a success"
        );
    }

    /// A3 2^j floor and its reset: excluded again without a success in between, the floor
    /// doubles (j = 1: 2L); after a success j is 0 again (floor L).
    #[test]
    fn readmission_floor_doubles_until_a_success() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        let mut view = exclude_4(&mgr);
        // Seen at 10, re-admitted by the plan for 16 (written at 12).
        quiet(&mgr, 9, 9, &mut view);
        view += 1;
        exec(&mgr, 10, &signed(1, view, &[4]));
        quiet(&mgr, 11, 16, &mut view);
        assert_eq!(installed_leader_exclusions(mgr.state()).unwrap(), vec![]);
        // Dead again: 3 misses at 17, excluded by the plan for 24 with floor 24 + 2L = 32.
        view += 4;
        exec(&mgr, 17, &missed(&[4, 4, 4], 1, view));
        quiet(&mgr, 18, 20, &mut view);
        let st = leader_stats(mgr.state(), &addr(4)).unwrap();
        assert_eq!((st.excluded_at, st.readmit_after, st.j), (Some(24), 32, 2));
        // Seen at 22, in (20, 24]: boundary 24 may not re-admit (24 + L = 28 < floor 32).
        // Seen again at 25, in (24, 28]: boundary 28 does (28 + L = 32 >= 32).
        quiet(&mgr, 21, 21, &mut view);
        view += 1;
        exec(&mgr, 22, &signed(1, view, &[4]));
        quiet(&mgr, 23, 24, &mut view);
        assert_eq!(plan(&mgr, 28), None, "floor 32 not reached by 28");
        view += 1;
        exec(&mgr, 25, &signed(1, view, &[4]));
        quiet(&mgr, 26, 28, &mut view);
        assert_eq!(plan(&mgr, 32).unwrap().readmit, vec![pk(4)]);
        // A success resets j.
        quiet(&mgr, 29, 32, &mut view);
        view += 1;
        exec(&mgr, 33, &ok(4, view));
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().j, 0);
    }

    /// Phase 3 (multi-node, s138): an excluded validator stays leader-eligible from the plan
    /// boundary until the install boundary L heights later, and a dead one misses its slots in
    /// between. Those misses must not survive into its re-admission: re-admitted, it has had no
    /// leader slot since (excluded until the update), so the plan written at the first boundary
    /// after its re-admission must not exclude it again. RED on 4137444b: the misses between plan
    /// and install were counted into `miss_run`, which the re-admission kept (>= K), so the
    /// recovered validator was excluded again at once, with j + 1.
    #[test]
    fn misses_between_plan_and_install_do_not_re_exclude_after_readmission() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        // K misses at 1: boundary 4 plans the exclusion for 8.
        exec(&mgr, 1, &missed(&[4, 4, 4], 1, 4));
        let mut view = 4;
        quiet(&mgr, 2, 4, &mut view);
        assert_eq!(excluded(&plan(&mgr, 8).expect("planned for 8")), vec![addr(4)]);
        // Still leader-eligible until 8 and still dead: three more misses.
        for h in 5..=7 {
            view += 2;
            exec(&mgr, h, &missed(&[4], 1, view));
        }
        quiet(&mgr, 8, 8, &mut view);
        assert_eq!(installed_leader_exclusions(mgr.state()).unwrap().len(), 1);
        assert_eq!(
            leader_stats(mgr.state(), &addr(4)).unwrap().miss_run,
            0,
            "misses of a validator in the exclusion lifecycle do not count"
        );
        // Recovered: seen at 10, re-admitted by the plan boundary 12 writes for 16.
        quiet(&mgr, 9, 9, &mut view);
        view += 1;
        exec(&mgr, 10, &signed(1, view, &[4]));
        quiet(&mgr, 11, 16, &mut view);
        assert!(installed_leader_exclusions(mgr.state()).unwrap().is_empty());
        // No slot (excluded until 16) and so no miss since: the plan for 20 is empty.
        assert_eq!(
            plan(&mgr, 20),
            None,
            "re-excluded at once on the misses it had before its re-admission"
        );
        assert_eq!(leader_stats(mgr.state(), &addr(4)).unwrap().miss_run, 0);
    }

    /// The j cap: the floor exponent never exceeds the cap (j saturates at it).
    #[test]
    fn exclusion_count_saturates_at_the_cap() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        let row = LeaderStats {
            miss_run: 3,
            j: 6,
            ..LeaderStats::default()
        };
        mgr.state()
            .put_cf_raw(
                CF_CONSENSUS_META,
                &validator_key(&addr(4)),
                &encode(&row).unwrap(),
            )
            .unwrap();
        let mut view = 0;
        quiet(&mgr, 1, 4, &mut view);
        let st = leader_stats(mgr.state(), &addr(4)).unwrap();
        assert_eq!(st.excluded_at, Some(8));
        assert_eq!(st.readmit_after, 8 + L * 64, "2^6, the cap");
        assert_eq!(st.j, 6, "capped");
    }

    /// The signature evidence is recorded only for validators in the exclusion lifecycle, at
    /// most once per epoch window (a live validator's every QC writes nothing).
    #[test]
    fn signatures_write_rows_only_for_excluded_validators() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        let rows = attribution_rows(mgr.state(), 1, L, &signed(1, 1, &[1, 2, 3, 4])).unwrap();
        assert_eq!(
            rows.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            vec![CURSOR_KEY.to_vec()],
            "live signers and a success of a clean validator change nothing but the cursor"
        );
        let mut view = exclude_4(&mgr);
        view += 1;
        exec(&mgr, 9, &signed(1, view, &[4]));
        view += 1;
        let rows = attribution_rows(mgr.state(), 10, L, &signed(1, view, &[4])).unwrap();
        assert_eq!(rows.len(), 1, "already seen in this window: cursor only");
    }

    /// A member that leaves the set leaves the exclusion lifecycle: hotstuff drops its exclusion
    /// with the delete, and so do the rows (installed set, index).
    #[test]
    fn a_departing_member_leaves_the_exclusions() {
        let (_d, mgr) = setup(&[100, 100, 100, 100, 100]);
        exec(&mgr, 1, &missed(&[5, 5, 5], 1, 4));
        let mut view = 4;
        quiet(&mgr, 2, 8, &mut view);
        assert_eq!(installed_leader_exclusions(mgr.state()).unwrap().len(), 1);
        // Validator 5 unbonds: the next member plan drops it.
        let v5 = Address::new(addr(5));
        let mut row = mgr.get_validator(&v5).unwrap().unwrap();
        row.status = ValidatorStatus::Jailed;
        mgr.put_validator(&v5, &row).unwrap();
        quiet(&mgr, 9, 16, &mut view);
        let members: Vec<_> = mgr
            .current_epoch_set()
            .unwrap()
            .unwrap()
            .iter()
            .map(|m| m.address)
            .collect();
        assert!(!members.contains(&addr(5)), "left the set");
        assert!(installed_leader_exclusions(mgr.state()).unwrap().is_empty());
        assert!(leader_excluded_index(mgr.state()).unwrap().is_empty());
        assert_eq!(
            leader_stats(mgr.state(), &addr(5)).unwrap().excluded_at,
            None
        );
    }

    /// Feature off (`params` = None): attribution rows may exist, but no plan is written.
    #[test]
    fn no_plan_without_params() {
        let (_d, mgr) = setup(&[100, 100, 100, 100]);
        let rows = attribution_rows(mgr.state(), 1, L, &missed(&[4, 4, 4], 1, 4)).unwrap();
        apply_leader_rows(mgr.state(), &rows).unwrap();
        for h in [4, 8] {
            EpochManager::execute_planned_rotation(&mgr, 16, h, L, None).unwrap();
            EpochManager::execute_leader_exclusions(&mgr, h, L, None).unwrap();
        }
        assert_eq!(plan(&mgr, 8), None);
        assert_eq!(plan(&mgr, 12), None);
    }

    fn meta_dump(mgr: &StakingManager) -> Vec<(Vec<u8>, Vec<u8>)> {
        mgr.state()
            .iterate_cf(CF_CONSENSUS_META, None)
            .unwrap()
            .into_iter()
            .filter(|(k, _)| k.starts_with(LEADER_STATS_PREFIX) || k.starts_with(b"epoch_vset:"))
            .collect()
    }

    /// Determinism (property): two executors fed the same random chain (misses, successes,
    /// signatures, boundaries) write byte-identical rows, and the run does exclude and re-admit.
    #[test]
    fn two_executors_fed_the_same_random_chain_write_identical_rows() {
        for seed in 1..=8u64 {
            let (_a, a) = setup(&[100, 100, 100, 100, 100, 100, 100]);
            let (_b, b) = setup(&[100, 100, 100, 100, 100, 100, 100]);
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut next = |m: u64| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng % m
            };
            let (mut view, mut planned) = (0u64, 0usize);
            for h in 1..=200u64 {
                let gap = next(4);
                let misses: Vec<u8> = (0..gap).map(|_| next(3) as u8 + 5).collect();
                view += gap + 1;
                let signers: Vec<u8> = (1..=7).filter(|_| next(2) == 0).collect();
                let attr = BlockAttribution {
                    signers: signers.iter().map(|n| addr(*n)).collect(),
                    ..missed(&misses, next(7) as u8 + 1, view)
                };
                exec(&a, h, &attr);
                exec(&b, h, &attr);
                if EpochManager::is_epoch_boundary(h, L) && plan(&a, h + L).is_some() {
                    planned += 1;
                }
            }
            assert_eq!(meta_dump(&a), meta_dump(&b), "seed {seed}");
            assert!(
                planned > 0,
                "seed {seed}: the chain must exercise exclusion plans"
            );
        }
    }
}
