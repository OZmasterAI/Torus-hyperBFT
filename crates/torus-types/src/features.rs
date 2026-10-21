//! P0.0 activation-height gating (docs/plans/p00-activation-height-gating.md).
//!
//! A [`Feature`] is a state-transition change that switches on at a block
//! height. Every block header carries `features`, the bitmap of the features
//! active at its height ([`ActivationSchedule::active_at`]); validators refuse
//! a header whose bits differ from their own schedule, and execution reads the
//! bits from the header of the block it executes (never from the local clock
//! or node config), so replay applies the rule each height was agreed under.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A gated state-transition change. Ids are fixed and append-only: an id is
/// the bit position in `TorusBlockHeader::features` and is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Feature {
    /// The running state hash (docs/plans/running-state-hash-impl.md); the old
    /// genesis field `consensus.state_hash_activation_height` is its alias.
    RunningStateHash = 0,
    /// TEST-ONLY (cargo feature `test-features`, enabled from
    /// torus-consensus's dev-dependencies; no node build reaches it): the P0.0
    /// integration tests' feature with a state-visible rule (the block pipeline
    /// writes a hashed row in every block that carries the bit). Id 127, the
    /// top bit, is reserved for it: no real feature takes it.
    #[cfg(feature = "test-features")]
    TestStateRule = 127,
}

impl Feature {
    /// Every feature compiled in, in id order (test-only ones included in a
    /// `test-features` build; [`KNOWN_FEATURES`] is what the node can run).
    #[cfg(not(feature = "test-features"))]
    pub const ALL: &'static [Feature] = &[Feature::RunningStateHash];
    /// Every feature compiled in, in id order (see above).
    #[cfg(feature = "test-features")]
    pub const ALL: &'static [Feature] = &[Feature::RunningStateHash, Feature::TestStateRule];

    /// The fixed id (the bit position).
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// This feature's bit in a header bitmap.
    pub const fn bit(self) -> u128 {
        1u128 << (self as u8)
    }

    /// A test-only feature (cargo feature `test-features`): never in
    /// [`KNOWN_FEATURES`], refused in a node's schedule
    /// ([`ActivationSchedule::for_chain`]).
    pub const fn is_test_only(self) -> bool {
        match self {
            Feature::RunningStateHash => false,
            #[cfg(feature = "test-features")]
            Feature::TestStateRule => true,
        }
    }

    /// The genesis name (`consensus.activations` key).
    pub const fn name(self) -> &'static str {
        match self {
            Feature::RunningStateHash => "running_state_hash",
            #[cfg(feature = "test-features")]
            Feature::TestStateRule => "test_state_rule",
        }
    }
}

/// The bits of every feature this binary knows. A header bit outside this
/// set is a feature of a newer release: the node refuses to vote or propose
/// on it and fail-stops before executing it (`UPGRADE REQUIRED`). Test-only
/// features ([`Feature::is_test_only`]) are never in it, whatever cargo
/// features the build has: their rules exist in test code only (Codex review
/// of phase 5, P2). Tests that run one add its bit to the known set they pass
/// (`AppOptions::known_features`).
pub const KNOWN_FEATURES: u128 = known_features();

const fn known_features() -> u128 {
    let mut bits = 0u128;
    let mut i = 0;
    while i < Feature::ALL.len() {
        if !Feature::ALL[i].is_test_only() {
            bits |= Feature::ALL[i].bit();
        }
        i += 1;
    }
    bits
}

/// The bits of `features` this binary does not know (0 = all known).
pub const fn unknown_features(features: u128) -> u128 {
    features & !KNOWN_FEATURES
}

/// The lowest unknown feature id in `features`, for the `UPGRADE REQUIRED` log.
pub fn first_unknown_feature(features: u128) -> Option<u32> {
    first_unknown_feature_in(features, KNOWN_FEATURES)
}

/// [`first_unknown_feature`] against the feature set `known` instead of this
/// binary's [`KNOWN_FEATURES`] (the node passes `KNOWN_FEATURES`; the P0.0
/// integration tests run an "old binary" in process with a smaller set).
pub fn first_unknown_feature_in(features: u128, known: u128) -> Option<u32> {
    let unknown = features & !known;
    (unknown != 0).then(|| unknown.trailing_zeros())
}

/// The activations built into this binary for a network (keyed by chain id).
/// The release that carries a fix carries its height. Empty for every network
/// today; a release adds `(chain_id, &[(feature, height)])` rows here.
pub fn builtin_activations(chain_id: u64) -> &'static [(Feature, u64)] {
    const TABLE: &[(u64, &[(Feature, u64)])] = &[];
    TABLE
        .iter()
        .find(|(id, _)| *id == chain_id)
        .map_or(&[], |(_, activations)| activations)
}

