//! `net_*` JSON-RPC namespace.

use jsonrpsee::core::{async_trait, RpcResult};
use jsonrpsee::proc_macros::rpc;

use crate::RpcState;

#[rpc(server, namespace = "net")]
pub trait NetApi {
    #[method(name = "version")]
    async fn version(&self) -> RpcResult<String>;

    #[method(name = "listening")]
    async fn listening(&self) -> RpcResult<bool>;

    #[method(name = "peerCount")]
    async fn peer_count(&self) -> RpcResult<String>;
}

#[async_trait]
impl NetApiServer for RpcState {
    async fn version(&self) -> RpcResult<String> {
        Ok(self.chain_id.to_string())
    }

    async fn listening(&self) -> RpcResult<bool> {
        Ok(true)
    }

    async fn peer_count(&self) -> RpcResult<String> {
        Ok("0x0".to_string())
    }
}
