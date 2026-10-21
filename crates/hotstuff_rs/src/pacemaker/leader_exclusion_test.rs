//! Dead-leader skip, phase 1 (docs/plans/dead-leader-skip.md §3 A4, §4): `select_leader` over a
//! validator set with a leader-excluded set.
//!
//! - an excluded validator never leads while the eligible power reaches a quorum;
//! - with an empty set the schedule is identical to the pre-exclusion function (frozen copy below);
//! - the closed form matches the reference walk with exclusions;
//! - below a quorum of eligible power, selection falls back to all members;
//! - excluded validators keep their voting power (total power and quorum unchanged).

use super::{select_leader, select_leader_reference, select_leader_reputation_weighted};
use crate::hotstuff::types::LeaderReputation;
use crate::types::{
    data_types::{Power, ViewNumber},
    update_sets::ValidatorSetUpdates,
    validator_set::{SigningKey, ValidatorSet, VerifyingKey},
};
use rand::{rngs::StdRng, Rng, SeedableRng};

/// Frozen copy of `select_leader` as it was before the leader-excluded set existed (base
/// 40cea7fc). The empty-set test proves the new function schedules exactly like this one, so a
/// chain with no exclusions keeps its leader schedule across the upgrade.
fn select_leader_pre_exclusion(view: ViewNumber, validator_set: &ValidatorSet) -> VerifyingKey {
    let p_total = validator_set.total_power();
    let n = validator_set.len();
    assert!(n > 0 && p_total.int() > 0);
    let index = (view.int() % (p_total.int() as u64)) as u128;
    let powers: Vec<u64> = validator_set
        .validators()
        .map(|v| validator_set.power(v).unwrap().int())
        .collect();
    let mut levels = powers
        .iter()
        .copied()
        .filter(|p| *p > 0)
        .collect::<Vec<_>>();
    levels.sort_unstable();
    levels.dedup();
    let mut cum: u128 = 0;
    let mut prev: u64 = 0;
    for level in levels {
        let members = powers.iter().filter(|p| **p >= level).count() as u128;
        let seg = (level - prev) as u128 * members;
        if cum + seg > index {
            let pos_in_row = ((index - cum) % members) as usize;
            let validator = validator_set
                .validators()
                .filter(|v| validator_set.power(v).unwrap().int() >= level)
                .nth(pos_in_row)
                .unwrap();
            return *validator;
        }
        cum += seg;
        prev = level;
    }
    unreachable!()
}

fn key(rng: &mut StdRng) -> VerifyingKey {
    SigningKey::from_bytes(&rng.gen::<[u8; 32]>()).verifying_key()
}

/// A validator set with these powers and fresh random keys (key order is random, so the IWRR row
/// order differs from the `powers` order).
fn build(rng: &mut StdRng, powers: &[u64]) -> (ValidatorSet, Vec<VerifyingKey>) {
    let mut vs = ValidatorSet::new();
    let mut updates = ValidatorSetUpdates::new();
    let mut keys = Vec::new();
    for p in powers {
        let vk = key(rng);
        updates.insert(vk, Power::new(*p));
        keys.push(vk);
    }
    vs.apply_updates(&updates);
    (vs, keys)
}

fn eligible_power(vs: &ValidatorSet) -> u128 {
    vs.validators_and_powers()
        .iter()
        .filter(|(v, _)| !vs.is_leader_excluded(v))
        .map(|(_, p)| p.int() as u128)
        .sum()
}

fn quorum(vs: &ValidatorSet) -> u128 {
    vs.total_power().int() * 2 / 3 + 1
}

/// Views to probe for a set: three full wraps of the eligible array (exhaustive), plus large
/// live-shaped views.
fn probe_views(vs: &ValidatorSet) -> Vec<u64> {
    let total = vs.total_power().int() as u64;
    let mut views: Vec<u64> = (0..total.saturating_mul(3).min(3_000)).collect();
    views.extend([
        1_179_840,
        2_500_000,
        5_999_999,
        6_000_000,
        6_000_001,
        u64::MAX / 3,
    ]);
    views
}

const STAKE_MIXES: &[&[u64]] = &[
    &[1, 1, 1, 1],
    &[2, 2, 1],
    &[2, 2, 2],
    &[1, 3, 2],
    &[5, 5, 1],
    &[1, 1, 1, 1, 10],
    &[4, 9, 2, 2, 6, 1, 1, 3],
    &[0, 3, 5, 4],
    &[7, 7, 7, 7, 7, 7, 7],
];

