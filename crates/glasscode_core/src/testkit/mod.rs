//! The test kit of Manager v2 (slice M02): what the tests of every later slice build on, with no
//! model anywhere (rule 13 of the slice plan).
//!
//! - [`home`]: a scratch Glasscode home with projects, scratch accounts, the agents' own files and a
//!   `state.json`; the binary `make-test-home` builds one by hand. Nothing here reaches the person's
//!   `~/.glasscode`, `~/.claude` or `~/.codex` (rule 11, exec-m4).
//! - [`clock`]: a virtual clock for [`crate::clock`], with sleep, jumps and DST changes.
//! - [`agents`]: tapes, the data the stand-in agents replay (`script/test-agents/`), and a player of
//!   the same tapes in this process.
//! - [`sim`]: scripts of inputs for a runtime under test, and what it did, with the assertions
//!   every slice uses (`no_duplicate_effects`, `one_report_per_turn`).
//! - [`load`]: the reference load of slice plan section 14, as a generator.
//!
//! The kit is compiled for the core's own tests and with the `testkit` feature (the eval suites in
//! `tests/evals`, the binary). It declares no `store::Store`: its files are scratch files, not
//! stores of the data map.

pub mod agents;
pub mod clock;
pub mod home;
pub mod load;
pub mod sim;

pub use clock::{VirtualClock, Zone};
pub use home::{Home, HomeSpec};
