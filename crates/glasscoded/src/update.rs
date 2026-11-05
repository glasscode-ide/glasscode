//! glasscoded keeps itself current: every few hours (when this server is signed in with
//! A releases endpoint) it asks for a newer release, checks the release key's signature and the
//! file's SHA-256, swaps ~/.glasscode/bin/glasscoded and starts again in place. Sessions
//! keep running in tmux; phones and Macs reconnect by themselves.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, bail};
use glasscode_core::paths;
use glasscode_core::update::{self, BUILD, VERSION};

const PRODUCT: &str = "glasscoded";
const CHANNEL: &str = "stable";
const EVERY: Duration = Duration::from_secs(3600);

fn installed() -> PathBuf {
    paths::root().join("bin").join("glasscoded")
}

/// Whether this process is the installed glasscoded (a copy run from elsewhere, like a
/// development build, never replaces anything).
fn running_installed() -> bool {
    let here = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    here.is_some() && here == installed().canonicalize().ok()
}

/// Download and put in place a newer release; Some(its build) when one was installed.
///
/// TODO(glasscode): auto-update fetched releases from the (removed) hosted release
/// server. The standalone fork has no release endpoint yet, so this is a no-op; `install`
/// glasscoded again by hand to update it. Wire `update::newer`/`download` to a self-hosted
/// releases endpoint to restore this.
pub fn install_newer() -> anyhow::Result<Option<u64>> {
    let _ = (PRODUCT, CHANNEL, BUILD, installed as fn() -> PathBuf);
    let _ = update::newer(PRODUCT, CHANNEL, BUILD)?;
    Ok(None)
}

/// Start the installed binary in this process's place (same PID, so systemd is happy).
fn restart_in_place() -> anyhow::Error {
    let error = Command::new(installed()).arg("serve").exec();
    anyhow::anyhow!("could not start the new glasscoded: {error}")
}

/// The daemon's update loop.
pub fn run() {
    let first = std::env::var("GLASSCODE_UPDATE_FIRST_CHECK").ok().and_then(|s| s.parse().ok()).unwrap_or(180);
    std::thread::sleep(Duration::from_secs(first));
    loop {
        let wait = if !running_installed() || crate::relay_link::load().is_none() {
            EVERY
        } else {
            match install_newer() {
                Ok(Some(build)) => {
                    crate::daemon::log(format!("update: installed glasscoded build {build}; restarting"));
                    let error = restart_in_place();
                    crate::daemon::log(format!("update: {error:#}"));
                    Duration::from_secs(3600)
                }
                Ok(None) => EVERY,
                Err(error) => {
                    crate::daemon::log(format!("update: {error:#}"));
                    Duration::from_secs(3600)
                }
            }
        };
        std::thread::sleep(wait);
    }
}

/// Make the running daemon start the installed binary, a moment from now. One from before
/// `restart` existed is restarted by systemd (a little later, so an SSH session through the
/// relay can finish), or stopped and started again here, detached: a server reached through
/// the relay depends on the daemon being up, and nothing would start it on demand.
pub fn restart_daemon() {
    use std::io::{BufRead as _, BufReader, Write as _};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(paths::daemon_socket()) else {
        // Not running: under systemd the service starts it (install does), else start it here.
        if !crate::install::systemd_user() {
            start_detached();
        }
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.write_all(b"{\"id\":1,\"method\":\"restart\"}\n");
    let mut line = String::new();
    let _ = BufReader::new(&stream).read_line(&mut line);
    if line.contains("\"result\"") {
        // It restarts a moment later, once whoever asked (maybe over the relay) is done.
        return;
    }
    if crate::install::systemd_user() && crate::install::delayed_service_restart() {
        return;
    }
    let _ = stream.write_all(b"{\"id\":2,\"method\":\"shutdown\"}\n");
    std::thread::sleep(Duration::from_millis(600));
    start_detached();
}

fn start_detached() {
    // Under systemd the service is started by systemctl; this is for servers without one.
    // SAFETY: setsid has no preconditions; it runs in the child before exec.
    let _ = unsafe {
        Command::new(installed())
            .arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()
    };
}

/// `glasscoded update`: check now; the running daemon restarts with the new build.
pub fn command() -> anyhow::Result<()> {
    println!("glasscoded {}", update::describe(VERSION, BUILD));
    match install_newer()? {
        None => println!("Up to date."),
        Some(build) => {
            println!("Installed build {build}.");
            restart_daemon();
            println!("The daemon restarts with it in a moment; sessions keep running.");
        }
    }
    Ok(())
}
