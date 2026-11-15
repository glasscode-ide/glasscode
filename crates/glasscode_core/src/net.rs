//! Small shared helpers: a `curl` wrapper (headers on stdin, so tokens never show in a
//! process listing) and a URL-safe random token generator.

use std::io::Write;
use std::process::Stdio;

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
    let mut cmd = crate::sys::command(crate::sys::CURL);
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
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        // Windows has curl.exe only from Windows 10 version 1803 on.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!("curl is not installed"),
        Err(error) => return Err(error).context("could not run curl"),
    };
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

// The lock between processes lives with the other operating system code; callers still find
// it here.
pub(crate) use crate::sys::FileLock;
