//! Bounded embedded usage collection primitives. This crate has no service,
//! credential writer or provider subprocess of its own. Sources never serialize
//! raw provider objects; only the public snapshot allowlist may leave Home.
pub mod model;
pub mod oauth;
pub mod transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Limit,
}

pub use model::{Account, AccountWindow, Provider, Snapshot, Status, Window};

pub mod codex_accounts;
pub mod codex_lb;
pub mod cswap;
pub mod quota_state;
pub mod sources;

pub mod credentials;

pub mod activity;
