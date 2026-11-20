//! Grants (M21.2): what lets an op run without a card.
//!
//! - **Chat grants** (D3): a chat turn proposes an op with its basis, a message and a quote from it.
//!   Rust checks that the message is the person's typed words (tier one), that the quote is inside
//!   its typed spans, that every target id is one Rust resolved from that message (or from the
//!   screen when it was sent), and that the grant is used once and only in its turn. [`decide`]
//!   then checks the op, its arguments and the level's ceiling.
//! - **Short grants**: "allow for the rest of the session" allows this op on these targets for this
//!   session, never a whole kind. They live in memory on the monotonic clock: all are dropped on a
//!   restart, and when the wall clock moved away from the monotonic one (the Mac slept, or the
//!   clock was changed: `machine.woke`).
//! - **Yes in chat**: a typed "yes" binds to a card only when exactly one is open; a spoken one
//!   never does.
//!
//! [`decide`]: super::decide

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Grant, op_hash};
use crate::ops::{Op, Targets};
use crate::words::{Origin, Tier, WordsId, WordsRecord};

/// Why a chat turn's proposal backs no grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refused {
    /// The message is not the person's typed words (a paste, a session's reply, voice, a phone).
    NotTheirWords,
    /// The quote is not in the message's typed spans.
    QuoteNotFound,
    /// The quote only says yes: it can answer a card, never back an op (land-12).
    OnlyYes,
    /// A target the op touches was not named in the message, nor on screen when it was sent.
    TargetNotNamed,
    /// The message already backed this op once.
    Used,
    /// The turn the grant was for has ended.
    TurnOver,
}

impl Refused {
    pub fn says(&self) -> &'static str {
        match self {
            Refused::NotTheirWords => "those are not words you typed in this chat",
            Refused::QuoteNotFound => "the quote is not in what you typed",
            Refused::OnlyYes => "a yes answers a card; it asks for nothing itself",
            Refused::TargetNotNamed => "it touches something your message did not name",
            Refused::Used => "your message already allowed this once",
            Refused::TurnOver => "the turn it was for has ended",
        }
    }
}

/// A chat turn's proposal: the op, and its basis in the person's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal<'a> {
    pub op: &'a Op,
    pub targets: &'a Targets,
    pub message: &'a WordsRecord,
    pub quote: &'a str,
    pub turn: &'a str,
}

/// The chat grants of one conversation: which message backed which op, and the turn now.
#[derive(Clone, Debug, Default)]
pub struct ChatGrants {
    used: BTreeSet<(WordsId, [u8; 32])>,
    turn: Option<String>,
}

impl ChatGrants {
    /// A new turn starts: grants of the last one end with it.
    pub fn start_turn(&mut self, turn: &str) {
        self.turn = Some(turn.to_string());
    }

    pub fn end_turn(&mut self) {
        self.turn = None;
    }

    /// The grant a proposal backs, or why not. `named` holds the ids Rust resolved from the message
    /// (or the screen when it was sent). It is used once: a second call for the same message and
    /// op is refused.
    pub fn grant(&mut self, proposal: &Proposal, named: &Targets) -> Result<Grant, Refused> {
        if self.turn.as_deref() != Some(proposal.turn) {
            return Err(Refused::TurnOver);
        }
        if proposal.message.tier() != Tier::One {
            return Err(Refused::NotTheirWords);
        }
        if is_yes(proposal.quote) {
            return Err(Refused::OnlyYes);
        }
        let quote = proposal.message.quote(proposal.quote).ok_or(Refused::QuoteNotFound)?;
        if !super::within(proposal.targets, named) {
            return Err(Refused::TargetNotNamed);
        }
        let hash = op_hash(proposal.op);
        if !self.used.insert((proposal.message.id.clone(), hash)) {
            return Err(Refused::Used);
        }
        Ok(Grant { message: proposal.message.id.clone(), quote, op_hash: hash, targets: proposal.targets.clone(), turn: proposal.turn.to_string() })
    }
}

/// How far the wall clock may run ahead of the monotonic one between two looks before it counts
/// as a wake (or a clock change).
pub const WAKE_GAP_MS: u64 = 60_000;

/// One short grant.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Short {
    session: String,
    op: String,
    targets: [u8; 32],
    until_mono_ms: u64,
}

/// "Allow for the rest of the session": op and target, never a kind; monotonic; never on disk.
#[derive(Clone, Debug, Default)]
pub struct SessionGrants {
    grants: Vec<Short>,
    /// The last look: (wall, monotonic) milliseconds.
    last: Option<(u64, u64)>,
}

/// The hash of what an op touches, for a short grant.
pub fn targets_hash(targets: &Targets) -> [u8; 32] {
    super::sha256_json(&serde_json::to_value(targets).unwrap_or(serde_json::Value::Null))
}

impl SessionGrants {
    /// Look at the clocks: a gap between the wall and the monotonic time since the last look is a
    /// wake or a clock change (`machine.woke`), and drops every grant. True when it did.
    pub fn look(&mut self, wall_ms: u64, mono_ms: u64) -> bool {
        let woke = match self.last {
            Some((wall, mono)) => {
                let walled = wall_ms as i128 - wall as i128;
                let ran = mono_ms as i128 - mono as i128;
                (walled - ran).unsigned_abs() > u128::from(WAKE_GAP_MS)
            }
            None => false,
        };
        self.last = Some((wall_ms, mono_ms));
        if woke {
            self.grants.clear();
        }
        woke
    }

    /// Allow `op` on `targets` for `session` for `for_ms` of monotonic time.
    pub fn allow(&mut self, session: &str, op: &Op, targets: &Targets, for_ms: u64, wall_ms: u64, mono_ms: u64) {
        self.look(wall_ms, mono_ms);
        let short = Short { session: session.to_string(), op: op.name().to_string(), targets: targets_hash(targets), until_mono_ms: mono_ms.saturating_add(for_ms) };
        self.grants.retain(|g| !(g.session == short.session && g.op == short.op && g.targets == short.targets));
        self.grants.push(short);
    }

    /// Whether `session` may run `op` on `targets` now.
    pub fn covers(&mut self, session: &str, op: &Op, targets: &Targets, wall_ms: u64, mono_ms: u64) -> bool {
        self.look(wall_ms, mono_ms);
        self.grants.retain(|g| g.until_mono_ms > mono_ms);
        let hash = targets_hash(targets);
        self.grants.iter().any(|g| g.session == session && g.op == op.name() && g.targets == hash)
    }

    pub fn len(&self) -> usize {
        self.grants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }
}

/// The words that say yes in chat (an English list: advisory, crit-18; a card's button is the
/// approval everywhere else).
const YES: &[&str] = &["yes", "y", "yep", "yeah", "ok", "okay", "sure", "go ahead", "do it", "approve", "approved", "allow", "allow it"];

/// Whether a chat message is a yes that binds to the one open card: typed in a composer (tier
/// one), a yes word and nothing else, and exactly one card open. A spoken yes never binds.
pub fn yes_binds(origin: &Origin, text: &str, open_cards: usize) -> bool {
    open_cards == 1 && matches!(origin, Origin::Composer(_)) && is_yes(text)
}

/// Whether a text only says yes.
fn is_yes(text: &str) -> bool {
    let said = text.trim().trim_end_matches(['.', '!', ',']).trim().to_lowercase();
    YES.contains(&said.as_str())
}
