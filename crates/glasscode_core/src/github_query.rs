//! The questions Glasscode asks GitHub itself (`github_api`), as GraphQL queries, and their
//! answers in the shapes `gh … --json` prints, so that `github`'s parsers read both alike.
//!
//! [`look_up`] is the pull-request status of many checkouts at once: one query per account and
//! repository for all their branches, the pull requests of review sessions, and the ones watched
//! for a merge (about 1 point of GitHub's 5,000 an hour, where asking `gh` took a call for each
//! branch tried and one for each PR's checks).

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::github::{self, CiStatus, Merged, PullRequest};
use crate::github_api::{self, ApiError, Auth};

/// A commit's checks: its check runs and commit statuses, 100 at a time.
const CHECKS: &str = "fragment Checks on Commit { id oid statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage endCursor } nodes { ...Context } } } }";
const CONTEXT: &str = "fragment Context on StatusCheckRollupContext { __typename ... on CheckRun { name status conclusion detailsUrl startedAt completedAt checkSuite { workflowRun { workflow { name } } } } ... on StatusContext { context state targetUrl createdAt } }";
/// What [`PullRequest`] holds.
const PR_LITE: &str = "fragment PrLite on PullRequest { number title url baseRefName headRefName isDraft state author { login } reviewRequests { totalCount } latestReviews(first: 20) { nodes { author { login } state submittedAt } } }";
/// [`PullRequest`] and its [`CiStatus`].
const PR: &str = "fragment Pr on PullRequest { ...PrLite headRefOid baseRefOid mergeable commits(last: 1) { nodes { commit { ...Checks } } } }";
/// What [`Merged`] needs.
const MERGE: &str = "fragment Merge on PullRequest { state mergedAt additions deletions }";

/// The most aliases in one query. GitHub answers larger ones, but takes longer (each branch
/// brings its checks) and one failure fails them all; smaller queries go side by side.
const ALIASES_MAX: usize = 12;
/// The most queries asked at the same time (GitHub allows 100; a few keep its secondary limits
/// far away).
const PARALLEL_MAX: usize = 6;
/// The most pages of checks read for one commit (100 each).
const CHECK_PAGES_MAX: usize = 10;

