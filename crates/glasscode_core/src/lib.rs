//! Glasscode's core: session model, persistence, agent drivers and git helpers.
//! Nothing in this crate depends on the UI toolkit.

pub mod agents;
pub mod board;
pub mod chat;
pub mod cli;
pub mod control;
pub mod day;
pub mod env;
pub mod git;
pub mod github;
pub mod github_accounts;
pub mod handoff;
pub mod history;
pub mod keychain;
pub mod linear;
pub mod update;
pub mod usage;
pub mod markdown;
pub mod net;
pub mod naming;
pub mod model;
pub mod paths;
pub mod prefs;
pub mod prompt;
pub mod store;
pub mod transcript;
pub mod versions;
