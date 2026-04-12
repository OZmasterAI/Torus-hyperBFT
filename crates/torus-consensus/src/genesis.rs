//! Genesis configuration for initializing consensus with an initial validator set.

use ed25519_dalek::VerifyingKey;

use hotstuff_rs::types::data_types::Power;
use hotstuff_rs::types::update_sets::AppStateUpdates;
use hotstuff_rs::types::validator_set::{ValidatorSet, ValidatorSetState};

/// Genesis configuration defining the initial validator set and chain identity.
pub struct GenesisConfig {
    pub chain_id: u64,
    pub validators: Vec<(VerifyingKey, u64)>,
}

impl GenesisConfig {
    /// Build the initial [`ValidatorSet`] from the genesis validators.
    pub fn validator_set(&self) -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        for (key, power) in &self.validators {
            vs.put(key, Power::new(*power));
        }
        vs
    }

    /// Build the [`ValidatorSetState`] for genesis initialization.
    ///
    /// Sets committed == previous (no prior changes), no update height,
    /// and update_decided = true (no pending validator set update at genesis).
    pub fn validator_set_state(&self) -> ValidatorSetState {
        let vs = self.validator_set();
        ValidatorSetState::new(vs.clone(), vs, None, true)
    }

    /// Build an empty initial app state (no pre-seeded key-value pairs).
    pub fn initial_app_state(&self) -> AppStateUpdates {
        AppStateUpdates::new()
    }
}
