//! torus-wallet library: the encrypted secp256k1 keystore, shared with the
//! validator price feeder (`tools/price-feeder`). The CLI modules stay private
//! to the `torus-wallet` binary.

pub mod keystore;
#[cfg(test)]
mod test_utils;
