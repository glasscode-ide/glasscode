//! Who runs each worktree, the Mac or the server, with a counter (the session runner epoch,
//! kept per worktree). glasscoded keeps these records and is their only writer; the switch asks
//! it to begin, commit or abort a move, and every runner checks the record before it starts an
//! agent, so a copy left behind by a move never runs again.
//!
//! A worktree with no record belongs to the machine whose disk holds it, at epoch 0.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{self, MachineId};
use crate::store::{OnDamage, Store};

/// How long a move may make no progress before the server gives it up (server clock).
pub const MOVE_STALE_MS: u64 = 120_000;

/// A worktree, named the same way on every machine: its path under the home folder (where a
/// move puts it, `sync::map_path`), with `/` between the parts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeKey(pub String);

impl WorktreeKey {
    /// The key of `worktree` on a machine whose home is `home`.
    pub fn of(worktree: &Path, home: &Path, project: &str, branch: Option<&str>) -> WorktreeKey {
        let rel = crate::sync::map_path(worktree, home, Path::new(""), project, branch);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        WorktreeKey(parts.join("/"))
    }

    /// Where this worktree is on a machine whose home is `home`.
    pub fn path_under(&self, home: &Path) -> PathBuf {
        self.0
            .split('/')
            .fold(home.to_path_buf(), |path, part| path.join(part))
    }

    /// A name for git refs and file names: no `/`.
    pub fn token(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.0.as_bytes());
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moving {
    pub to: MachineId,
    /// The epoch the worktree gets when the move commits.
    pub epoch: u64,
    /// When the move last made progress (milliseconds since the Unix epoch, server clock).
    pub since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerRecord {
    pub owner: MachineId,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moving: Option<Moving>,
}

/// Why a step of a move was refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum OwnerError {
    /// The worktree's epoch is no longer the one the caller saw.
    EpochChanged { current: u64 },
    /// Another move of this worktree is under way.
    Moving { to: MachineId },
    /// No move of this worktree at that epoch is under way.
    NotMoving,
    /// The caller does not own the worktree it wants to move.
    NotOwner { owner: MachineId },
}

impl std::fmt::Display for OwnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwnerError::EpochChanged { current } => write!(
                f,
                "this worktree moved meanwhile (it is at epoch {current} now)"
            ),
            OwnerError::Moving { .. } => write!(f, "this worktree is already being moved"),
            OwnerError::NotMoving => write!(f, "this move is no longer under way"),
            OwnerError::NotOwner { .. } => write!(f, "this worktree runs on the other machine now"),
        }
    }
}

impl std::error::Error for OwnerError {}

/// Every worktree that has moved at least once, by key (`owners.json` on the server).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owners {
    #[serde(default)]
    pub records: BTreeMap<WorktreeKey, OwnerRecord>,
}

impl Store for Owners {
    const NAME: &'static str = "owners";
    const VERSION: u32 = 1;
    // A lost file only forgets past moves: each worktree then belongs to where it is.
    const ON_DAMAGE: OnDamage = OnDamage::Empty;

    fn path() -> PathBuf {
        paths::owners_file()
    }
}

impl Owners {
    /// The record of `key`, or the one it has without a record: owned by `holder`, the machine
    /// whose disk holds it, at epoch 0.
    pub fn get(&self, key: &WorktreeKey, holder: &MachineId) -> OwnerRecord {
        self.records
            .get(key)
            .cloned()
            .unwrap_or_else(|| OwnerRecord {
                owner: holder.clone(),
                epoch: 0,
                moving: None,
            })
    }

    /// Whether `machine` may run agents in this worktree now: it owns it and no move is under
    /// way. `holder` is who owns it without a record.
    pub fn may_run(&self, key: &WorktreeKey, machine: &MachineId, holder: &MachineId) -> bool {
        let record = self.get(key, holder);
        &record.owner == machine && record.moving.is_none()
    }

    /// Start moving `key` from `from` to `to`; `seen` is the epoch the caller read. The worktree
    /// gets epoch `seen + 1` when the move commits. Beginning the same move again (a retry after
    /// a lost reply) is allowed and refreshes it.
    pub fn begin(
        &mut self,
        key: &WorktreeKey,
        from: &MachineId,
        to: &MachineId,
        seen: u64,
        holder: &MachineId,
        now_ms: u64,
    ) -> Result<OwnerRecord, OwnerError> {
        let mut record = self.get(key, holder);
        if let Some(moving) = &mut record.moving {
            if &record.owner == from
                && &moving.to == to
                && moving.epoch == seen + 1
                && record.epoch == seen
            {
                moving.since_ms = now_ms;
                self.records.insert(key.clone(), record.clone());
                return Ok(record);
            }
            return Err(OwnerError::Moving {
                to: moving.to.clone(),
            });
        }
        if record.epoch != seen {
            return Err(OwnerError::EpochChanged {
                current: record.epoch,
            });
        }
        if &record.owner != from {
            return Err(OwnerError::NotOwner {
                owner: record.owner,
            });
        }
        record.moving = Some(Moving {
            to: to.clone(),
            epoch: seen + 1,
            since_ms: now_ms,
        });
        self.records.insert(key.clone(), record.clone());
        Ok(record)
    }

