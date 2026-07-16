//! Networking layer for Torus-hyperBFT.
//!
//! Bridges libp2p (async, tokio) with hotstuff_rs `Network` trait (sync).
//!
//! ## Architecture
//!
//! - Background tokio task drives the libp2p Swarm event loop
//! - Outbound: `send()`/`broadcast()` push commands to an mpsc channel,
//!   consumed by the tokio task which publishes via GossipSub or request-response
//! - Inbound: tokio task receives events, pushes `(VerifyingKey, Message)` pairs
//!   to a shared `VecDeque`. `recv()` pops from it — non-blocking.
//! - `broadcast()` also pushes a copy to the local inbound queue (self-delivery).

pub mod behaviour;
pub mod bridge;
pub mod caps;
pub mod codec;
pub mod config;
pub mod mesh_watchdog;
pub mod native_inbound;
pub mod peer;
pub mod peer_scoring;
pub mod pending_send;
pub mod swarm;
pub mod sync;
pub mod transport;
pub mod tx_gossip;

pub use bridge::{should_push_hashes_only, LibP2PNetwork, HASH_ONLY_PUSH_THRESHOLD};
pub use config::NetworkConfig;
pub use native_inbound::{NativeInboundKind, PerPeerBudget, RawNativeInbound};
pub use pending_send::PendingSendQueue;
pub use sync::{SyncRequest, SyncResponse};
pub use tx_gossip::{NativeGossipHandle, TxGossipHandle};
