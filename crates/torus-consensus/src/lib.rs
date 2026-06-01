//! Consensus layer — hotstuff_rs integration for Torus-hyperBFT.
//!
//! Implements the three pluggable traits required by hotstuff_rs v0.4.0:
//! - [`KVStore`](kv_store::RocksKVStore) — RocksDB-backed consensus storage
//! - [`App`](app::TorusApp) — block production and validation
//! - [`Network`](network::ChannelNetwork) — in-process channel transport (testing)
//!
//! Plus genesis configuration for initial validator sets.

pub mod app;
pub mod genesis;
pub mod kv_store;
pub mod network;
pub mod slashing;

pub use app::{LeaderState, PreProposalBundle, TorusApp};
pub use genesis::GenesisConfig;
pub use kv_store::RocksKVStore;
pub use network::ChannelNetwork;
pub use slashing::{DoubleSignDetector, DoubleSignEvidence, DowntimeTracker};