fn owner_name(repo: &str) -> Option<(&str, &str)> {
    repo.split_once('/').filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

// ----- checks, in gh's shape ---------------------------------------------------------------------

/// One check as `gh pr view --json statusCheckRollup` prints it.
fn context_json(node: &Value) -> Value {
    if node["__typename"] == "StatusContext" {
        json!({
            "__typename": "StatusContext",
            "context": node["context"],
            "state": node["state"],
            "targetUrl": node["targetUrl"],
            "startedAt": node["createdAt"],
        })
    } else {
        json!({
            "__typename": node["__typename"],
            "name": node["name"],
            "status": node["status"],
            "conclusion": node["conclusion"],
            "detailsUrl": node["detailsUrl"],
            "startedAt": node["startedAt"],
            "completedAt": node["completedAt"],
            "workflowName": node["checkSuite"]["workflowRun"]["workflow"]["name"].as_str().unwrap_or_default(),
        })
    }
}

/// All the checks of a commit (a `Checks` fragment), the pages after the first read too: a PR
/// with more than 100 checks keeps its failures.
fn rollup(auth: &Auth, env: &HashMap<String, String>, commit: &Value) -> Value {
    let contexts = &commit["statusCheckRollup"]["contexts"];
    let mut all: Vec<Value> = contexts["nodes"].as_array().into_iter().flatten().map(context_json).collect();
    let mut page = contexts["pageInfo"].clone();
    let id = text(&commit["id"]);
    let mut pages = 1;
    while page["hasNextPage"].as_bool() == Some(true) && !id.is_empty() && pages < CHECK_PAGES_MAX {
        let query = format!(
            "query($id: ID!, $after: String) {{ node(id: $id) {{ ... on Commit {{ statusCheckRollup {{ contexts(first: 100, after: $after) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ ...Context }} }} }} }} }} }} {CONTEXT}"
        );
        match github_api::graphql(auth, env, &query, json!({ "id": id, "after": page["endCursor"] })) {
            Ok(data) => {
                let more = &data["node"]["statusCheckRollup"]["contexts"];
                all.extend(more["nodes"].as_array().into_iter().flatten().map(context_json));
                page = more["pageInfo"].clone();
            }
            Err(err) => {
                log::warn!("could not read all the checks of {}: {err}", text(&commit["oid"]));
                break;
            }
        }
        pages += 1;
    }
    Value::Array(all)
}

/// A pull request (`Pr` fields) as `gh pr view --json number,title,…,CI_FIELDS` prints it.
fn gh_shaped_pr(auth: &Auth, env: &HashMap<String, String>, node: &Value) -> Value {
    let mut pr = node.clone();
    let commit = &node["commits"]["nodes"][0]["commit"];
    pr["statusCheckRollup"] = rollup(auth, env, commit);
    if let Some(map) = pr.as_object_mut() {
        map.remove("commits");
    }
    pr
}

fn pull_request(node: &Value) -> Option<PullRequest> {
    serde_json::from_value(node.clone()).ok()
}

// ----- the status of many checkouts --------------------------------------------------------------

/// What to find for one checkout: the open pull request of the first of `candidates` that has one
/// (`github::pr_branch_candidates`), with its checks; or, for a review session, pull request
/// `number` (any state, no checks).
#[derive(Clone, Debug, PartialEq)]
pub struct Lookup {
    /// The caller's name for it (a worktree's folder).
    pub key: String,
    /// `owner/name`.
    pub repo: String,
    pub candidates: Vec<String>,
    pub number: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub pr: Option<PullRequest>,
    pub ci: Option<CiStatus>,
}

/// The answers of [`look_up`]: for each lookup's key, and for each watched pull request whether
/// it merged. A lookup that failed is an error: its caller keeps what it knew.
#[derive(Debug, Default)]
pub struct Answers {
    pub found: HashMap<String, Result<Found, String>>,
    pub merged: HashMap<(String, u64), Result<Option<Merged>, String>>,
    /// Queries sent to GitHub (not counting `gh` calls for repositories read without a token).
    pub queries: usize,
}

/// One query's worth of a group: aliases for branches (`h`), review PRs (`n`) and merges (`m`).
#[derive(Debug, Default, PartialEq)]
struct Chunk {
    branches: Vec<String>,
    numbers: Vec<u64>,
    merges: Vec<u64>,
}

impl Chunk {
    fn len(&self) -> usize {
        self.branches.len() + self.numbers.len() + self.merges.len()
    }
}

/// Split what a repository asks for into queries of at most `max` aliases.
fn chunks(mut branches: Vec<String>, mut numbers: Vec<u64>, mut merges: Vec<u64>, max: usize) -> Vec<Chunk> {
    branches.dedup();
    numbers.sort_unstable();
    numbers.dedup();
    merges.sort_unstable();
    merges.dedup();
    let mut out = vec![Chunk::default()];
    for branch in branches {
        if out.last().unwrap().len() >= max {
            out.push(Chunk::default());
        }
        out.last_mut().unwrap().branches.push(branch);
    }
    for number in numbers {
        if out.last().unwrap().len() >= max {
            out.push(Chunk::default());
        }
        out.last_mut().unwrap().numbers.push(number);
    }
    for number in merges {
        if out.last().unwrap().len() >= max {
            out.push(Chunk::default());
        }
        out.last_mut().unwrap().merges.push(number);
    }
    out.retain(|c| c.len() > 0);
    out
}

/// The query for one chunk, and its variables. Branch names go as variables; numbers are
/// numbers. Only the fragments it uses are in it (GraphQL refuses an unused one).
fn chunk_query(owner: &str, name: &str, chunk: &Chunk) -> (String, Value) {
    let mut params = vec!["$owner: String!".to_string(), "$name: String!".to_string()];
    let mut fields = Vec::new();
    let mut variables = json!({ "owner": owner, "name": name });
    for (i, branch) in chunk.branches.iter().enumerate() {
        params.push(format!("$h{i}: String!"));
        variables[format!("h{i}")] = json!(branch);
        fields.push(format!("h{i}: pullRequests(headRefName: $h{i}, states: OPEN, first: 1, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ ...Pr }} }}"));
    }
    for (i, number) in chunk.numbers.iter().enumerate() {
        fields.push(format!("n{i}: pullRequest(number: {number}) {{ ...PrLite }}"));
    }
    for (i, number) in chunk.merges.iter().enumerate() {
        fields.push(format!("m{i}: pullRequest(number: {number}) {{ ...Merge }}"));
    }
    let mut fragments = Vec::new();
    if !chunk.branches.is_empty() {
        fragments.extend([PR, CHECKS, CONTEXT]);
    }
    if !chunk.branches.is_empty() || !chunk.numbers.is_empty() {
        fragments.push(PR_LITE);
    }
    if !chunk.merges.is_empty() {
        fragments.push(MERGE);
    }
    let query = format!("query({}) {{ rateLimit {{ cost }} repository(owner: $owner, name: $name) {{ {} }} }} {}", params.join(", "), fields.join(" "), fragments.join(" "));
    (query, variables)
}

/// The pull-request status of many checkouts, and whether watched pull requests merged. One
/// query per account and repository (more only past [`ALIASES_MAX`] questions); a repository
/// with no token to be had is asked through `gh`, one call at a time, as before.
pub fn look_up(lookups: &[Lookup], watched: &[(String, u64)], env: &HashMap<String, String>) -> Answers {
    let mut answers = Answers::default();
    let mut groups: Vec<Group> = Vec::new();
    let mut auths: HashMap<String, Option<Auth>> = HashMap::new();
    let mut auth_of = |repo: &str| auths.entry(repo.to_lowercase()).or_insert_with(|| github_api::auth_for(repo, env)).clone();
    let mut by_gh: Vec<&Lookup> = Vec::new();
    let mut merges_by_gh: Vec<&(String, u64)> = Vec::new();
    for lookup in lookups {
        match auth_of(&lookup.repo) {
            Some(auth) => match groups.iter_mut().find(|g| g.auth.account == auth.account && g.repo.eq_ignore_ascii_case(&lookup.repo)) {
                Some(group) => group.lookups.push(lookup.clone()),
                None => groups.push(Group { auth, repo: lookup.repo.clone(), lookups: vec![lookup.clone()], merges: Vec::new() }),
            },
            None => by_gh.push(lookup),
        }
    }
    for pr in watched {
        match auth_of(&pr.0) {
            Some(auth) => match groups.iter_mut().find(|g| g.auth.account == auth.account && g.repo.eq_ignore_ascii_case(&pr.0)) {
                Some(group) => group.merges.push(pr.1),
                None => groups.push(Group { auth, repo: pr.0.clone(), lookups: Vec::new(), merges: vec![pr.1] }),
            },
            None => merges_by_gh.push(pr),
        }
    }

    // Every query of every group, side by side (a few at a time).
    let plans: Vec<(usize, Chunk)> = groups.iter().enumerate().flat_map(|(index, group)| plan(group).into_iter().map(move |chunk| (index, chunk))).collect();
    let groups_ref = &groups;
    let replies: Vec<Result<Value, String>> = std::thread::scope(|scope| {
        let mut replies = Vec::new();
        for batch in plans.chunks(PARALLEL_MAX) {
            let running: Vec<_> = batch.iter().map(|(index, chunk)| scope.spawn(move || ask(&groups_ref[*index], chunk, env))).collect();
            replies.extend(running.into_iter().map(|thread| thread.join().unwrap_or_else(|_| Err("the lookup stopped".to_string()))));
        }
        replies
    });
    answers.queries += plans.len();
    for (index, group) in groups.iter().enumerate() {
        let mine: Vec<(&Chunk, &Result<Value, String>)> = plans.iter().zip(&replies).filter(|((i, _), _)| *i == index).map(|((_, chunk), reply)| (chunk, reply)).collect();
        let part = read_group(group, &mine, env);
        answers.found.extend(part.found);
        answers.merged.extend(part.merged);
    }

    // No token for these: `gh`, one call at a time.
    for lookup in by_gh {
        let found = match lookup.number {
            Some(number) => github::pr_by_number_gh(&lookup.repo, number, env).map(|pr| Found { pr, ci: None }),
            None => github::pr_for_candidates_gh(&lookup.repo, &lookup.candidates, env).map(|pr| {
                let ci = pr.as_ref().and_then(|pr| github::pr_ci_gh(&lookup.repo, pr.number, env).ok());
                Found { pr, ci }
            }),
        };
        answers.found.insert(lookup.key.clone(), found.map_err(|e| format!("{e:#}")));
    }
    for (repo, number) in merges_by_gh {
        answers.merged.insert((repo.clone(), *number), github::pr_merged_gh(repo, *number, env).map_err(|e| format!("{e:#}")));
    }
    answers
}

/// What one account asks of one repository in [`look_up`].
struct Group {
    auth: Auth,
    repo: String,
    lookups: Vec<Lookup>,
    merges: Vec<u64>,
}

/// The queries of one group: its branches, review PRs and merges, [`ALIASES_MAX`] at most each.
fn plan(group: &Group) -> Vec<Chunk> {
    if owner_name(&group.repo).is_none() {
        return Vec::new();
    }
    let mut branches: Vec<String> = Vec::new();
    for lookup in group.lookups.iter().filter(|l| l.number.is_none()) {
        for candidate in &lookup.candidates {
            if !branches.contains(candidate) {
                branches.push(candidate.clone());
            }
        }
    }
    let numbers: Vec<u64> = group.lookups.iter().filter_map(|l| l.number).collect();
    chunks(branches, numbers, group.merges.clone(), ALIASES_MAX)
}

/// One query of a group: its `repository` part, or why there is none.
fn ask(group: &Group, chunk: &Chunk, env: &HashMap<String, String>) -> Result<Value, String> {
    let (owner, name) = owner_name(&group.repo).ok_or_else(|| format!("not a GitHub repository: {}", group.repo))?;
    let (query, variables) = chunk_query(owner, name, chunk);
    let data = github_api::graphql(&group.auth, env, &query, variables).map_err(|e| e.to_string())?;
    if data["repository"].is_null() {
        return Err(format!("GitHub has no repository {} that {} can read", group.repo, group.auth.account));
    }
    Ok(data["repository"].clone())
}

/// A group's answers from the replies to its queries.
fn read_group(group: &Group, replies: &[(&Chunk, &Result<Value, String>)], env: &HashMap<String, String>) -> Answers {
    let mut part = Answers::default();
    if owner_name(&group.repo).is_none() {
        for lookup in &group.lookups {
            part.found.insert(lookup.key.clone(), Err(format!("not a GitHub repository: {}", group.repo)));
        }
        return part;
    }
    // Branch → its open PR's node (or null), number → its node, merge number → its node.
    let mut by_branch: HashMap<String, Result<Value, String>> = HashMap::new();
    let mut by_number: HashMap<u64, Result<Value, String>> = HashMap::new();
    let mut by_merge: HashMap<u64, Result<Value, String>> = HashMap::new();
    for (chunk, reply) in replies {
        for (i, branch) in chunk.branches.iter().enumerate() {
            by_branch.insert(branch.clone(), reply.as_ref().map(|r| r[format!("h{i}")]["nodes"][0].clone()).map_err(Clone::clone));
        }
        for (i, number) in chunk.numbers.iter().enumerate() {
            by_number.insert(*number, reply.as_ref().map(|r| r[format!("n{i}")].clone()).map_err(Clone::clone));
        }
        for (i, number) in chunk.merges.iter().enumerate() {
            by_merge.insert(*number, reply.as_ref().map(|r| r[format!("m{i}")].clone()).map_err(Clone::clone));
        }
    }
    for lookup in &group.lookups {
        let found = match lookup.number {
            Some(number) => match by_number.get(&number) {
                Some(Ok(node)) if !node.is_null() => Ok(Found { pr: pull_request(node), ci: None }),
                Some(Ok(_)) => Err(format!("{}#{number}: no such pull request", group.repo)),
                Some(Err(err)) => Err(err.clone()),
                None => Err("not asked".into()),
            },
            None => {
                let mut found = Ok(Found::default());
                for candidate in &lookup.candidates {
                    match by_branch.get(candidate) {
                        Some(Ok(node)) if !node.is_null() => {
                            let pr = gh_shaped_pr(&group.auth, env, node);
                            found = Ok(Found { pr: pull_request(node), ci: Some(github::ci_from_json(&pr)) });
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            found = Err(err.clone());
                            break;
                        }
                        None => {}
                    }
                }
                found
            }
        };
        part.found.insert(lookup.key.clone(), found);
    }
    for number in &group.merges {
        let merged = match by_merge.get(number) {
            Some(Ok(node)) if !node.is_null() => Ok(github::merged_from_json(node)),
            Some(Ok(_)) => Err(format!("{}#{number}: no such pull request", group.repo)),
            Some(Err(err)) => Err(err.clone()),
            None => Err("not asked".into()),
        };
        part.merged.insert((group.repo.clone(), *number), merged);
    }
    part
}

// ----- one pull request --------------------------------------------------------------------------

/// Pull request `number`'s node with `fragment` (named `name`), or None when there is no such PR.
fn pr_node(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64, name: &str, fragments: &[&str]) -> anyhow::Result<Option<Value>> {
    let Some((owner, repo_name)) = owner_name(repo) else { anyhow::bail!("not a GitHub repository: {repo}") };
    let query = format!("query($owner: String!, $name: String!) {{ rateLimit {{ cost }} repository(owner: $owner, name: $name) {{ pullRequest(number: {number}) {{ ...{name} }} }} }} {}", fragments.join(" "));
    let data = github_api::graphql(auth, env, &query, json!({ "owner": owner, "name": repo_name }))?;
    if data["repository"].is_null() {
        anyhow::bail!("GitHub has no repository {repo} that {} can read", auth.account);
    }
    let node = data["repository"]["pullRequest"].clone();
    Ok((!node.is_null()).then_some(node))
}

pub fn pr_by_number(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64) -> anyhow::Result<Option<PullRequest>> {
    match pr_node(auth, env, repo, number, "PrLite", &[PR_LITE])? {
        Some(node) => Ok(pull_request(&node)),
        None => anyhow::bail!("{repo}#{number}: no such pull request"),
    }
}

pub fn pr_ci(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64) -> anyhow::Result<CiStatus> {
    match pr_node(auth, env, repo, number, "Pr", &[PR, PR_LITE, CHECKS, CONTEXT])? {
        Some(node) => Ok(github::ci_from_json(&gh_shaped_pr(auth, env, &node))),
        None => anyhow::bail!("{repo}#{number}: no such pull request"),
    }
}

pub fn pr_merged(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64) -> anyhow::Result<Option<Merged>> {
    match pr_node(auth, env, repo, number, "Merge", &[MERGE])? {
        Some(node) => Ok(github::merged_from_json(&node)),
        None => anyhow::bail!("{repo}#{number}: no such pull request"),
    }
}

pub fn head_oid(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64) -> anyhow::Result<String> {
    let node = pr_node(auth, env, repo, number, "Head", &["fragment Head on PullRequest { headRefOid }"])?;
    let oid = node.map(|n| text(&n["headRefOid"])).unwrap_or_default();
    if oid.is_empty() {
        anyhow::bail!("no head commit for {repo}#{number}");
    }
    Ok(oid)
}

/// The open pull request of the first of `candidates` that has one: one query for all of them.
pub fn pr_for_candidates(auth: &Auth, env: &HashMap<String, String>, repo: &str, candidates: &[String]) -> anyhow::Result<Option<PullRequest>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let lookup = Lookup { key: String::new(), repo: repo.to_string(), candidates: candidates.to_vec(), number: None };
    let Some((owner, name)) = owner_name(repo) else { anyhow::bail!("not a GitHub repository: {repo}") };
    let chunk = Chunk { branches: lookup.candidates.clone(), ..Chunk::default() };
    let (query, variables) = chunk_query(owner, name, &chunk);
    let data = github_api::graphql(auth, env, &query, variables)?;
    if data["repository"].is_null() {
        anyhow::bail!("GitHub has no repository {repo} that {} can read", auth.account);
    }
    for (i, _) in chunk.branches.iter().enumerate() {
        let node = &data["repository"][format!("h{i}")]["nodes"][0];
        if !node.is_null() {
            return Ok(pull_request(node));
        }
    }
    Ok(None)
}

