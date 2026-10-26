//! Glasscode's core: session model, persistence, agent drivers and git helpers.
//! Nothing in this crate depends on the UI toolkit.

pub mod agents;
pub mod env;
pub mod git;
pub mod keychain;
pub mod model;
pub mod paths;
pub mod prefs;
pub mod store;
pub mod transcript;
