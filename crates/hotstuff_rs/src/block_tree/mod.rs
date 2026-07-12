//! The persistent state of a replica.
//!
//! # The Block Tree
//!
//! The Block Tree is the fundamental persistent data structure in HotStuff-rs, the HotStuff
//! subprotocol, and "Chain-Based" state machine replication in general.
//!
//! A block tree is a directed acyclic graph (or "Tree") of [Blocks](crate::types::block::Block) rooted
//! at a starting block called the "Genesis Block". Block trees are very "narrow" trees, in the sense
//! that most blocks in a block tree will only have one edge coming into it. Specifically, all blocks
//! in a block tree "under" a highest committed block (i.e., closer to genesis block to than the highest
//! committed block) will only have one edge coming out of it.
//!
//! Because of this, a block tree can be understood as being a linked list with a tree attached to it
//! at the highest committed block. In this understanding, there are **two kinds of blocks** in a block
//! tree:
//! 1. **Committed Blocks**: blocks in the linked list. These are permanently part of the block tree.
//! 2. **Speculative Blocks**: blocks in the tree. These, along with their descendants, are "pruned"
//!    when a "conflicting" block is committed.
//!
//! A speculative block is committed when a 3-Chain is formed extending it. This is the point at which
//! the HotStuff subprotocol can guarantee that a block that conflicts with the block can never be
//! committed and that therefore, the block is now permanently part of the block tree. The logic that
//! makes this guarantee sound is implemented in [`invariants`].
//!
//! # Beyond the Block Tree
//!
//! The main purpose of the definitions in the `block_tree` module is to store and maintain the local
//! replica's persistent block tree. However, the block tree is not the only persistent state in a
//! replica.
//!
//! In addition, The block tree module also stores additional data fundamental to the operation of the
//! HotStuff-rs. These data include data relating to the [two App-mutable
//! states](crate::app#two-app-mutable-states-app-state-and-validator-set), as well as state variables
//! used by the [HotStuff](crate::hotstuff) and [Pacemaker](crate::pacemaker) subprotocols to make sure
//! that the block tree is updated in a way that preserves its core [safety and liveness
//! invariants](invariants).
//!
//! The [List of State Variables](variables#list-of-state-variables) section of the `variables`
//! submodule comprehensively lists everything stored by the block tree module.
//!
//! # Pluggable persistence
//!
//! A key feature of HotStuff-rs is "pluggable persistence". The key enabler for pluggable persistence
//! is the fact that the `block_tree` module does not care about how its state variables are stored in
//! persistent storage, as long as whatever mechanism the library user chooses implements the abstract
//! functionality of a key-value store with atomic, batched writes.
//!
//! The interface that `block_tree` code uses to interact with this functionality is expressed in the
//! traits defined in the [`pluggables`] module. To plug a new persistence mechanism for use by a
//! replica, the user simply needs to implement these traits and provide an implementation of the top-
//! level [`KVStore`](pluggables::KVStore) trait to the `ReplicaSpecBuilder` by calling its
//! [`builder`](crate::replica::ReplicaSpecBuilder::kv_store) method.
//!
//! Upon replica startup, the provided implementations of the pluggable persistence traits are wrapped
//! inside types called block tree [`accessors`], which provide safe interfaces for accessing the block
//! tree for both library-internal and public use.

pub mod accessors;

pub mod invariants;

pub mod recovery;

pub mod variables;

pub mod pluggables;

use std::sync::atomic::{AtomicU64, Ordering};

/// Floor for [`set_block_tree_retention`]: retention windows smaller than this
/// are clamped up so pruning can never reach the uncommitted tail / speculative
/// window (pipeline depth ~1-3 views). Production deployments should use much
/// larger windows (torus-node clamps to >= 1000).
pub const MIN_BLOCK_TREE_RETENTION: u64 = 8;

/// Block-tree retention window in blocks. `0` = pruning disabled (archive).
///
/// Node-local storage policy, NOT consensus-critical: validators may run
/// different retention windows (or none) without affecting agreement. Mirrors
/// the `set_reputation_leader_selection` crate-static pattern.
static BLOCK_TREE_RETENTION: AtomicU64 = AtomicU64::new(0);

/// Enable pruning of old committed blocks from the block tree, retaining the
/// most recent `retention` committed heights. `None` or `Some(0)` disables
/// pruning (archive mode, the default). Values below
/// [`MIN_BLOCK_TREE_RETENTION`] are clamped up.
///
/// Call once at node startup, before the replica starts.
pub fn set_block_tree_retention(retention: Option<u64>) {
    let value = match retention {
        Some(0) | None => 0,
        Some(r) => r.max(MIN_BLOCK_TREE_RETENTION),
    };
    BLOCK_TREE_RETENTION.store(value, Ordering::Relaxed);
}

/// The active block-tree retention window, if pruning is enabled.
pub fn block_tree_retention() -> Option<u64> {
    match BLOCK_TREE_RETENTION.load(Ordering::Relaxed) {
        0 => None,
        r => Some(r),
    }
}
