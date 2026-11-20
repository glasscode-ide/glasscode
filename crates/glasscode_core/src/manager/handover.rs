//! Who does each of the manager's rules, in each mode: the hand-over from v1's harness to v2
//! (M40 part 2, M44).
//!
//! | rule | engine v1, or v2 before it takes over | v2, taken over |
//! |---|---|---|
//! | needs-you | v1's harness (`needs|<session>`) | the native built-in `needs-you` |
//! | review-tell | v1's harness (`tell|…`, `tell_sessions`) | the native built-in `review-tell`, on the `review.received` events v1's own reader writes |
//! | follow-ups | v1's harness | the built-in `follow-ups`: v1's `followups` rules, run for v2 |
//! | daily brief | v1's harness | the built-in `daily-brief`: v1's brief, run for v2 |
//! | stuck session, usage | nobody (v1 has none) | the native built-ins `stuck-session`, `usage-handoff` |
//! | sending | v1's outbox in `data.json` | v2's outbox (`manager/outbox.json`), each send claimed (`messages::send_intent`) |
//! | checks | v1's harness | v1's harness, until the check's schedule (`v1-check-<id>`) is approved |
//! | inbox | v1's harness | v1's reader, its holding replies in v2's outbox |
//!
//! With the manager off, nobody does any of them. A built-in the person switched off in v2 is
//! nobody's: v1 does not take it back. v2 takes over only on the home, once this launch moved
//! v1's data (M44) and the journal runs; until then v1 keeps every rule, as before.

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
    Checks,
    Inbox,
}

/// Every rule.
pub const RULES: [Rule; 9] = [Rule::NeedsYou, Rule::ReviewTell, Rule::FollowUps, Rule::Brief, Rule::Stuck, Rule::Usage, Rule::Send, Rule::Checks, Rule::Inbox];

/// Who does a rule now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    V1,
    V2,
    Nobody,
}

/// The mode the manager is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    pub engine_v2: bool,
    pub manager_on: bool,
    /// v2 took over on this machine in this launch: it is the home, v1's data moved (M44), and
    /// the journal runs.
    pub taken_over: bool,
}

impl Rule {
    /// The built-in that stands for it in v2 (its entry in the playbooks' file switches it).
    pub fn builtin(self) -> Option<&'static str> {
        match self {
            Rule::NeedsYou => Some("needs-you"),
            Rule::ReviewTell => Some("review-tell"),
            Rule::FollowUps => Some("follow-ups"),
            Rule::Brief => Some("daily-brief"),
            Rule::Stuck => Some("stuck-session"),
            Rule::Usage => Some("usage-handoff"),
            Rule::Send | Rule::Checks | Rule::Inbox => None,
        }
    }

    /// Whether v1's harness has it at all.
    fn in_v1(self) -> bool {
        !matches!(self, Rule::Stuck | Rule::Usage)
    }
}

/// Who does `rule` in `mode`; `switched_on` says whether a built-in is on (its entry).
pub fn owner(rule: Rule, mode: Mode, switched_on: &dyn Fn(&str) -> bool) -> Owner {
    if !mode.manager_on {
        return Owner::Nobody;
    }
    if !mode.engine_v2 || !mode.taken_over {
        return if rule.in_v1() { Owner::V1 } else { Owner::Nobody };
    }
    match rule {
        Rule::Checks | Rule::Inbox => Owner::V1,
        Rule::Send => Owner::V2,
        _ => match rule.builtin() {
            Some(id) if switched_on(id) => Owner::V2,
            _ => Owner::Nobody,
        },
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

/// Who does each rule of v1's harness, for one round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owned {
    pub needs_you: Owner,
    pub review_tell: Owner,
    pub follow_ups: Owner,
    pub brief: Owner,
    pub send: Owner,
    pub checks: Owner,
}

impl Owned {
    /// Everything v1's, as with the engine at v1.
    pub const V1: Owned = Owned { needs_you: Owner::V1, review_tell: Owner::V1, follow_ups: Owner::V1, brief: Owner::V1, send: Owner::V1, checks: Owner::V1 };

    pub fn of(mode: Mode, switched_on: &dyn Fn(&str) -> bool) -> Owned {
        let of = |rule| owner(rule, mode, switched_on);
        Owned { needs_you: of(Rule::NeedsYou), review_tell: of(Rule::ReviewTell), follow_ups: of(Rule::FollowUps), brief: of(Rule::Brief), send: of(Rule::Send), checks: of(Rule::Checks) }
    }

    /// Whether v2 has any of them (it took over).
    pub fn v2(&self) -> bool {
        [self.needs_you, self.review_tell, self.follow_ups, self.brief, self.send, self.checks].contains(&Owner::V2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule in every mode, with its built-in on and off.
    #[test]
    fn who_owns_each_rule_in_each_mode() {
        for rule in RULES {
            for engine_v2 in [false, true] {
                for manager_on in [false, true] {
                    for taken_over in [false, true] {
                        for on in [false, true] {
                            let mode = Mode { engine_v2, manager_on, taken_over };
                            let got = owner(rule, mode, &|_| on);
                            let want = if !manager_on {
                                Owner::Nobody
                            } else if !engine_v2 || !taken_over {
                                // v1 keeps what it has, whatever the v2 switches say.
                                if matches!(rule, Rule::Stuck | Rule::Usage) { Owner::Nobody } else { Owner::V1 }
                            } else {
                                match rule {
                                    Rule::Checks | Rule::Inbox => Owner::V1,
                                    Rule::Send => Owner::V2,
                                    // A built-in switched off is nobody's: v1 does not take it back.
                                    _ if on => Owner::V2,
                                    _ => Owner::Nobody,
                                }
                            };
                            assert_eq!(got, want, "{rule:?} in {mode:?}, built-in on: {on}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn one_switch_per_built_in() {
        let mode = Mode { engine_v2: true, manager_on: true, taken_over: true };
        let owned = Owned::of(mode, &|id| id != "review-tell");
        assert_eq!((owned.needs_you, owned.review_tell, owned.follow_ups, owned.brief, owned.send, owned.checks), (Owner::V2, Owner::Nobody, Owner::V2, Owner::V2, Owner::V2, Owner::V1));
        assert!(owned.v2() && !Owned::V1.v2());
        assert_eq!(Owned::of(Mode { engine_v2: true, manager_on: true, taken_over: false }, &|_| true), Owned::V1);
        for id in RULES.iter().filter_map(|r| r.builtin()) {
            assert!(crate::playbooks::builtins::find(id).is_some(), "{id} is a built-in");
        }
        assert_eq!(rule_of_kind("session.activity"), Some(Rule::NeedsYou));
        assert_eq!(rule_of_kind("ci.failed"), None);
    }
}
