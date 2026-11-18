//! The last pull-request statuses and PR details Glasscode read, kept on disk
//! (`github-cache.json` in Glasscode's folder, readable by the person only), so that the
//! sidebar, Today and the PR panel show them the moment Glasscode starts.
//!
//! Display only: an entry was true when it was written and is asked of GitHub again soon after
//! the start; nothing acts on it before then (the app marks such a status unconfirmed). Each
//! entry names the account it was read as, and one read as another account than the
//! repository's now is not shown. The file is bounded in entries, age and size, and holds no
//! token.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::github::{CiStatus, PrDetails, PullRequest};

const VERSION: u32 = 1;
/// Kept at most: statuses (one per checkout), PR details (the most recently read).
const STATUSES_MAX: usize = 200;
const DETAILS_MAX: usize = 30;
/// An entry older than this is not shown (seconds).
const MAX_AGE: u64 = 7 * 24 * 3600;
/// A file larger than this is not read.
const FILE_MAX: u64 = 8 * 1024 * 1024;

/// A checkout's pull-request status, as last read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeptStatus {
    /// The account it was read as ([`account_label`]).
    pub account: String,
    pub repo: Option<String>,
    pub branch: String,
    pub pr: Option<PullRequest>,
    pub ci: Option<CiStatus>,
    /// When it was read (Unix seconds).
    pub at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeptDetails {
    pub account: String,
    /// `owner/name`.
    pub repo: String,
    pub at: u64,
    pub details: PrDetails,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub version: u32,
    /// By the checkout's folder.
    #[serde(default)]
    pub statuses: BTreeMap<String, KeptStatus>,
    #[serde(default)]
    pub details: Vec<KeptDetails>,
}

pub fn path() -> PathBuf {
    crate::paths::root().join("github-cache.json")
}

/// The account a repository is read as, for the cache: its project's or the default one, or ""
/// for gh's own (no token is needed to know it).
pub fn account_label(repo: &str) -> String {
    crate::github_accounts::account_for_repo(repo).unwrap_or_default()
}

/// What is kept and still young enough, for accounts as they are routed now. A file that is
/// missing, too large, of another version or not readable gives nothing.
pub fn load() -> Cache {
    let file = path();
    if std::fs::metadata(&file).map_or(true, |m| m.len() > FILE_MAX) {
        return Cache::default();
    }
    let Some(mut cache) = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<Cache>(&bytes).ok()) else {
        return Cache::default();
    };
    if cache.version != VERSION {
        return Cache::default();
    }
    let now = crate::model::now();
    let young = |at: u64| now.saturating_sub(at) < MAX_AGE;
    cache.statuses.retain(|_, s| young(s.at) && s.repo.as_deref().is_none_or(|repo| account_label(repo) == s.account));
    cache.details.retain(|d| young(d.at) && account_label(&d.repo) == d.account);
    cache
}

/// Keep `cache` (bounded: the newest statuses and details), written whole or not at all, for
/// the person only.
pub fn save(mut cache: Cache) -> std::io::Result<()> {
    cache.version = VERSION;
    if cache.statuses.len() > STATUSES_MAX {
        let mut ages: Vec<u64> = cache.statuses.values().map(|s| s.at).collect();
        ages.sort_unstable_by(|a, b| b.cmp(a));
        let cut = ages[STATUSES_MAX - 1];
        cache.statuses.retain(|_, s| s.at >= cut);
    }
    cache.details.sort_by(|a, b| b.at.cmp(&a.at));
    cache.details.truncate(DETAILS_MAX);
    let bytes = serde_json::to_vec(&cache).map_err(std::io::Error::other)?;
    let file = path();
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temp = file.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = options.open(&temp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
    }
    std::fs::rename(&temp, &file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(at: u64) -> KeptStatus {
        KeptStatus { account: String::new(), repo: Some("acme/app".into()), branch: "feat/x".into(), pr: None, ci: Some(CiStatus { head: "abc".into(), ..Default::default() }), at }
    }

    #[test]
    fn what_is_kept_comes_back_bounded_and_young() {
        crate::paths::use_test_root();
        let now = crate::model::now();
        let mut cache = Cache::default();
        for i in 0..(STATUSES_MAX as u64 + 5) {
            cache.statuses.insert(format!("/wt/{i}"), status(now - 10 - i));
        }
        cache.statuses.insert("/wt/old".into(), status(now - MAX_AGE - 1));
        cache.details.push(KeptDetails { account: String::new(), repo: "acme/app".into(), at: now, details: PrDetails { number: 7, ..Default::default() } });
        save(cache).unwrap();
        let back = load();
        assert_eq!(back.statuses.len(), STATUSES_MAX);
        assert!(!back.statuses.contains_key("/wt/old"));
        assert_eq!(back.statuses["/wt/0"].ci.as_ref().unwrap().head, "abc");
        assert_eq!(back.details[0].details.number, 7);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // An entry read as another account than the repository's now is not shown.
        let mut other = Cache::default();
        other.statuses.insert("/wt/a".into(), KeptStatus { account: "someone-else".into(), ..status(now) });
        save(other).unwrap();
        assert!(load().statuses.is_empty());
        // A file Glasscode cannot read gives nothing.
        std::fs::write(path(), b"{ not json").unwrap();
        assert!(load().statuses.is_empty());
    }
}
