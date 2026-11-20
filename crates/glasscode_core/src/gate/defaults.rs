//! The default table (trust-11): per op group (and, where a group holds ops of very different
//! weight, per effect), per level and per column, the rule a non-person caller gets when nothing
//! else decides, and the ceiling no grant can pass. It is data, so Settings (M53) shows this same
//! table, and a test fails when an op of the registry has no row.
//!
//! The person-started sessions do not read it: they keep the rules of Settings › Permissions
//! (the 13 kinds of `state.json`, exec-7). A program outside Glasscode does not either: it keeps
//! [`crate::caller::outside_rule`].

use serde::Serialize;

use super::levels::{Column, Level};
use crate::control::{Kind, Rule};
use crate::ops::{EffectClass, OpGroup, OpSpec};

/// Which ops of its group a row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effects {
    /// Every op of the group.
    Any,
    /// The ops of the group with one of these effects.
    Only(&'static [EffectClass]),
    /// The ops of any group that move work between machines or accounts (v1's kind Move): a
    /// hand-off, a move to the server and back.
    Moves,
}

/// What a cell allows when nothing else decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Base {
    Rule(Rule),
    /// Allow for the sessions this caller started, ask for any other.
    StartedByCaller,
    /// Allow where the forge carries the wall for the project (D29: the default branch blocks
    /// force-push, requires review, with no bypass); ask elsewhere, and while the facts are unknown.
    ForgeWall,
}

/// One cell: the rule when nothing else decides, and the most a grant can lift it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub base: Base,
    pub ceiling: Rule,
}

/// One row of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Row {
    pub group: OpGroup,
    pub effects: Effects,
    /// What the ops of the row do, after "the manager asks before it …".
    pub what: &'static str,
    /// Careful, for the manager and for an automation session.
    pub careful: (Cell, Cell),
    pub standard: Cell,
    pub autopilot: Cell,
    /// What Settings says beside the row, if anything.
    pub note: &'static str,
}

const fn rule(base: Rule, ceiling: Rule) -> Cell {
    Cell { base: Base::Rule(base), ceiling }
}

const ALLOW: Cell = rule(Rule::Allow, Rule::Allow);
/// Asks; a grant from the person's own words in chat may lift it.
const ASK: Cell = rule(Rule::Ask, Rule::Allow);
/// Asks, always: a card with the full payload.
const CARD: Cell = rule(Rule::Ask, Rule::Ask);
const NEVER: Cell = rule(Rule::Never, Rule::Never);

const fn row(group: OpGroup, effects: Effects, what: &'static str, careful: (Cell, Cell), standard: Cell, autopilot: Cell) -> Row {
    Row { group, effects, what, careful, standard, autopilot, note: "" }
}

const fn same(group: OpGroup, what: &'static str, cell: Cell) -> Row {
    row(group, Effects::Any, what, (cell, cell), cell, cell)
}

use EffectClass::{Create, Destructive, LocalWrite};
use OpGroup::*;

/// The table, in the order it is read: the first row that matches an op decides.
pub const TABLE: &[Row] = &[
    same(Read, "reads", ALLOW),
    same(Navigate, "changes what the window shows", ALLOW),
    same(ForgeRead, "reads pull requests and checks", ALLOW),
    same(Board, "keeps the board", ALLOW),
    same(Manager, "keeps its own follow-ups and checks", ALLOW),
    row(Memory, Effects::Only(&[Destructive]), "forgets a note", (CARD, CARD), ALLOW, ALLOW),
    same(Memory, "notes and proposes memory", ALLOW),
    same(Settings, "changes settings", NEVER),
    Row { note: "routine pings follow the send policy (M42)", ..same(Message, "sends a message", CARD) },
    same(Browser, "acts in the browser", CARD),
    // Before the groups that hold them: a hand-off, a move to the server and back.
    Row { group: SessionStart, effects: Effects::Moves, note: "a hand-off between accounts is offered, never automatic (D17)", ..same(SessionStart, "hands off or moves work", CARD) },
    same(Worktree, "hands off or moves work", CARD).moves(),
    row(SessionStart, Effects::Any, "starts a session", (ASK, NEVER), ALLOW, ALLOW),
    row(SessionInput, Effects::Any, "types into a session", (ASK, ASK), Cell { base: Base::StartedByCaller, ceiling: Rule::Allow }, ALLOW),
    Row { note: "a session's mode is never raised", ..row(SessionControl, Effects::Any, "closes, restarts or renames a session", (ASK, ASK), ASK, ALLOW) },
    row(Worktree, Effects::Only(&[Destructive]), "removes or cleans up worktrees", (CARD, CARD), ASK, ASK),
    row(Worktree, Effects::Only(&[Create]), "makes a worktree", (ASK, NEVER), ALLOW, ALLOW),
    row(Worktree, Effects::Only(&[LocalWrite]), "renames a worktree", (ASK, ASK), ALLOW, ALLOW),
    // Any other worktree op (none yet) asks, a card at Careful.
    row(Worktree, Effects::Any, "changes a worktree", (CARD, CARD), ASK, ASK),
    row(Project, Effects::Any, "imports or removes a project", (CARD, CARD), ASK, ASK),
    row(Tickets, Effects::Any, "moves or comments on tickets", (CARD, CARD), CARD, ALLOW),
    Row { note: "only where the forge's rules guard merge and force-push (D29)", ..row(ForgeWrite, Effects::Any, "comments, labels, re-runs checks or opens a draft", (CARD, CARD), CARD, Cell { base: Base::ForgeWall, ceiling: Rule::Allow }) },
];

