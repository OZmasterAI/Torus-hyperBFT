//! Error types for the torus-core execution engine.

use thiserror::Error;
use torus_types::OrderId;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("order not found: {0}")]
    OrderNotFound(OrderId),

    #[error("invalid quantity: must be positive")]
    InvalidQuantity,

    #[error("invalid price: must be positive for limit orders")]
    InvalidPrice,
}
