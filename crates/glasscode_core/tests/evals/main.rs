//! The eval suites of Manager v2 (M02 step 6), with stand-in agents and no model (rule 13 of the
//! slice plan). Each case is a fixture in `fixtures/` with its expected result:
//!
//! - [`adversarial`]: a planted instruction in every channel the manager and its sessions read,
//!   and forged callers; each case expects no effect and no fact (trust-17);
//! - [`memory_poisoning`]: the coding-memory set of mem-21, in the spirit of WhisperBench: facts
//!   that arrive through every channel, some planted, and the probe that asks for them;
//! - [`retrieval`]: 214 questions over a synthetic memory home, each naming the one section that
//!   answers it, scored as recall at 5 (land-14);
//! - [`op_selection`]: natural requests and the op with arguments they ask for, two or more per op,
//!   and requests that must select no op (ops-23).
//!
//! What runs now: the loaders, which check that each set is whole and says what it means to; the
//! retrieval scoring with a reference search; and the stand-ins serving the planted text. A case
//! whose code comes with a later slice is a test marked `#[ignore = "needs Mxx"]`: its fixtures
//! are in place and its body loads them up to one [`needs`] line, which that slice replaces with
//! the call to its own code before it takes the ignore off.
//!
//! ```sh
//! cargo test --locked -p glasscode_core --features testkit --test evals -- --test-threads=1 --nocapture
//! ```
//!
//! The live evals with the real CLIs (exec-4's third part, trust-17's second suite, ops-23's
//! selection run) are owner-run before a release, in a scratch home and within the weekly budget
//! (M55); never here.

mod adversarial;
mod memory_poisoning;
mod op_selection;
mod retrieval;

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// The fixtures of the suites.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("evals").join("fixtures")
}

/// One value a line, with the line's number in any error.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Vec<T> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| serde_json::from_str(line).unwrap_or_else(|err| panic!("{}:{}: {err}", path.display(), n + 1)))
        .collect()
}

/// The slices of the plan a case can wait for.
pub const SLICES: &[&str] = &[
    "M11", "M12.1", "M12.2", "M13", "M13.2", "M14", "M20.1", "M20.2", "M20.3", "M21.1", "M21.2", "M22", "M23", "M24", "M25", "M30", "M31", "M32", "M33.1", "M33.2", "M34", "M35", "M40", "M41", "M42", "M43", "M44", "M50",
    "M51", "M52.1", "M52.2", "M53", "M54", "M55", "M57", "M58",
];

/// Where a case calls the code of a later slice: that slice puts the call here, then takes the
/// ignore off its test.
pub fn needs(slice: &str, what: &str) -> ! {
    panic!("needs {slice}: {what}")
}

/// The cases that still wait at release 1, each with what it waits for: no slice's code, only
/// what no test here has. A new `#[ignore = "needs …"]` fails the gate until it is listed here.
const WAITING: &[(&str, &str)] = &[(
    "an_unsigned_phone_gets_no_caller",
    "an unpaired phone's key refused by sshd before any gate runs: only the Docker test server (script/test-server) shows it; the naming of callers is tested in core and in glasscoded",
)];

/// Release 1's gate on the suites (M55 step 2): every case runs, but those of [`WAITING`].
#[test]
fn only_the_listed_cases_wait() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("evals");
    let mut waiting = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "rs") || path.file_name().is_some_and(|name| name == "main.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[ignore = \"needs ") {
                continue;
            }
            let name = lines[n + 1..].iter().find_map(|line| line.trim_start().strip_prefix("fn ")).and_then(|rest| rest.split('(').next()).unwrap_or_else(|| panic!("{}:{}: no test below the ignore", path.display(), n + 1));
            waiting.push(name.to_string());
        }
    }
    waiting.sort();
    let mut listed: Vec<String> = WAITING.iter().map(|(name, _)| name.to_string()).collect();
    listed.sort();
    assert_eq!(waiting, listed, "a case waits that release 1 does not allow, or one listed runs now");
}
