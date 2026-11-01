//! Small shared helpers: a `curl` wrapper (headers on stdin, so tokens never show in a
//! process listing) and a URL-safe random token generator.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::{RngCore as _, SeedableRng as _};

use crate::paths;

pub(crate) fn http(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let _ = paths::ensure_dirs();
    let mut body_file = None;
    let mut cmd = Command::new("/usr/bin/curl");
    cmd.args(["-sS", "--max-time", "90", "-X", method, "-H", "@-", "-w", "\n%{http_code}"]);
    if let Some(bytes) = body {
        let file = tempfile::Builder::new()
            .prefix("gc-")
            .tempfile_in(paths::root())?;
        std::fs::write(file.path(), bytes)?;
        cmd.arg("--data-binary").arg(format!("@{}", file.path().display()));
        body_file = Some(file);
    }
    cmd.arg(url);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("could not run curl")?;
    {
        let mut stdin = child.stdin.take().context("stdin")?;
        for (name, value) in headers {
            writeln!(stdin, "{name}: {value}")?;
        }
    }
    let output = child.wait_with_output()?;
    drop(body_file);
    if !output.status.success() {
        bail!("network error: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let mut bytes = output.stdout;
    let split = bytes.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let status: u16 = String::from_utf8_lossy(&bytes[split..]).trim().parse().unwrap_or(0);
    bytes.truncate(split);
    Ok((status, bytes))
}

pub(crate) fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::StdRng::from_os_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// A cross-process advisory file lock (flock), held until dropped.
pub(crate) struct FileLock(std::fs::File);

impl FileLock {
    pub(crate) fn take(path: &std::path::Path) -> anyhow::Result<FileLock> {
        use std::os::fd::AsRawFd;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            bail!("could not lock {}", path.display());
        }
        Ok(FileLock(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: as above.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