/// Everything the PR panel shows (`github::pr_details`): one query, then the preview
/// deployment's REST reads, which come back as 304 when nothing changed.
pub fn pr_details(auth: &Auth, env: &HashMap<String, String>, repo: &str, number: u64, preview: &github::PreviewConfig) -> anyhow::Result<github::PrDetails> {
    let Some((owner, name)) = owner_name(repo) else { anyhow::bail!("not a GitHub repository: {repo}") };
    let query = format!(
        "query($owner: String!, $name: String!) {{ rateLimit {{ cost }} repository(owner: $owner, name: $name) {{ pullRequest(number: {number}) {{ \
         number title url state isDraft headRefName baseRefName author {{ login }} updatedAt mergeable mergeStateStatus reviewDecision \
         labels(first: 100) {{ nodes {{ name }} }} \
         reviewRequests(first: 100) {{ nodes {{ requestedReviewer {{ __typename ... on User {{ login }} ... on Bot {{ login }} ... on Mannequin {{ login }} ... on Team {{ name }} }} }} }} \
         latestReviews(first: 100) {{ nodes {{ author {{ login }} authorAssociation state submittedAt }} }} \
         comments(last: 100) {{ nodes {{ author {{ login }} body createdAt url isMinimized }} }} \
         commits(last: 1) {{ nodes {{ commit {{ ...Checks }} }} }} \
         reviewThreads(first: 100) {{ nodes {{ id isResolved isOutdated path line originalLine comments(first: 50) {{ nodes {{ author {{ login }} body createdAt url diffHunk }} }} }} }} \
         }} }} }} {CHECKS} {CONTEXT}"
    );
    let data = github_api::graphql(auth, env, &query, json!({ "owner": owner, "name": name }))?;
    let node = &data["repository"]["pullRequest"];
    if node.is_null() {
        anyhow::bail!("{repo}#{number}: no such pull request that {} can read", auth.account);
    }
    // gh's shape: lists, not connections.
    let nodes = |connection: &Value| Value::Array(connection["nodes"].as_array().cloned().unwrap_or_default());
    let mut pr = node.clone();
    pr["labels"] = nodes(&node["labels"]);
    pr["latestReviews"] = nodes(&node["latestReviews"]);
    pr["comments"] = nodes(&node["comments"]);
    pr["reviewRequests"] = Value::Array(node["reviewRequests"]["nodes"].as_array().into_iter().flatten().map(|r| r["requestedReviewer"].clone()).collect());
    pr["statusCheckRollup"] = rollup(auth, env, &node["commits"]["nodes"][0]["commit"]);
    let threads = node["reviewThreads"]["nodes"].clone();
    let mut details = github::details_from_json(number, &pr, &threads);

    let environment = preview.environment_for(number);
    let deployment = github_api::rest_get(auth, env, &format!("/repos/{repo}/deployments?environment={}&per_page=1", urlencoding::encode(&environment)))
        .ok()
        .and_then(|d| d.as_array().and_then(|a| a.first().cloned()));
    let status = deployment
        .as_ref()
        .and_then(|d| d["id"].as_u64())
        .and_then(|id| github_api::rest_get(auth, env, &format!("/repos/{repo}/deployments/{id}/statuses?per_page=1")).ok())
        .and_then(|s| s.as_array().and_then(|a| a.first().cloned()));
    let label = || match github_api::rest_get(auth, env, &format!("/repos/{repo}/labels/{}", urlencoding::encode(&preview.label))) {
        Ok(_) => true,
        Err(ApiError::NotFound) => false,
        Err(err) => {
            log::debug!("could not see whether {repo} has the label {}: {err}", preview.label);
            false
        }
    };
    github::finish_preview(&mut details, preview, deployment.as_ref(), status.as_ref(), label);
    Ok(details)
}

