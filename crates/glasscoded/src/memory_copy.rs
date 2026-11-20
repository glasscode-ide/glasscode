//! The server's read-only copy of the Mac's memory (M20.3): the Mac pushes a filtered history
//! (`glasscode_core::memory_copy`) to `memory-server.git` here, and this checks its commit out
//! into `memory-server/`, read-only. Server sessions read it; they propose changes through the
//! `glasscode` command, which the Mac decides. Only a Glasscode machine's own connection calls
//! these (`Caller::Machine`).

use anyhow::{anyhow, bail};
use glasscode_core::{memory_copy, paths};
use serde_json::{Value, json};

use crate::daemon::log;

fn git(args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git").arg("-C").arg(paths::memory_server_repo()).args(args).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The copy's commit checked out now, if any.
pub fn current() -> Option<String> {
    git(&["rev-parse", "--verify", "--quiet", memory_copy::SERVER_REF]).ok().filter(|c| !c.is_empty())
}

pub fn dispatch(method: &str, params: &Value) -> Option<anyhow::Result<Value>> {
    let answer = match method {
        // Where the Mac pushes (a bare repository, made at the first ask).
        "memory.copy.target" => (|| {
            let repo = paths::memory_server_repo();
            if !repo.join("HEAD").is_file() {
                std::fs::create_dir_all(&repo)?;
                let out = std::process::Command::new("git").arg("init").arg("-q").arg("--bare").arg(&repo).output()?;
                if !out.status.success() {
                    bail!("could not make {}: {}", repo.display(), String::from_utf8_lossy(&out.stderr).trim());
                }
            }
            Ok(serde_json::to_value(repo)?)
        })(),
        // The Mac pushed a new commit of the copy: check it out, read-only.
        "memory.copy.update" => (|| {
            let commit = params.get("commit").and_then(Value::as_str).ok_or_else(|| anyhow!("no commit"))?;
            if !commit.chars().all(|c| c.is_ascii_hexdigit()) || commit.len() < 40 {
                bail!("not a commit id");
            }
            if current().as_deref() != Some(commit) {
                bail!("{commit} is not the copy the Mac pushed");
            }
            memory_copy::check_out_read_only(&paths::memory_server_repo(), commit, &paths::memory_server_dir())?;
            log(format!("the Mac's memory (without its local-only projects) is here at {}", &commit[..12]));
            Ok(json!({ "commit": commit }))
        })(),
        "memory.copy.get" => Ok(json!({ "commit": current(), "path": paths::memory_server_dir() })),
        _ => return None,
    };
    Some(answer)
}
