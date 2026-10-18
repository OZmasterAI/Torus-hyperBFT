//! EIP-1559 base fee arithmetic. Item 7 step 2: moved to `torus_types::eip1559` (torus-economics
//! computes the F3 per-lane base fees); re-exported here so `torus_evm::calc_next_block_base_fee`
//! callers compile unchanged.

pub use torus_types::eip1559::calc_next_block_base_fee;