/// Heights 0 and 1 are both block 1 (the genesis parent header at 0 carries
/// block 1's bits).
const fn first_block(height: u64) -> u64 {
    if height == 0 {
        1
    } else {
        height
    }
}

/// When each feature switches on: genesis `consensus.activations` (with the
/// old `state_hash_activation_height` alias) united with the built-in table
/// for the chain id. Heights 0 and 1 both mean "from block 1", and those bits
/// are in the genesis parent header too, so a node with another genesis set
/// fails block 1's parent link.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationSchedule {
    heights: BTreeMap<Feature, u64>,
}

impl ActivationSchedule {
    /// Genesis activations united with a built-in table. A feature named in
    /// both with different heights (0 and 1 are the same height) is refused:
    /// the node does not start.
    pub fn merge(
        genesis: &BTreeMap<Feature, u64>,
        builtin: &[(Feature, u64)],
    ) -> Result<Self, String> {
        let mut heights = genesis.clone();
        for &(feature, height) in builtin {
            match heights.get(&feature) {
                Some(&g) if first_block(g) != first_block(height) => {
                    return Err(format!(
                        "feature {} is at height {g} in the genesis and at {height} in this \
                         binary's built-in table",
                        feature.name()
                    ));
                }
                Some(_) => {}
                None => {
                    heights.insert(feature, height);
                }
            }
        }
        Ok(Self { heights })
    }

    /// The schedule of a network: genesis activations + its built-in table.
    /// A feature outside [`KNOWN_FEATURES`] (a test-only one, in a build with
    /// `test-features`) is refused: the node does not start.
    pub fn for_chain(chain_id: u64, genesis: &BTreeMap<Feature, u64>) -> Result<Self, String> {
        let schedule = Self::merge(genesis, builtin_activations(chain_id))?;
        if let Some((feature, _)) = schedule.iter().find(|(f, _)| KNOWN_FEATURES & f.bit() == 0) {
            return Err(format!(
                "feature {} is test-only: a node cannot schedule it",
                feature.name()
            ));
        }
        Ok(schedule)
    }

    /// The bitmap a header at `height` must carry. Height 0 (the genesis parent
    /// header) carries the bits of block 1.
    pub fn active_at(&self, height: u64) -> u128 {
        let height = first_block(height);
        self.heights
            .iter()
            .filter(|(_, at)| first_block(**at) <= height)
            .fold(0, |bits, (feature, _)| bits | feature.bit())
    }

    /// The scheduled height of `feature` as configured (None = never).
    pub fn height_of(&self, feature: Feature) -> Option<u64> {
        self.heights.get(&feature).copied()
    }

    /// The next height above `height` at which this schedule switches a
    /// feature on (`None`: nothing is scheduled above it). Same reading as
    /// [`Self::active_at`]: heights 0 and 1 are block 1, so a feature at 0 or
    /// 1 is already on at height 0.
    pub fn next_activation_after(&self, height: u64) -> Option<u64> {
        let height = first_block(height);
        self.heights
            .values()
            .map(|at| first_block(*at))
            .filter(|at| *at > height)
            .min()
    }