// ----- across repositories -----------------------------------------------------------------------

/// The open pull requests `auth`'s account wrote, newest first, each with its checks and review:
/// one search.
pub fn my_open_prs(auth: &Auth, env: &HashMap<String, String>, limit: u32) -> anyhow::Result<Vec<github::MyPr>> {
    let query = format!(
        "query {{ rateLimit {{ cost }} search(query: \"is:pr is:open author:@me archived:false sort:updated-desc\", type: ISSUE, first: {limit}) {{ nodes {{ ... on PullRequest {{ \
         number title url updatedAt isDraft repository {{ nameWithOwner }} reviewDecision headRefOid state mergeable baseRefName baseRefOid \
         commits(last: 1) {{ nodes {{ commit {{ ...Checks }} }} }} }} }} }} }} {CHECKS} {CONTEXT}"
    );
    let data = github_api::graphql(auth, env, &query, Value::Null)?;
    Ok(data["search"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|node| {
            let repo = text(&node["repository"]["nameWithOwner"]);
            if repo.is_empty() {
                return None;
            }
            let pr = gh_shaped_pr(auth, env, node);
            Some(github::MyPr {
                repo,
                number: node["number"].as_u64()?,
                title: text(&node["title"]),
                url: text(&node["url"]),
                updated_at: text(&node["updatedAt"]),
                draft: node["isDraft"].as_bool().unwrap_or(false),
                ci: Some(github::ci_from_json(&pr)),
                review: text(&node["reviewDecision"]),
            })
        })
        .collect())
}