    /// The move of `key` to epoch `epoch` is still under way: note the progress.
    pub fn touch(&mut self, key: &WorktreeKey, epoch: u64, now_ms: u64) -> Result<(), OwnerError> {
        match self.records.get_mut(key).and_then(|r| r.moving.as_mut()) {
            Some(moving) if moving.epoch == epoch => {
                moving.since_ms = now_ms;
                Ok(())
            }
            _ => Err(OwnerError::NotMoving),
        }
    }

    /// The machine the move of `key` to `epoch` goes to, if that move is under way.
    pub fn moving_to(&self, key: &WorktreeKey, epoch: u64) -> Option<&MachineId> {
        self.records
            .get(key)?
            .moving
            .as_ref()
            .filter(|m| m.epoch == epoch)
            .map(|m| &m.to)
    }

    /// Finish the move: the target owns the worktree at the new epoch. Committing a move that
    /// already committed is fine (a retry after a lost reply).
    pub fn commit(&mut self, key: &WorktreeKey, epoch: u64) -> Result<OwnerRecord, OwnerError> {
        let record = self.records.get_mut(key).ok_or(OwnerError::NotMoving)?;
        match record.moving.take() {
            Some(moving) if moving.epoch == epoch => {
                record.owner = moving.to;
                record.epoch = epoch;
                Ok(record.clone())
            }
            Some(other) => {
                record.moving = Some(other);
                Err(OwnerError::NotMoving)
            }
            None if record.epoch == epoch => Ok(record.clone()),
            None => Err(OwnerError::NotMoving),
        }
    }

    /// Give the move up: the worktree stays with its owner at its old epoch. Aborting a move
    /// that is no longer under way is fine.
    pub fn abort(&mut self, key: &WorktreeKey, epoch: u64) -> Option<OwnerRecord> {
        let record = self.records.get_mut(key)?;
        if record.moving.as_ref().is_some_and(|m| m.epoch == epoch) {
            record.moving = None;
        }
        Some(record.clone())
    }

    /// Moves that made no progress for [`MOVE_STALE_MS`]: given up, and returned with the epoch
    /// they would have had, so their held sessions can be removed.
    pub fn expire(&mut self, now_ms: u64) -> Vec<(WorktreeKey, u64)> {
        let mut gone = Vec::new();
        for (key, record) in &mut self.records {
            if let Some(moving) = &record.moving
                && now_ms.saturating_sub(moving.since_ms) > MOVE_STALE_MS
            {
                gone.push((key.clone(), moving.epoch));
                record.moving = None;
            }
        }
        gone
    }

    /// The server continued a worktree on its own (the fallback take-over from the mirror): it
    /// owns it at a new epoch, so the Mac's copy stops when the Mac next hears from it.
    pub fn take(&mut self, key: &WorktreeKey, to: &MachineId, holder: &MachineId) -> OwnerRecord {
        let mut record = self.get(key, holder);
        if &record.owner == to && record.moving.is_none() {
            return record;
        }
        record.epoch = record
            .moving
            .as_ref()
            .map_or(record.epoch, |m| m.epoch.max(record.epoch))
            + 1;
        record.owner = to.clone();
        record.moving = None;
        self.records.insert(key.clone(), record.clone());
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> MachineId {
        MachineId::parse(text).unwrap()
    }

    fn key() -> WorktreeKey {
        WorktreeKey(".glasscode/worktrees/app/fix-retry".into())
    }

    #[test]
    fn a_worktree_without_a_record_belongs_to_where_it_is() {
        let owners = Owners::default();
        let record = owners.get(&key(), &id("mac"));
        assert_eq!(
            record,
            OwnerRecord {
                owner: id("mac"),
                epoch: 0,
                moving: None
            }
        );
        assert!(owners.may_run(&key(), &id("mac"), &id("mac")));
        assert!(!owners.may_run(&key(), &id("server"), &id("mac")));
    }

    #[test]
    fn a_move_commits_to_the_next_epoch() {
        let mut owners = Owners::default();
        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 1_000)
            .unwrap();
        // While it moves, nobody runs it.
        assert!(!owners.may_run(&key(), &id("mac"), &id("mac")));
        assert!(!owners.may_run(&key(), &id("server"), &id("mac")));
        let record = owners.commit(&key(), 1).unwrap();
        assert_eq!(
            (record.owner, record.epoch, record.moving),
            (id("server"), 1, None)
        );
        assert!(owners.may_run(&key(), &id("server"), &id("mac")));
        assert!(!owners.may_run(&key(), &id("mac"), &id("mac")));
    }

