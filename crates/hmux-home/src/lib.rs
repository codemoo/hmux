//! Candidate Home configuration, catalog, peer and owned WSS reconnect.
//! Connected PTY/view, refresh, proxy and explicit process startup are candidates.
//! Exact provider bindings, public conversations, host metrics and completion
//! observation and shared source-separated usage are included.
//! Recovery, shared workspace, provider setup and workflow owners are included.
//! Production CLI/services and platform/device acceptance remain incomplete.

pub mod agent_support;
pub mod binding;
pub mod catalog;
mod completion;
pub mod completion_tracker;
pub mod config;
pub mod connector;
mod conversation;
pub mod create_plan;
pub mod dial;
pub mod filestage;
pub mod inspection;
pub mod metrics;
pub mod metrics_parsers;
pub mod observation;
pub mod peer;
pub mod process;
mod proxy;
pub mod pty;
pub mod records;
pub mod refresh;
pub mod runtime;
pub mod sessions;
pub mod sessionstate;
pub mod singleton;
mod terminal;
pub mod transcript;
pub mod upgrade;
mod upload;
pub mod usage_http;
pub mod view;

#[cfg(test)]
#[path = "metrics_parsers_tests.rs"]
mod metrics_parsers_tests;

pub mod usage_credentials;
pub mod usage_oauth;

pub mod usage_lb;

pub mod usage;
pub mod usage_activity;
pub mod usage_config;
pub mod usage_sources;

pub mod workspace;

pub mod workflow;

pub mod providers;

pub mod recovery;
mod recovery_binding;