/// A query `github` keeps as text (`REVIEW_REQUESTS`), answered as `gh api graphql` prints it
/// (`{"data": …}`).
pub fn as_gh_graphql(auth: &Auth, env: &HashMap<String, String>, query: &str) -> anyhow::Result<Value> {
    let data = github_api::graphql(auth, env, query, Value::Null)?;
    Ok(json!({ "data": data }))
}

// ----- the forge's own rules (M23, D29) ---------------------------------------------------------

/// What GitHub enforces for a branch, from its rules (`/repos/{repo}/rules/branches/{branch}`,
/// each rule with its ruleset), the rulesets' `current_user_can_bypass`, and the classic branch
/// protection when it can be read (it needs admin rights). Glasscode only reads them.
pub fn forge_facts_of(rules: &Value, bypass: &HashMap<u64, String>, classic: Option<&Value>, read_ms: u64) -> crate::gate::ForgeFacts {
    let rules: &[Value] = rules.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let review_rules: Vec<&Value> = rules.iter().filter(|r| r["type"] == "pull_request" && r["parameters"]["required_approving_review_count"].as_u64().unwrap_or(0) >= 1).collect();
    let force_rules: Vec<&Value> = rules.iter().filter(|r| r["type"] == "non_fast_forward").collect();
    let never_bypassed = |rule: &&Value| rule["ruleset_id"].as_u64().and_then(|id| bypass.get(&id)).is_some_and(|b| b == "never");
    let (mut review, mut no_force, mut no_bypass) = (!review_rules.is_empty(), !force_rules.is_empty(), !review_rules.is_empty() && !force_rules.is_empty() && review_rules.iter().all(never_bypassed) && force_rules.iter().all(never_bypassed));
    if let Some(classic) = classic.filter(|c| c.is_object() && c.get("url").is_some()) {
        let classic_review = classic["required_pull_request_reviews"]["required_approving_review_count"].as_u64().unwrap_or(0) >= 1;
        let classic_force = classic["allow_force_pushes"]["enabled"] == Value::Bool(false) || classic.get("allow_force_pushes").is_none();
        let enforced = classic["enforce_admins"]["enabled"] == Value::Bool(true);
        if !(review && no_force && no_bypass) && classic_review && classic_force && enforced {
            (review, no_force, no_bypass) = (true, true, true);
        } else {
            review |= classic_review;
            no_force |= classic_force;
        }
    }
    crate::gate::ForgeFacts { review_required: review, no_force_push: no_force, no_bypass, read_ms }
}

