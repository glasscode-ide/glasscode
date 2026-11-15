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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _};

    /// Glasscode's folder, where `http` puts the body file, in a new temporary folder until
    /// dropped: a test leaves the person's own folder alone.
    struct TemporaryRoot {
        dir: tempfile::TempDir,
        before: Option<std::ffi::OsString>,
    }

    impl TemporaryRoot {
        fn new() -> TemporaryRoot {
            let dir = tempfile::tempdir().unwrap();
            let before = std::env::var_os("GLASSCODE_HOME");
            // SAFETY: the core tests run one at a time (`--test-threads=1`).
            unsafe { std::env::set_var("GLASSCODE_HOME", dir.path()) };
            TemporaryRoot { dir, before }
        }
    }

    impl Drop for TemporaryRoot {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            match self.before.take() {
                Some(value) => unsafe { std::env::set_var("GLASSCODE_HOME", value) },
                None => unsafe { std::env::remove_var("GLASSCODE_HOME") },
            }
        }
    }

    #[test]
    fn http_sends_the_headers_and_the_body_and_gives_the_reply() {
        // A server for one request on this computer, to see what curl sends.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/graphql", listener.local_addr().unwrap().port());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim_end().is_empty() {
                    break;
                }
                head.push(line.trim_end().to_string());
            }
            let length = head.iter().find_map(|h| h.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse::<usize>().unwrap())).unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let reply = "first line\nsecond line\n";
            write!(&stream, "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            (head, body)
        });
        let root = TemporaryRoot::new();
        let body = br#"{"query":"{ viewer { id } }"}"#.to_vec();
        let (status, reply) = http("POST", &url, &[("Authorization", "test-token".into()), ("Content-Type", "application/json".into())], Some(body.clone())).unwrap();
        let (head, received) = server.join().unwrap();
        assert_eq!(status, 201);
        // Byte for byte: curl writes the reply without a change of the line ends.
        assert_eq!(reply, b"first line\nsecond line\n");
        assert_eq!(head[0], "POST /graphql HTTP/1.1");
        assert!(head.iter().any(|h| h == "Authorization: test-token"), "{head:?}");
        assert!(head.iter().any(|h| h == "Content-Type: application/json"), "{head:?}");
        assert_eq!(received, body);
        let left: Vec<_> = std::fs::read_dir(root.dir.path()).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n.to_string_lossy().starts_with("gc-")).collect();
        assert!(left.is_empty(), "the body file is still there: {left:?}");
    }

    #[test]
    #[ignore = "needs the network"]
    fn http_gets_a_package_from_npm() {
        let _root = TemporaryRoot::new();
        let (status, body) = http("GET", "https://registry.npmjs.org/left-pad/latest", &[("Accept", "application/json".into())], None).unwrap();
        let body = String::from_utf8_lossy(&body);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""name":"left-pad""#), "{body}");
    }
}
