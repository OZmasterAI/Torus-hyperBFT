use serde::{Deserialize, Serialize};

use torus_types::{TorusBlockBody, TorusBlockHeader};

/// Block sync request messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncRequest {
    GetBlockHeaders { start: u64, count: u64 },
    GetBlockBodies { numbers: Vec<u64> },
}

/// Block sync response messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncResponse {
    BlockHeaders { headers: Vec<TorusBlockHeader> },
    BlockBodies { bodies: Vec<TorusBlockBody> },
}