impl Row {
    /// The same row, for the moves of its group only.
    const fn moves(self) -> Row {
        Row { effects: Effects::Moves, ..self }
    }

    /// Whether the row is about `spec`. The rows of moves come before the other rows of their
    /// group, so those need not leave moves out.
    pub fn covers(&self, spec: &OpSpec) -> bool {
        self.group == spec.group
            && match self.effects {
                Effects::Moves => spec.kind_v1 == Kind::Move,
                Effects::Any => true,
                Effects::Only(effects) => effects.contains(&spec.effect),
            }
    }

    pub fn cell(&self, level: Level, column: Column) -> Cell {
        match (level, column) {
            (Level::Careful, Column::Manager) => self.careful.0,
            (Level::Careful, Column::Automation) => self.careful.1,
            (Level::Standard, _) => self.standard,
            (Level::Autopilot, _) => self.autopilot,
        }
    }
}

/// The row of `spec`: the first that covers it. Every op of the registry has one (a test).
pub fn row_of(spec: &OpSpec) -> Option<&'static Row> {
    TABLE.iter().find(|row| row.covers(spec))
}

/// Every group of the registry, for the coverage test: matched without a catch-all, so a new
/// group cannot be added without its row.
pub fn all_groups() -> [OpGroup; 16] {
    let groups = [Read, Navigate, Board, Memory, SessionStart, SessionInput, SessionControl, Worktree, Project, Settings, Tickets, ForgeRead, ForgeWrite, Message, Browser, Manager];
    for group in groups {
        match group {
            Read | Navigate | Board | Memory | SessionStart | SessionInput | SessionControl | Worktree | Project | Settings | Tickets | ForgeRead | ForgeWrite | Message | Browser | Manager => {}
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{OPS, OpHost, Retry, Undo};

    const EFFECTS: [EffectClass; 7] = [EffectClass::Read, EffectClass::Navigate, EffectClass::LocalWrite, EffectClass::SessionInput, EffectClass::Create, EffectClass::Destructive, EffectClass::Outward];

    fn spec(group: OpGroup, effect: EffectClass, kind: Kind) -> OpSpec {
        OpSpec { name: "test.op", group, effect, kind_v1: kind, retry: Retry::Idempotent, undo: Undo::None, host: OpHost::Core, ..OpSpec::READ }
    }

    /// Every group, with every effect and as a move, has a row; so does every op of the registry.
    #[test]
    fn every_op_group_level_and_column_has_a_cell() {
        for group in all_groups() {
            for effect in EFFECTS {
                for kind in [Kind::Read, Kind::Move] {
                    let spec = spec(group, effect, kind);
                    let row = row_of(&spec).unwrap_or_else(|| panic!("{group:?} {effect:?} {kind:?} has no row"));
                    for level in Level::ALL {
                        for column in [Column::Manager, Column::Automation] {
                            let cell = row.cell(level, column);
                            // A ceiling is never stricter than its base, which it would hide.
                            if let Base::Rule(base) = cell.base {
                                assert!(super::super::levels::strictness(cell.ceiling) <= super::super::levels::strictness(base), "{group:?} {level:?} {column:?}");
                            }
                        }
                    }
                }
            }
        }
        for op in OPS {
            assert!(row_of(op).is_some(), "{} has no row", op.name);
        }
    }

    /// The table of the slice text (M21.1 step 2), cell by cell where it says one.
    #[test]
    fn the_slice_table() {
        let base = |group, effect, kind, level, column| row_of(&spec(group, effect, kind)).unwrap().cell(level, column).base;
        use Column::{Automation as A, Manager as M};
        use Level::{Autopilot as P, Careful as C, Standard as S};
        let allow = Base::Rule(Rule::Allow);
        let ask = Base::Rule(Rule::Ask);
        let never = Base::Rule(Rule::Never);
        assert_eq!(base(SessionStart, Create, Kind::Start, C, M), ask);
        assert_eq!(base(SessionStart, Create, Kind::Start, C, A), never);
        assert_eq!(base(SessionStart, Create, Kind::Start, S, M), allow);
        assert_eq!(base(SessionStart, Create, Kind::Start, P, A), allow);
        assert_eq!(base(SessionInput, EffectClass::SessionInput, Kind::Send, S, M), Base::StartedByCaller);
        assert_eq!(base(SessionControl, Destructive, Kind::Close, S, M), ask);
        assert_eq!(base(SessionControl, Destructive, Kind::Close, P, M), allow);
        for level in Level::ALL {
            assert_eq!(base(Worktree, Destructive, Kind::Remove, level, M), ask);
            assert_eq!(base(Settings, LocalWrite, Kind::Settings, level, M), never);
            assert_eq!(base(Message, EffectClass::Outward, Kind::Board, level, M), ask);
            assert_eq!(base(Browser, EffectClass::Outward, Kind::Show, level, M), ask);
            assert_eq!(base(Read, EffectClass::Read, Kind::Read, level, A), allow);
            assert_eq!(base(Board, LocalWrite, Kind::Board, level, A), allow);
            // A hand-off or a move to the server always asks.
            assert_eq!(base(SessionStart, Create, Kind::Move, level, M), ask);
            assert_eq!(base(Worktree, Create, Kind::Move, level, M), ask);
        }
        assert_eq!(base(Tickets, EffectClass::Outward, Kind::Tickets, P, M), allow);
        assert_eq!(base(ForgeWrite, EffectClass::Outward, Kind::Github, P, M), Base::ForgeWall);
        assert_eq!(base(ForgeWrite, EffectClass::Outward, Kind::Github, S, M), ask);
    }
}