    #[test]
    fn retries_of_begin_and_commit_are_harmless() {
        let mut owners = Owners::default();
        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 1_000)
            .unwrap();
        let again = owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 2_000)
            .unwrap();
        assert_eq!(again.moving.unwrap().since_ms, 2_000);
        owners.commit(&key(), 1).unwrap();
        assert_eq!(owners.commit(&key(), 1).unwrap().epoch, 1);
    }

    #[test]
    fn a_stale_epoch_or_a_second_move_is_refused() {
        let mut owners = Owners::default();
        assert_eq!(
            owners.begin(&key(), &id("mac"), &id("server"), 3, &id("mac"), 0),
            Err(OwnerError::EpochChanged { current: 0 })
        );
        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 0)
            .unwrap();
        assert!(matches!(
            owners.begin(&key(), &id("other-mac"), &id("server"), 0, &id("mac"), 0),
            Err(OwnerError::Moving { .. })
        ));
        assert!(matches!(
            owners.begin(&key(), &id("server"), &id("mac"), 0, &id("mac"), 0),
            Err(OwnerError::Moving { .. })
        ));
        owners.commit(&key(), 1).unwrap();
        // The Mac no longer owns it, so it cannot move it.
        assert_eq!(
            owners.begin(&key(), &id("mac"), &id("server"), 1, &id("mac"), 0),
            Err(OwnerError::NotOwner {
                owner: id("server")
            })
        );
    }

    #[test]
    fn an_aborted_or_stale_move_leaves_the_owner() {
        let mut owners = Owners::default();
        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 1_000)
            .unwrap();
        let record = owners.abort(&key(), 1).unwrap();
        assert_eq!(
            (record.owner, record.epoch, record.moving),
            (id("mac"), 0, None)
        );
        assert_eq!(owners.commit(&key(), 1), Err(OwnerError::NotMoving));

        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 1_000)
            .unwrap();
        assert!(owners.expire(1_000 + MOVE_STALE_MS).is_empty());
        owners.touch(&key(), 1, 50_000).unwrap();
        assert!(
            owners.expire(1_000 + MOVE_STALE_MS + 1).is_empty(),
            "progress keeps it alive"
        );
        assert_eq!(owners.expire(50_000 + MOVE_STALE_MS + 1), vec![(key(), 1)]);
        assert!(owners.may_run(&key(), &id("mac"), &id("mac")));
    }

    #[test]
    fn a_take_over_raises_the_epoch_past_any_move() {
        let mut owners = Owners::default();
        owners
            .begin(&key(), &id("mac"), &id("server"), 0, &id("mac"), 0)
            .unwrap();
        let record = owners.take(&key(), &id("server"), &id("mac"));
        assert_eq!((record.owner, record.epoch), (id("server"), 2));
        // A late commit of the old move changes nothing.
        assert_eq!(owners.commit(&key(), 1), Err(OwnerError::NotMoving));
        assert_eq!(owners.take(&key(), &id("server"), &id("mac")).epoch, 2);
    }

    #[test]
    fn keys_match_on_both_machines() {
        let mac = WorktreeKey::of(
            Path::new("/Users/alex/.glasscode/worktrees/app/fix-retry"),
            Path::new("/Users/alex"),
            "app",
            Some("fix/retry"),
        );
        let server = WorktreeKey::of(
            Path::new("/home/dev/.glasscode/worktrees/app/fix-retry"),
            Path::new("/home/dev"),
            "app",
            Some("fix/retry"),
        );
        assert_eq!(mac, server);
        assert_eq!(mac.0, ".glasscode/worktrees/app/fix-retry");
        assert_eq!(
            mac.path_under(Path::new("/home/dev")),
            PathBuf::from("/home/dev/.glasscode/worktrees/app/fix-retry")
        );
        // Outside the home folder, the place a move puts it.
        let outside = WorktreeKey::of(
            Path::new("/opt/code/app"),
            Path::new("/Users/alex"),
            "App",
            Some("main"),
        );
        assert_eq!(outside.0, ".glasscode/worktrees/App/main");
        assert_eq!(mac.token().len(), 16);
    }
}
