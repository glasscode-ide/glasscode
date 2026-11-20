//! Who does each of the manager's rules: v2's built-ins and round, once v2 has taken over on the
//! home (M40 part 2, M44; v1's harness, which had them before, went with M56).
//!
//! | rule | who |
//! |---|---|
//! | needs-you | the native built-in `needs-you` |
//! | review-tell | the native built-in `review-tell`, on the review feed's `review.received` events |
//! | follow-ups | the built-in `follow-ups` (the work ledger's round) |
//! | daily brief | the built-in `daily-brief` |
//! | stuck session, usage | the native built-ins `stuck-session`, `usage-handoff` |
//! | sending | v2's round, from `manager/outbox.json`, each send claimed (`messages::send_intent`) |
//! | inbox | v2's round, into `manager/inbox.json` |
//!
//! With the manager off, or before v2 took over (on the home, once v1's data moved or the move
//! gave up: `lifetime::Migration`), nobody does any of them. A built-in the person switched off is
//! nobody's.

/// One of the manager's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rule {
    NeedsYou,
    ReviewTell,
    FollowUps,
    Brief,
    Stuck,
    Usage,
    Send,
    Inbox,
}

/// Every rule.
pub const RULES: [Rule; 8] = [Rule::NeedsYou, Rule::ReviewTell, Rule::FollowUps, Rule::Brief, Rule::Stuck, Rule::Usage, Rule::Send, Rule::Inbox];

/// Who does a rule now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    V2,
    Nobody,
}

/// The mode the manager is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub manager_on: bool,
    /// v2 took over on this machine in this launch: it is the home, v1's data moved (or the move
    /// gave up, M56), and the journal runs.
    pub taken_over: bool,
}

impl Rule {
    /// The built-in that stands for it (its entry in the playbooks' file switches it).
    pub fn builtin(self) -> Option<&'static str> {
        match self {
            Rule::NeedsYou => Some("needs-you"),
            Rule::ReviewTell => Some("review-tell"),
            Rule::FollowUps => Some("follow-ups"),
            Rule::Brief => Some("daily-brief"),
            Rule::Stuck => Some("stuck-session"),
            Rule::Usage => Some("usage-handoff"),
            Rule::Send | Rule::Inbox => None,
        }
    }
}

/// Who does `rule` in `mode`; `switched_on` says whether a built-in is on (its entry).
pub fn owner(rule: Rule, mode: Mode, switched_on: &dyn Fn(&str) -> bool) -> Owner {
    if !mode.manager_on || !mode.taken_over {
        return Owner::Nobody;
    }
    match rule.builtin() {
        Some(id) if !switched_on(id) => Owner::Nobody,
        _ => Owner::V2,
    }
}

/// The rule a journal event kind is for, among the native built-ins.
pub fn rule_of_kind(kind: &str) -> Option<Rule> {
    match kind {
        "session.needs_input" | "session.activity" => Some(Rule::NeedsYou),
        "review.received" => Some(Rule::ReviewTell),
        "turn.ended" => Some(Rule::Stuck),
        "usage.threshold" => Some(Rule::Usage),
        _ => None,
    }
}

/// Who does each rule, for one round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owned {
    pub needs_you: Owner,
    pub review_tell: Owner,
    pub follow_ups: Owner,
    pub brief: Owner,
    pub send: Owner,
}

impl Owned {
    pub fn of(mode: Mode, switched_on: &dyn Fn(&str) -> bool) -> Owned {
        let of = |rule| owner(rule, mode, switched_on);
        Owned { needs_you: of(Rule::NeedsYou), review_tell: of(Rule::ReviewTell), follow_ups: of(Rule::FollowUps), brief: of(Rule::Brief), send: of(Rule::Send) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule in every mode, with its built-in on and off.
    #[test]
    fn who_owns_each_rule_in_each_mode() {
        for rule in RULES {
            for manager_on in [false, true] {
                for taken_over in [false, true] {
                    for on in [false, true] {
                        let mode = Mode { manager_on, taken_over };
                        let got = owner(rule, mode, &|_| on);
                        let want = if !manager_on || !taken_over {
                            Owner::Nobody
                        } else if matches!(rule, Rule::Send | Rule::Inbox) || on {
                            Owner::V2
                        } else {
                            // A built-in switched off is nobody's.
                            Owner::Nobody
                        };
                        assert_eq!(got, want, "{rule:?} in {mode:?}, built-in on: {on}");
                    }
                }
            }
        }
    }

    #[test]
    fn one_switch_per_built_in() {
        let mode = Mode { manager_on: true, taken_over: true };
        let owned = Owned::of(mode, &|id| id != "review-tell");
        assert_eq!((owned.needs_you, owned.review_tell, owned.follow_ups, owned.brief, owned.send), (Owner::V2, Owner::Nobody, Owner::V2, Owner::V2, Owner::V2));
        assert_eq!(Owned::of(Mode { manager_on: true, taken_over: false }, &|_| true).send, Owner::Nobody);
        for id in RULES.iter().filter_map(|r| r.builtin()) {
            assert!(crate::playbooks::builtins::find(id).is_some(), "{id} is a built-in");
        }
        assert_eq!(rule_of_kind("session.activity"), Some(Rule::NeedsYou));
        assert_eq!(rule_of_kind("ci.failed"), None);
    }
}