/// §4: `select_leader` never returns an excluded validator while the eligible power reaches a
/// quorum. Every single-validator exclusion and every exclusion of a quorum-preserving prefix, for
/// every stake mix; plus random sets.
#[test]
fn select_leader_never_returns_an_excluded_validator() {
    let mut rng = StdRng::seed_from_u64(0xdead_1ead);
    let mut checked_sets = 0;
    let mut check = |vs: &ValidatorSet| {
        if vs.leader_excluded().len() == 0 || eligible_power(vs) < quorum(vs) {
            return;
        }
        checked_sets += 1;
        for view in probe_views(vs) {
            let leader = select_leader(ViewNumber::new(view), vs);
            assert!(
                !vs.is_leader_excluded(&leader),
                "view {view}: excluded validator selected (powers {:?})",
                vs.validators_and_powers()
                    .iter()
                    .map(|(_, p)| p.int())
                    .collect::<Vec<_>>()
            );
        }
    };
    for powers in STAKE_MIXES {
        let (base, keys) = build(&mut rng, powers);
        for k in &keys {
            let mut vs = base.clone();
            vs.exclude_leader(k);
            check(&vs);
        }
        for cut in 2..keys.len() {
            let mut vs = base.clone();
            for k in &keys[..cut] {
                vs.exclude_leader(k);
            }
            check(&vs);
        }
    }
    for _ in 0..200 {
        let n = rng.gen_range(2, 12);
        let powers: Vec<u64> = (0..n).map(|_| rng.gen_range(0, 20)).collect();
        if powers.iter().sum::<u64>() == 0 {
            continue;
        }
        let (mut vs, keys) = build(&mut rng, &powers);
        for k in &keys {
            if rng.gen_bool(0.3) {
                vs.exclude_leader(k);
            }
        }
        check(&vs);
    }
    assert!(
        checked_sets > 50,
        "too few active exclusion sets checked: {checked_sets}"
    );
}

/// §4: with an empty leader-excluded set the schedule is identical to the pre-exclusion function,
/// for many stake mixes (fixed and random) and many views (exhaustive over three wraps plus large
/// views, and random views).
#[test]
fn empty_exclusion_set_keeps_the_pre_exclusion_schedule() {
    let mut rng = StdRng::seed_from_u64(0x5c4e_d01e);
    let mut sets: Vec<ValidatorSet> = STAKE_MIXES.iter().map(|p| build(&mut rng, p).0).collect();
    sets.push(build(&mut rng, &[2_000_000, 2_000_000, 1_000_000]).0);
    sets.push(build(&mut rng, &[2_000_000, 2_000_000, 2_000_000]).0);
    for _ in 0..300 {
        let n = rng.gen_range(1, 16);
        let powers: Vec<u64> = (0..n).map(|_| rng.gen_range(0, 1_000)).collect();
        if powers.iter().sum::<u64>() == 0 {
            continue;
        }
        sets.push(build(&mut rng, &powers).0);
    }
    for vs in &sets {
        assert_eq!(vs.leader_excluded().len(), 0);
        let mut views = probe_views(vs);
        views.extend((0..200).map(|_| rng.gen::<u64>()));
        for view in views {
            let v = ViewNumber::new(view);
            assert_eq!(
                select_leader(v, vs),
                select_leader_pre_exclusion(v, vs),
                "schedule changed with no exclusions at view {view}"
            );
        }
    }

    // An exclusion that is added then re-admitted leaves the set (and schedule) as it was.
    let (mut vs, keys) = build(&mut rng, &[3, 1, 2, 2]);
    let before = vs.clone();
    assert!(vs.exclude_leader(&keys[1]));
    assert!(vs.readmit_leader(&keys[1]));
    assert!(vs == before);
    for view in 0..64 {
        let v = ViewNumber::new(view);
        assert_eq!(
            select_leader(v, &vs),
            select_leader_pre_exclusion(v, &before)
        );
    }
}

/// §4: the closed form matches the reference walk with exclusions (including the fallback case).
#[test]
fn select_leader_closed_form_matches_reference_with_exclusions() {
    let mut rng = StdRng::seed_from_u64(0x00c1_05ed);
    let mut sets = Vec::new();
    for powers in STAKE_MIXES {
        let (base, keys) = build(&mut rng, powers);
        // Every subset for small sets, a sample of subsets otherwise.
        let n = keys.len();
        let masks: Vec<u32> = if n <= 5 {
            (0..(1u32 << n)).collect()
        } else {
            (0..64).map(|_| rng.gen_range(0, 1u32 << n)).collect()
        };
        for mask in masks {
            let mut vs = base.clone();
            for (i, k) in keys.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    vs.exclude_leader(k);
                }
            }
            sets.push(vs);
        }
    }
    let (mut live, keys) = build(&mut rng, &[2_000_000, 2_000_000, 2_000_000, 2_000_000]);
    live.exclude_leader(&keys[2]);
    sets.push(live);

    for vs in &sets {
        let total = eligible_power(vs).max(1) as u64;
        let mut views: Vec<u64> = (0..total.saturating_mul(3).min(600)).collect();
        views.extend([1_179_840, 2_500_000, 5_999_999, 6_000_000, 6_000_001]);
        for view in views {
            let v = ViewNumber::new(view);
            assert_eq!(
                select_leader(v, vs),
                select_leader_reference(v, vs),
                "diverged at view {view}, powers {:?}, excluded {}",
                vs.validators_and_powers()
                    .iter()
                    .map(|(_, p)| p.int())
                    .collect::<Vec<_>>(),
                vs.leader_excluded().len()
            );
        }
    }
}

