//! `web3_*` JSON-RPC namespace.

use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;

use crate::types::{hex_b256, parse_bytes};
use crate::RpcState;

#[rpc(server, namespace = "web3")]
pub trait Web3Api {
    #[method(name = "clientVersion")]
    async fn client_version(&self) -> RpcResult<String>;

    #[method(name = "sha3")]
    async fn sha3(&self, data: String) -> RpcResult<String>;
}

#[async_trait]
impl Web3ApiServer for RpcState {
    async fn client_version(&self) -> RpcResult<String> {
        Ok("torus/v0.1.0".to_string())
    }

    async fn sha3(&self, data: String) -> RpcResult<String> {
        // Input is hex-encoded, decode first then keccak256.
        let bytes = parse_bytes(&data).map_err(jsonrpsee::types::ErrorObjectOwned::from)?;
        let hash = alloy_primitives::keccak256(&bytes);
        Ok(hex_b256(hash))
    }
}
