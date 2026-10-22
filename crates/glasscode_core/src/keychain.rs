//! Secrets in the macOS Keychain, through `/usr/bin/security`. A secret never goes on a
//! command line: `security -i` reads the command that stores it from stdin.

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};

pub fn read(service: &str, account: &str) -> Option<String> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let secret = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!secret.is_empty()).then_some(secret)
}

pub fn write(service: &str, account: &str, secret: &str) -> anyhow::Result<()> {
    if [service, account, secret].iter().any(|s| s.contains('"') || s.contains('\n') || s.contains('\\')) {
        bail!("unexpected characters in what goes to the Keychain");
    }
    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let line = format!("add-generic-password -U -s \"{service}\" -a \"{account}\" -w \"{secret}\"\n");
    child.stdin.take().context("stdin")?.write_all(line.as_bytes())?;
    if !child.wait()?.success() {
        bail!("could not save it to the Keychain");
    }
    Ok(())
}

pub fn delete(service: &str, account: &str) {
    let _ = Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-s", service, "-a", account])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
