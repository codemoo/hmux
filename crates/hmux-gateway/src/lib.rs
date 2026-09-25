//! Candidate gateway. Production still uses the Go implementation.

pub mod admission;
pub mod auth;
pub mod auth_store;
pub mod bootstrap;
mod browser_control;
pub mod browser_terminal;
pub mod browser_upload;
pub mod diagnostics;
pub mod http_auth;
mod http_body;
pub mod http_boundary;
pub mod http_home;
pub mod http_upgrade;
pub mod hub;
pub mod observation;
pub mod push;
pub mod push_crypto;
pub mod push_state;
pub mod push_transport;
pub mod runtime;
pub mod session_location;
pub mod static_assets;
pub(crate) mod upload_contract;
pub mod usage_preferences;
