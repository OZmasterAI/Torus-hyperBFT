//! Validator price feeder (item 2, option B; `docs/plans/oracle-feeder.md`).
//!
//! One feeder per validator: it fetches spot mids from 7 venues, aggregates a
//! weighted median per configured market, and submits `SubmitOraclePrices` to
//! the LOCAL node, signed by the validator's hot oracle signer.

pub mod config;
pub mod exchange;
pub mod feeder;
pub mod fetch;
pub mod health;
pub mod keyfile;
pub mod node;
pub mod price;
pub mod submit;
pub mod testing;