/// Read what GitHub enforces for `branch` of `repo` as the repository's account. It talks to the
/// network: never on the main thread.
pub fn forge_facts(repo: &str, branch: &str, env: &HashMap<String, String>) -> Option<crate::gate::ForgeFacts> {
    let auth = crate::github_api::auth_for(repo, env)?;
    let rules = crate::github_api::rest_get(&auth, env, &format!("/repos/{repo}/rules/branches/{branch}")).ok()?;
    let mut bypass = HashMap::new();
    for id in rules.as_array().into_iter().flatten().filter_map(|r| r["ruleset_id"].as_u64()) {
        if bypass.contains_key(&id) {
            continue;
        }
        let ruleset = crate::github_api::rest_get(&auth, env, &format!("/repos/{repo}/rulesets/{id}")).ok();
        bypass.insert(id, ruleset.and_then(|r| r["current_user_can_bypass"].as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into()));
    }
    let classic = crate::github_api::rest_get(&auth, env, &format!("/repos/{repo}/branches/{branch}/protection")).ok();
    Some(forge_facts_of(&rules, &bypass, classic.as_ref(), crate::clock::now_ms()))
}

#[cfg(test)]
mod tests {
    /// D29: Autopilot's push needs the forge to require a review, block force-push, and let the
    /// person bypass neither.
    #[test]
    fn autopilot_push_needs_forge_rules() {
        use std::collections::HashMap;
        let rules = serde_json::json!([
            { "type": "pull_request", "ruleset_id": 7, "parameters": { "required_approving_review_count": 1 } },
            { "type": "non_fast_forward", "ruleset_id": 7 },
        ]);
        let never: HashMap<u64, String> = [(7, "never".to_string())].into();
        assert!(super::forge_facts_of(&rules, &never, None, 1).holds());
        let always: HashMap<u64, String> = [(7, "always".to_string())].into();
        assert!(!super::forge_facts_of(&rules, &always, None, 1).holds(), "the person can bypass");
        let no_force_rule = serde_json::json!([{ "type": "pull_request", "ruleset_id": 7, "parameters": { "required_approving_review_count": 1 } }]);
        assert!(!super::forge_facts_of(&no_force_rule, &never, None, 1).holds());
        let zero = serde_json::json!([{ "type": "pull_request", "ruleset_id": 7, "parameters": { "required_approving_review_count": 0 } }, { "type": "non_fast_forward", "ruleset_id": 7 }]);
        assert!(!super::forge_facts_of(&zero, &never, None, 1).holds(), "a pull request with no approval needed");
        // Classic protection, readable by an admin, with admins held too.
        let classic = serde_json::json!({ "url": "x", "required_pull_request_reviews": { "required_approving_review_count": 2 }, "allow_force_pushes": { "enabled": false }, "enforce_admins": { "enabled": true } });
        assert!(super::forge_facts_of(&serde_json::json!([]), &HashMap::new(), Some(&classic), 1).holds());
        let loose = serde_json::json!({ "url": "x", "required_pull_request_reviews": { "required_approving_review_count": 2 }, "enforce_admins": { "enabled": false } });
        assert!(!super::forge_facts_of(&serde_json::json!([]), &HashMap::new(), Some(&loose), 1).holds());
        // The gate: Autopilot's forge writes only where they hold.
        let line = crate::gate::forge_line(Some(&super::forge_facts_of(&rules, &never, None, 1)));
        assert!(line.starts_with("Forge enforces"));
        assert!(crate::gate::forge_line(None).starts_with("Guidance only"));
    }

    use super::*;

    #[test]
    fn questions_split_into_queries_of_a_bounded_size() {
        let branches: Vec<String> = (0..7).map(|i| format!("b{i}")).collect();
        let parts = chunks(branches, vec![3, 3, 1], vec![9], 4);
        assert_eq!(parts.iter().map(Chunk::len).collect::<Vec<_>>(), [4, 4, 2]);
        assert_eq!(parts[1].branches, ["b4", "b5", "b6"]);
        assert_eq!(parts[1].numbers, [1]);
        assert_eq!(parts[2].numbers, [3]);
        assert_eq!(parts[2].merges, [9]);
        assert!(chunks(Vec::new(), Vec::new(), Vec::new(), 4).is_empty());
    }

