//! Load and save `state.json` atomically.

use std::io::Write;

use crate::model::State;
use crate::paths;

pub fn load() -> State {
    let path = paths::state_file();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            log::error!("state.json is unreadable ({err}); keeping a backup and starting fresh");
            let _ = std::fs::copy(&path, path.with_extension("json.broken"));
            State::default()
        }),
        Err(_) => State::default(),
    }
}

pub fn save(state: &State) -> anyhow::Result<()> {
    paths::ensure_dirs()?;
    let path = paths::state_file();
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state)?;
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
