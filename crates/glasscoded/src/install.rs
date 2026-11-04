//! `glasscoded install`: put the binary in `~/.glasscode/bin` and run it as a systemd user
//! service, so the daemon starts at boot and restarts after a crash. Sessions themselves
//! live in tmux and do not depend on the service staying up.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, bail};

const UNIT: &str = "\
[Unit]
Description=Glasscode server: keeps Claude and Codex sessions running
After=network-online.target

[Service]
Type=simple
ExecStart=%h/.glasscode/bin/glasscoded serve
Restart=always
RestartSec=3
# Sessions run in tmux, which must outlive a daemon restart or update.
KillMode=process

[Install]
WantedBy=default.target
";

pub fn bin_path() -> PathBuf {
    glasscode_core::paths::root().join("bin").join("glasscoded")
}

fn unit_path() -> PathBuf {
    glasscode_core::paths::home().join(".config/systemd/user/glasscoded.service")
}

fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl").arg("--user").args(args).status().context("systemctl not found")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

/// A user service manager exists when its socket does (systemd-logind makes it for logged-in
/// or lingering users); containers often have `systemctl` but no systemd running.
pub fn systemd_user() -> bool {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    cfg!(target_os = "linux") && std::path::Path::new(&format!("/run/user/{uid}/systemd/private")).exists()
}

/// systemd restarts the service a few seconds from now (after this SSH session ends).
pub fn delayed_service_restart() -> bool {
    Command::new("systemd-run").args(["--user", "--on-active=3", "systemctl", "--user", "restart", "glasscoded"]).status().is_ok_and(|s| s.success())
        || Command::new("systemctl").args(["--user", "restart", "--no-block", "glasscoded"]).status().is_ok_and(|s| s.success())
}

pub fn install() -> anyhow::Result<()> {
    glasscode_core::paths::ensure_dirs()?;
    let target = bin_path();
    std::fs::create_dir_all(target.parent().unwrap())?;
    let current = std::env::current_exe()?;
    if current.canonicalize().ok() != target.canonicalize().ok() {
        // Copy beside it and rename, so a running daemon's binary is replaced atomically.
        let tmp = target.with_extension("new");
        std::fs::copy(&current, &tmp)?;
        std::fs::rename(&tmp, &target)?;
        println!("✓ installed {}", target.display());
    }
    let systemd = systemd_user();
    if !systemd {
        // No user service manager (a container, or macOS): the daemon starts on demand when
        // the app connects. Stop a running old one so the new binary takes over.
        crate::update::restart_daemon();
        println!("! no systemd user session here: glasscoded starts on demand and does not come back after a reboot by itself");
        return Ok(());
    }
    let unit = unit_path();
    std::fs::create_dir_all(unit.parent().unwrap())?;
    let unit_changed = std::fs::read_to_string(&unit).ok().as_deref() != Some(UNIT);
    if unit_changed {
        std::fs::write(&unit, UNIT)?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", "glasscoded"])?;
    // The new binary takes over a moment after this returns: an install that came through the
    // relay runs inside an SSH session the daemon carries, and must finish first. Sessions
    // keep running in tmux.
    let active = Command::new("systemctl").args(["--user", "is-active", "--quiet", "glasscoded"]).status().is_ok_and(|s| s.success());
    if !active {
        systemctl(&["start", "glasscoded"])?;
    } else if unit_changed {
        // A new unit needs systemd's own restart; a timer lets this session end first.
        if !delayed_service_restart() {
            crate::update::restart_daemon();
        }
    } else {
        crate::update::restart_daemon();
    }
    println!("✓ systemd user service glasscoded enabled and running");

    let user = std::env::var("USER").unwrap_or_default();
    let linger = Command::new("loginctl").args(["show-user", &user, "-p", "Linger", "--value"]).output().ok();
    let lingering = linger.is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes");
    if !lingering {
        // Allowed without sudo on most systems; otherwise print the command.
        let ok = Command::new("loginctl").args(["enable-linger", &user]).status().is_ok_and(|s| s.success());
        if ok {
            println!("✓ linger enabled: the service keeps running after you log out");
        } else {
            println!("! run `sudo loginctl enable-linger {user}` so sessions keep running after you log out");
        }
    }
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    if cfg!(target_os = "linux") {
        let _ = systemctl(&["disable", "--now", "glasscoded"]);
        let _ = std::fs::remove_file(unit_path());
        let _ = systemctl(&["daemon-reload"]);
        println!("✓ service removed. Running sessions stay in tmux: tmux -L glasscode ls");
    }
    Ok(())
}