    #[test]
    fn a_query_has_its_aliases_variables_and_only_the_fragments_it_uses() {
        let chunk = Chunk { branches: vec!["feat/x\" injected".into()], numbers: vec![958], merges: vec![] };
        let (query, variables) = chunk_query("acme", "app", &chunk);
        assert!(query.contains("h0: pullRequests(headRefName: $h0, states: OPEN, first: 1"));
        assert!(query.contains("n0: pullRequest(number: 958) { ...PrLite }"));
        assert!(query.contains("fragment Pr on") && query.contains("fragment Checks on") && query.contains("fragment PrLite on"));
        assert!(!query.contains("fragment Merge on"));
        // A branch name is a variable, never part of the query.
        assert!(!query.contains("injected"));
        assert_eq!(variables["h0"], "feat/x\" injected");
        assert_eq!(variables["owner"], "acme");
        // Merges only: neither the Pr nor the PrLite fragment.
        let (query, _) = chunk_query("acme", "app", &Chunk { merges: vec![5], ..Chunk::default() });
        assert!(query.contains("m0: pullRequest(number: 5) { ...Merge }") && query.contains("fragment Merge on"));
        assert!(!query.contains("fragment Pr on") && !query.contains("fragment PrLite on") && !query.contains("$h"));
    }

    #[test]
    fn checks_come_out_as_gh_prints_them() {
        let run = json!({ "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://x/1",
            "startedAt": "2026-11-17T10:00:00Z", "completedAt": "2026-11-17T10:05:00Z", "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } } });
        assert_eq!(
            context_json(&run),
            json!({ "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://x/1",
                "startedAt": "2026-11-17T10:00:00Z", "completedAt": "2026-11-17T10:05:00Z", "workflowName": "CI" })
        );
        let status = json!({ "__typename": "StatusContext", "context": "vercel", "state": "PENDING", "targetUrl": "https://v", "createdAt": "2026-11-17T09:00:00Z" });
        assert_eq!(context_json(&status), json!({ "__typename": "StatusContext", "context": "vercel", "state": "PENDING", "targetUrl": "https://v", "startedAt": "2026-11-17T09:00:00Z" }));
        // A check run outside Actions has no workflow.
        let other = json!({ "__typename": "CheckRun", "name": "lint", "checkSuite": { "workflowRun": null } });
        assert_eq!(context_json(&other)["workflowName"], "");
    }

    #[test]
    fn a_pr_node_reads_as_the_pr_and_its_ci() {
        let node = json!({
            "number": 12, "title": "Fix", "url": "https://github.com/acme/app/pull/12", "baseRefName": "main", "headRefName": "fix", "isDraft": false, "state": "OPEN",
            "headRefOid": "abc", "baseRefOid": "def", "mergeable": "CONFLICTING",
            "commits": { "nodes": [{ "commit": { "id": "C1", "oid": "abc", "statusCheckRollup": { "contexts": {
                "pageInfo": { "hasNextPage": false, "endCursor": null },
                "nodes": [{ "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "u", "startedAt": "s", "completedAt": "c",
                    "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } } }] } } } }] }
        });
        let auth = Auth::test("node-test");
        let pr = gh_shaped_pr(&auth, &HashMap::new(), &node);
        let ci = github::ci_from_json(&pr);
        assert_eq!((ci.head.as_str(), ci.state.as_str(), ci.base.as_str(), ci.base_head.as_str()), ("abc", "OPEN", "main", "def"));
        assert!(ci.conflicting());
        assert_eq!(ci.failed().len(), 1);
        assert_eq!(ci.checks[0].workflow, "CI");
        let pr = pull_request(&node).unwrap();
        assert_eq!((pr.number, pr.head_ref_name.as_str(), pr.state.as_str()), (12, "fix", "OPEN"));
        // A commit with no checks at all.
        let bare = json!({ "number": 1, "title": "t", "url": "u", "state": "OPEN", "commits": { "nodes": [{ "commit": { "id": "C2", "oid": "x", "statusCheckRollup": null } }] } });
        assert!(github::ci_from_json(&gh_shaped_pr(&auth, &HashMap::new(), &bare)).checks.is_empty());
    }

    /// Against GitHub itself: the same questions through `gh` and asked of GitHub directly give
    /// the same answers. Not run by default (it needs `gh` signed in and the network):
    ///
    /// GLASSCODE_PARITY_PRS="owner/name#12 owner/name#34" (pull requests that do not change
    /// while it runs), GLASSCODE_PARITY_BRANCHES="owner/name:branch …" (one with a PR, one
    /// without), GLASSCODE_PARITY_ROUTE="owner/name=login" (a repository read as another
    /// account), then `cargo test -p glasscode_core parity -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn parity_with_gh() {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let env: HashMap<String, String> = std::env::vars().filter(|(k, _)| k == "PATH" || k == "HOME" || k == "GH_TOKEN").collect();
        let by_repo: HashMap<String, String> = var("GLASSCODE_PARITY_ROUTE").split_whitespace().filter_map(|r| r.split_once('=')).map(|(r, l)| (r.to_lowercase(), l.to_string())).collect();
        crate::github_accounts::configure(&Default::default(), by_repo, &env);
        let prs: Vec<(String, u64)> = var("GLASSCODE_PARITY_PRS").split_whitespace().filter_map(|p| p.split_once('#')).map(|(r, n)| (r.to_string(), n.parse().unwrap())).collect();
        let branches: Vec<(String, String)> = var("GLASSCODE_PARITY_BRANCHES").split_whitespace().filter_map(|b| b.split_once(':')).map(|(r, b)| (r.to_string(), b.to_string())).collect();
        assert!(!prs.is_empty(), "set GLASSCODE_PARITY_PRS");
        let preview = github::PreviewConfig::load(std::path::Path::new("/nonexistent"));
        let ask = |off: bool| {
            if off {
                unsafe { std::env::set_var("GLASSCODE_GITHUB_API", "off") };
            } else {
                unsafe { std::env::remove_var("GLASSCODE_GITHUB_API") };
            }
            let mut out = serde_json::Map::new();
            for (repo, n) in &prs {
                out.insert(format!("{repo}#{n} pr"), json!(format!("{:?}", github::pr_by_number(repo, *n, &env).map_err(|e| e.to_string()))));
                out.insert(format!("{repo}#{n} ci"), json!(github::pr_ci(repo, *n, &env).map(|c| serde_json::to_value(c).unwrap()).map_err(|e| e.to_string())));
                out.insert(format!("{repo}#{n} merged"), json!(format!("{:?}", github::pr_merged(repo, *n, &env).map_err(|e| e.to_string()))));
                out.insert(format!("{repo}#{n} head"), json!(format!("{:?}", github::head_oid(repo, *n, &env).map_err(|e| e.to_string()))));
                crate::github_api::forget_pr(repo, *n);
                out.insert(format!("{repo}#{n} details"), github::pr_details(repo, *n, &preview, &env).map(|d| serde_json::to_value(d).unwrap()).unwrap_or_else(|e| json!(e.to_string())));
            }
            for (repo, branch) in &branches {
                let found = if off { github::pr_for_candidates_gh(repo, &[branch.clone()], &env) } else { pr_for_candidates(&crate::github_api::auth_for(repo, &env).unwrap(), &env, repo, &[branch.clone()]) };
                out.insert(format!("{repo}:{branch}"), json!(format!("{:?}", found.map_err(|e| e.to_string()))));
            }
            let mine = github::my_open_prs(&env).unwrap();
            for pr in mine.iter().take(github::MY_PR_DETAILS) {
                out.insert(format!("mine {}#{}", pr.repo, pr.number), json!({ "title": pr.title, "draft": pr.draft, "review": pr.review, "ci": serde_json::to_value(&pr.ci).unwrap() }));
            }
            out.insert("mine count".into(), json!(mine.len()));
            let reviews = github::review_requested(&env).unwrap();
            out.insert("review requests".into(), serde_json::to_value(reviews).unwrap());
            out
        };
        let by_gh = ask(true);
        let native = ask(false);
        let mut differ = Vec::new();
        for (key, value) in &by_gh {
            if native.get(key) != Some(value) {
                differ.push(format!("{key}\n  gh:     {}\n  native: {}", value, native.get(key).map(|v| v.to_string()).unwrap_or_default()));
            }
        }
        println!("compared {} answers", by_gh.len());
        assert!(differ.is_empty(), "differences:\n{}", differ.join("\n"));
    }

    /// Against GitHub itself: one refresh of every session of a Glasscode `state.json`, as the
    /// app makes it (git for each checkout, then `look_up`), twice: the second with git's
    /// answers kept, as every refresh after the first. Prints the time, queries and points.
    /// GLASSCODE_ROUND_STATE=~/.glasscode/state.json cargo test -p glasscode_core round_of -- --ignored --nocapture
    #[test]
    #[ignore]
    fn round_of_the_persons_sessions() {
        let state: Value = serde_json::from_slice(&std::fs::read(std::env::var("GLASSCODE_ROUND_STATE").unwrap()).unwrap()).unwrap();
        let env: HashMap<String, String> = std::env::vars().filter(|(k, _)| k == "PATH" || k == "HOME").collect();
        let mut by_repo = HashMap::new();
        for project in state["projects"].as_array().into_iter().flatten() {
            if let (Some(login), Some(root)) = (project["github_account"].as_str(), project["root"].as_str()) {
                if let Some(repo) = github::repo_slug(std::path::Path::new(root)) {
                    by_repo.insert(repo.to_lowercase(), login.to_string());
                }
            }
        }
        let settings = crate::github_accounts::GithubSettings { default: state["github"]["default"].as_str().map(str::to_string), ..Default::default() };
        crate::github_accounts::configure(&settings, by_repo, &env);
        let sessions: Vec<(std::path::PathBuf, Option<u64>)> =
            state["sessions"].as_array().into_iter().flatten().map(|s| (std::path::PathBuf::from(text(&s["worktree"])), s["review"]["number"].as_u64())).collect();
        for pass in ["first (git asked)", "next (git kept)"] {
            let started = std::time::Instant::now();
            let before = github_api::stats();
            let mut lookups = Vec::new();
            for (worktree, review) in &sessions {
                if !worktree.is_dir() {
                    continue;
                }
                let info = crate::git_info::info(worktree);
                if let Some(repo) = info.repo {
                    let key = format!("{}|{review:?}", worktree.display());
                    if !lookups.iter().any(|l: &Lookup| l.key == key) {
                        lookups.push(Lookup { key, repo, candidates: if review.is_none() { info.candidates } else { Vec::new() }, number: *review });
                    }
                }
            }
            let git_ms = started.elapsed().as_millis();
            let answers = look_up(&lookups, &[], &env);
            let after = github_api::stats();
            let with_pr = answers.found.values().filter(|f| f.as_ref().is_ok_and(|f| f.pr.is_some())).count();
            let failed: Vec<&String> = answers.found.iter().filter(|(_, f)| f.is_err()).map(|(k, _)| k).collect();
            println!(
                "{pass}: {} sessions, {} checkouts ({with_pr} with a PR) in {} ms (git {git_ms} ms); {} queries, {} requests, {} points; failed: {failed:?}",
                sessions.len(),
                lookups.len(),
                started.elapsed().as_millis(),
                answers.queries,
                after.requests - before.requests,
                after.points - before.points
            );
        }
    }

    #[test]
    fn an_owner_and_name_are_needed() {
        assert_eq!(owner_name("acme/app"), Some(("acme", "app")));
        assert_eq!(owner_name("acme"), None);
        assert_eq!(owner_name("acme/app/x"), None);
        assert_eq!(owner_name("/app"), None);
    }
}
