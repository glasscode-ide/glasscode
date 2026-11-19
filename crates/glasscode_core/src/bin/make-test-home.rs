//! `make-test-home <folder>`: a scratch Glasscode home for trying Manager v2 by hand (M02 step 1).
//!
//! It fills `<folder>` (new or empty, never the person's own `~/.glasscode`, `~/.claude` or
//! `~/.codex`) with `testkit::home`'s default home and prints the variables that point a shell, and
//! Glasscode started from it, at that home and at the stand-in agents:
//!
//! ```sh
//! cargo run -p glasscode_core --features testkit --bin make-test-home -- /tmp/gc-home > /tmp/gc-home.env
//! . /tmp/gc-home.env && cargo run      # Glasscode on the scratch home, with stand-in claude and codex
//! ```
//!
//! `--bare` makes one project and no conversations.

use std::path::PathBuf;

use glasscode_core::testkit::home::{self, HomeSpec};

fn main() {
    let mut folder = None;
    let mut spec = HomeSpec::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--bare" => spec = HomeSpec::bare(),
            "-h" | "--help" => {
                println!("usage: make-test-home [--bare] <folder>\n\nFills <folder> with a scratch Glasscode home and prints the variables to use it.");
                return;
            }
            _ if arg.starts_with('-') => fail(&format!("unknown option {arg}")),
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let Some(folder) = folder else { fail("usage: make-test-home [--bare] <folder>") };
    let folder = if folder.is_absolute() { folder } else { std::env::current_dir().expect("a working folder").join(folder) };
    if home::is_real(&folder) {
        fail(&format!("{} is a folder of your own; a test home goes in a scratch folder", folder.display()));
    }
    if folder.exists() && std::fs::read_dir(&folder).map(|mut entries| entries.next().is_some()).unwrap_or(true) {
        fail(&format!("{} is not empty", folder.display()));
    }
    let built = home::build_at(&folder, &spec);
    let mut env: Vec<(String, String)> = home::agent_env(&built).into_iter().collect();
    env.sort();
    for (name, value) in env {
        println!("export {name}={}", quoted(&value));
    }
    eprintln!("A scratch Glasscode home in {}: {} projects, accounts {} and {}.", built.top.display(), built.projects.len(), home::CLAUDE_ACCOUNT, home::CODEX_ACCOUNT);
}

/// A value for a POSIX shell, in single quotes.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn fail(message: &str) -> ! {
    eprintln!("make-test-home: {message}");
    std::process::exit(2);
}