    /// Every scheduled feature and its height, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (Feature, u64)> + '_ {
        self.heights.iter().map(|(f, h)| (*f, *h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(pairs: &[(Feature, u64)]) -> ActivationSchedule {
        ActivationSchedule::merge(&pairs.iter().copied().collect(), &[]).unwrap()
    }

    #[test]
    fn ids_are_fixed_and_known_features_is_their_union() {
        assert_eq!(Feature::RunningStateHash.id(), 0);
        assert_eq!(Feature::RunningStateHash.bit(), 1);
        // Test-only features are never in this binary's set (Codex review
        // of phase 5, P2): see `known_features_never_hold_a_test_feature`.
        assert_eq!(KNOWN_FEATURES, 1);
        assert_eq!(first_unknown_feature_in(0b11, 0b01), Some(1));
        assert_eq!(first_unknown_feature_in(0b01, 0b01), None);
        assert_eq!(unknown_features(0b101), 0b100);
        assert_eq!(first_unknown_feature(0b1), None);
        assert_eq!(first_unknown_feature(1 | (1 << 9) | (1 << 77)), Some(9));
        for f in Feature::ALL {
            let json = serde_json::to_string(f).unwrap();
            assert_eq!(json, format!("\"{}\"", f.name()));
        }
    }

    /// Codex review of phase 5 (P2): a build with the cargo feature
    /// `test-features` (a workspace `--all-features` build of the node does
    /// this) must not count the test feature as one it can execute: its rule
    /// exists in test code only, so such a binary would apply the bit without
    /// its transition. `KNOWN_FEATURES` holds no test feature, and a genesis /
    /// built-in schedule naming one refuses to start; tests inject the bit
    /// through the known set they pass (`AppOptions::known_features`).
    /// RED before the fix (with `--features test-features`): bit 127 known,
    /// a genesis `test_state_rule` accepted.
    #[test]
    fn known_features_never_hold_a_test_feature() {
        for f in Feature::ALL {
            assert_eq!(
                KNOWN_FEATURES & f.bit() != 0,
                !f.is_test_only(),
                "{}: known iff not test-only",
                f.name()
            );
        }
        assert_eq!(KNOWN_FEATURES & (1 << 127), 0, "bit 127 is reserved for tests");
        #[cfg(feature = "test-features")]
        {
            assert!(Feature::TestStateRule.is_test_only());
            assert_eq!(first_unknown_feature(Feature::TestStateRule.bit()), Some(127));
            let genesis: BTreeMap<_, _> = [(Feature::TestStateRule, 5)].into_iter().collect();
            let err = ActivationSchedule::for_chain(crate::eip712::TORUS_CHAIN_ID, &genesis)
                .expect_err("a test-only feature in a node schedule refuses to start");
            assert!(err.contains("test_state_rule"), "{err}");
            // Tests build their schedules with `merge`.
            assert!(ActivationSchedule::merge(&genesis, &[]).is_ok());
        }
        assert!(!Feature::RunningStateHash.is_test_only());
    }

    /// RED before P0.0 phase 1: `active_at` returned 0 for every height.
    #[test]
    fn a_feature_is_active_from_its_height_and_0_or_1_means_block_1() {
        let s = schedule(&[(Feature::RunningStateHash, 5)]);
        assert_eq!(s.active_at(0), 0);
        assert_eq!(s.active_at(4), 0);
        assert_eq!(s.active_at(5), 1);
        assert_eq!(s.active_at(6), 1);
        assert_eq!(s.active_at(u64::MAX), 1);
        for at in [0, 1] {
            let s = schedule(&[(Feature::RunningStateHash, at)]);
            assert_eq!(
                s.active_at(0),
                1,
                "genesis parent header carries block 1's bits ({at})"
            );
            assert_eq!(s.active_at(1), 1, "{at}");
        }
        assert_eq!(ActivationSchedule::default().active_at(1_000_000), 0);
    }

    /// RED before P0.0 phase 1: a conflicting built-in height was ignored.
    #[test]
    fn genesis_and_builtin_must_agree_on_a_shared_feature() {
        let genesis: BTreeMap<_, _> = [(Feature::RunningStateHash, 10)].into_iter().collect();
        let err = ActivationSchedule::merge(&genesis, &[(Feature::RunningStateHash, 11)])
            .expect_err("different heights refuse to start");
        assert!(err.contains("running_state_hash"), "{err}");
        let same = ActivationSchedule::merge(&genesis, &[(Feature::RunningStateHash, 10)]).unwrap();
        assert_eq!(same.height_of(Feature::RunningStateHash), Some(10));
        // 0 and 1 are the same height (block 1).
        let g0: BTreeMap<_, _> = [(Feature::RunningStateHash, 0)].into_iter().collect();
        assert!(ActivationSchedule::merge(&g0, &[(Feature::RunningStateHash, 1)]).is_ok());
        // Built-in only: taken as is.
        let only =
            ActivationSchedule::merge(&BTreeMap::new(), &[(Feature::RunningStateHash, 7)]).unwrap();
        assert_eq!(only.active_at(7), 1);
        assert_eq!(only.active_at(6), 0);
    }

    /// P0.0 phase 6 (ops gauges / RPC): the next activation above a height.
    /// RED before phase 6: always `None`.
    #[test]
    fn next_activation_is_the_lowest_scheduled_height_above_the_given_one() {
        let s = schedule(&[(Feature::RunningStateHash, 10)]);
        assert_eq!(s.next_activation_after(0), Some(10));
        assert_eq!(s.next_activation_after(9), Some(10));
        assert_eq!(s.next_activation_after(10), None, "on at 10");
        assert_eq!(s.next_activation_after(u64::MAX), None);
        for at in [0, 1] {
            let s = schedule(&[(Feature::RunningStateHash, at)]);
            assert_eq!(s.next_activation_after(0), None, "{at}: block 1 bits at 0");
        }
        assert_eq!(ActivationSchedule::default().next_activation_after(0), None);
    }

    #[test]
    fn every_network_has_an_empty_builtin_table_today() {
        for chain_id in [crate::eip712::TORUS_CHAIN_ID, 7779, 1] {
            assert!(builtin_activations(chain_id).is_empty(), "{chain_id}");
        }
    }

    #[test]
    fn schedule_serde_uses_feature_names() {
        let s = schedule(&[(Feature::RunningStateHash, 42)]);
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["heights"]["running_state_hash"], 42);
        let back: ActivationSchedule = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
        let unknown = serde_json::json!({"heights": {"no_such_feature": 1}});
        assert!(serde_json::from_value::<ActivationSchedule>(unknown).is_err());
    }
}