/// With exclusions active, IWRR runs over the eligible members at their own powers: over one
/// cycle of the eligible power, each eligible member leads exactly `power` views.
#[test]
fn select_leader_with_exclusions_is_weighted_over_eligible_members() {
    let mut rng = StdRng::seed_from_u64(7);
    let (mut vs, keys) = build(&mut rng, &[3, 1, 2, 2, 4]);
    vs.exclude_leader(&keys[1]); // power 1 of 12: eligible 11 >= quorum 9
    let cycle = eligible_power(&vs) as u64;
    assert_eq!(cycle, 11);
    let leaders: Vec<VerifyingKey> = (0..cycle)
        .map(|v| select_leader(ViewNumber::new(v), &vs))
        .collect();
    for (k, p) in keys.iter().zip([3u64, 0, 2, 2, 4]) {
        assert_eq!(
            leaders.iter().filter(|l| *l == k).count() as u64,
            p,
            "leader count for a validator of power {p}"
        );
    }
    // The schedule repeats with the eligible cycle, not the total power.
    for v in 0..cycle {
        assert_eq!(
            select_leader(ViewNumber::new(v), &vs),
            select_leader(ViewNumber::new(v + cycle), &vs)
        );
    }
}

/// §4 quorum fallback: when the eligible power is below a quorum of the whole set, selection falls
/// back to all members (exactly the empty-set schedule), excluded ones included.
#[test]
fn select_leader_falls_back_to_all_members_below_quorum() {
    let mut rng = StdRng::seed_from_u64(0xfa11);

    // 4 equal stakes: quorum 3. One excluded: eligible 3 -> active. Two excluded: 2 < 3 -> fallback.
    let (base, keys) = build(&mut rng, &[1, 1, 1, 1]);
    let mut one = base.clone();
    one.exclude_leader(&keys[0]);
    assert!(one.leader_exclusion_active());
    let mut two = one.clone();
    two.exclude_leader(&keys[1]);
    assert!(!two.leader_exclusion_active());
    let mut excluded_led = false;
    for view in 0..400 {
        let v = ViewNumber::new(view);
        let leader = select_leader(v, &two);
        assert_eq!(leader, select_leader_pre_exclusion(v, &base), "view {view}");
        excluded_led |= two.is_leader_excluded(&leader);
    }
    assert!(
        excluded_led,
        "the fallback schedule includes the excluded validators"
    );

    // Bench shape 2/2/1: quorum 4. Excluding the stake-1 node leaves 4 -> active; excluding a
    // stake-2 node leaves 3 -> fallback.
    let (base, keys) = build(&mut rng, &[2_000_000, 2_000_000, 1_000_000]);
    let mut small = base.clone();
    small.exclude_leader(&keys[2]);
    assert!(small.leader_exclusion_active());
    for view in [
        0u64, 1, 2, 3, 4, 2_999_999, 3_000_000, 4_999_999, 5_000_000, 12_345_678,
    ] {
        assert_ne!(select_leader(ViewNumber::new(view), &small), keys[2]);
    }
    let mut big = base.clone();
    big.exclude_leader(&keys[0]);
    assert!(!big.leader_exclusion_active());
    for view in [
        0u64, 1, 2, 3, 4, 2_999_999, 3_000_000, 4_999_999, 5_000_000, 12_345_678,
    ] {
        let v = ViewNumber::new(view);
        assert_eq!(
            select_leader(v, &big),
            select_leader_pre_exclusion(v, &base)
        );
    }

    // Everyone excluded: fallback, no panic.
    let mut all = base.clone();
    for k in &keys {
        all.exclude_leader(k);
    }
    for view in 0..10 {
        let v = ViewNumber::new(view);
        assert_eq!(
            select_leader(v, &all),
            select_leader_pre_exclusion(v, &base)
        );
    }
}

/// Exclusion changes who leads, nothing else: total power and quorum are those of all members.
#[test]
fn exclusion_keeps_voting_power_and_quorum() {
    let mut rng = StdRng::seed_from_u64(11);
    let (base, keys) = build(&mut rng, &[2, 2, 1]);
    let mut vs = base.clone();
    vs.exclude_leader(&keys[2]);
    assert_eq!(vs.total_power().int(), base.total_power().int());
    assert!(vs.quorum() == base.quorum());
    assert_eq!(vs.power(&keys[2]), Some(&Power::new(1)));
    assert!(vs.contains(&keys[2]));
    assert_eq!(vs.len(), 3);
    assert_eq!(vs.leader_eligible_power().int(), 4);
}

/// B3 reputation weighting (off by default) keeps the exclusions: the adjusted set it builds must
/// carry them, or turning it on would bring excluded leaders back.
#[test]
fn reputation_weighted_selection_keeps_exclusions() {
    let mut rng = StdRng::seed_from_u64(13);
    let (mut vs, keys) = build(&mut rng, &[10, 10, 10, 10]);
    vs.exclude_leader(&keys[3]);
    let rep = LeaderReputation::new(100);
    for view in 20..400 {
        let leader = select_leader_reputation_weighted(ViewNumber::new(view), &vs, &rep);
        assert_ne!(leader, keys[3], "view {view}");
    }
}
