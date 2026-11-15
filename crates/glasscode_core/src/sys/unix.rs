//! `sys` on macOS (and on Linux, where the server daemon runs).

use std::path::Path;

use super::LocalTime;

// ----- time ------------------------------------------------------------------------------------

/// The local date and time of a Unix time; None when the system cannot say.
pub fn local_time(unix_seconds: i64) -> Option<LocalTime> {
    let t = unix_seconds as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we give it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some(LocalTime { year: tm.tm_year + 1900, month: (tm.tm_mon + 1) as u32, day: tm.tm_mday as u32, hour: tm.tm_hour as u32, minute: tm.tm_min as u32, weekday: tm.tm_wday as u32 })
}

// ----- file lock -------------------------------------------------------------------------------

/// A cross-process advisory file lock (flock), held until dropped.
pub struct FileLock(std::fs::File);

impl FileLock {
    pub fn take(path: &Path) -> anyhow::Result<FileLock> {
        use std::os::fd::AsRawFd;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            anyhow::bail!("could not lock {}", path.display());
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

// ----- control channel -------------------------------------------------------------------------

/// The way from the `glasscode` command into the app: a Unix socket only this user can open.
pub mod control {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use sha2::{Digest, Sha256};

    /// The socket the app listens on: `GLASSCODE_CONTROL_SOCKET`, else `control.sock` in
    /// Glasscode's folder (in a folder of this user's own under /tmp when that path would be too
    /// long for a socket).
    pub fn socket_path() -> PathBuf {
        if let Some(path) = std::env::var_os("GLASSCODE_CONTROL_SOCKET").filter(|p| !p.is_empty()) {
            return PathBuf::from(path);
        }
        let path = crate::paths::root().join("control.sock");
        if path.as_os_str().len() < 100 {
            return path;
        }
        let mut hash = Sha256::new();
        hash.update(path.as_os_str().as_encoded_bytes());
        let digest = hash.finalize();
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/glasscode-{uid}/control-{:02x}{:02x}{:02x}{:02x}.sock", digest[0], digest[1], digest[2], digest[3]))
    }

    /// Make sure the socket's folder is this user's alone before listening there: the /tmp folder
    /// is made 0700, and one that belongs to someone else (or is open to others) is refused.
    pub fn prepare_socket_dir(socket: &Path) -> anyhow::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let Some(dir) = socket.parent() else { return Ok(()) };
        // Only our own auto-made folder under /tmp needs guarding; an explicitly set socket path
        // (a test or dev override) is trusted, and a socket in ~/.glasscode needs nothing.
        let ours = dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("glasscode-")) && dir.parent().is_some_and(|p| p == Path::new("/tmp") || p == Path::new("/private/tmp"));
        if !ours {
            return Ok(());
        }
        if !dir.exists() {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        let meta = std::fs::symlink_metadata(dir)?;
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != uid {
            anyhow::bail!("{} is not this user's own folder", dir.display());
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// The process on the other end of a Unix socket.
    pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        #[cfg(target_os = "macos")]
        {
            let mut pid: libc::pid_t = 0;
            let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
            // SAFETY: getsockopt writes at most `len` bytes into `pid`.
            let ok = unsafe { libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&mut pid as *mut libc::pid_t).cast(), &mut len) } == 0;
            return (ok && pid > 0).then_some(pid as u32);
        }
        #[cfg(target_os = "linux")]
        {
            let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: getsockopt writes at most `len` bytes into `cred`.
            let ok = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) } == 0;
            return (ok && cred.pid > 0).then_some(cred.pid as u32);
        }
        #[allow(unreachable_code)]
        None
    }

    /// One connection, from either end.
    pub struct Stream(UnixStream);

    impl Stream {
        /// A second handle on the same connection (one to read, one to write).
        pub fn try_clone(&self) -> io::Result<Stream> {
            self.0.try_clone().map(Stream)
        }

        /// How long a read may wait; past it, the read fails with `WouldBlock` or `TimedOut`.
        pub fn set_read_timeout(&self, limit: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(limit)
        }

        /// The process on the other end.
        pub fn peer_pid(&self) -> Option<u32> {
            peer_pid(&self.0)
        }
    }

    impl io::Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl io::Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    /// Reach the app listening at `path`.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path).map(Stream)
    }

    /// Where the app waits for the `glasscode` command.
    pub struct Listener(UnixListener);

    impl Listener {
        /// The next connection.
        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept().map(|(stream, _)| Stream(stream))
        }
    }

    /// Listen at `path`, for this user only. A socket file left there by an app that has quit
    /// goes first, so check that nobody answers (`connect`) before this.
    pub fn listen(path: &Path) -> io::Result<Listener> {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener(listener))
    }

    /// The system itself refused the connection (a sandbox, such as Codex's, that may not open
    /// sockets), as opposed to nobody listening.
    pub fn is_denied(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES))
    }
}

// ----- programs and processes (S03) ------------------------------------------------------------

/// The first file called `name` in the folders of a PATH value.
pub fn find_program(name: impl AsRef<std::ffi::OsStr>, path: impl AsRef<std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let name = name.as_ref();
    std::env::split_paths(path.as_ref()).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

/// A command for `program`; Unix finds it on PATH by itself.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

/// The person's own shell (zsh, the macOS default, when `SHELL` is not set), as a login shell
/// so that it reads their profile.
pub fn default_shell() -> super::Shell {
    let program = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".to_string());
    super::Shell { program, session_args: vec!["-l".into()], command_args: vec!["-l".into(), "-c".into()] }
}

/// The name of this Mac, as the person set it in System Settings.
pub fn computer_name() -> String {
    command("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

// ----- secrets and network (S12) ---------------------------------------------------------------

/// The curl of the system (`net::http`).
pub const CURL: &str = "/usr/bin/curl";

/// Secrets in the macOS Keychain, through `/usr/bin/security`. A secret never goes on a
/// command line: `security -i` reads the command that stores it from stdin.
pub mod secrets {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    use anyhow::{Context as _, bail};

    /// The store, as messages name it.
    pub const NAME: &str = "the Keychain";

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
}

// ----- files, links and sounds (S10) -----------------------------------------------------------

/// `open`: Finder for a folder, the program for the type of a file.
pub(crate) fn open_path_command(path: &Path) -> std::process::Command {
    let mut open = command("open");
    open.arg(path);
    open
}

/// `open -R`: the file selected in a Finder window.
pub(crate) fn reveal_path_command(path: &Path) -> std::process::Command {
    let mut open = command("open");
    open.arg("-R").arg(path);
    open
}

/// Open an address in the program the Mac has for it (the browser, Mail for `mailto:`…).
pub fn open_url(url: &str) -> std::io::Result<()> {
    open_url_command(url).spawn().map(drop)
}

/// `open <address>`.
pub(crate) fn open_url_command(url: &str) -> std::process::Command {
    let mut open = command("open");
    open.arg(url);
    open
}

/// Play a notification's sound; nothing waits for it to end.
pub fn play_sound(sound: super::Sound) {
    if let Err(error) = sound_command(sound).spawn() {
        log::warn!("could not play the {sound:?} sound: {error}");
    }
}

/// `afplay` with one of the system sounds of the Mac.
pub(crate) fn sound_command(sound: super::Sound) -> std::process::Command {
    use std::process::Stdio;
    let file = match sound {
        super::Sound::NeedsYou => "/System/Library/Sounds/Glass.aiff",
        super::Sound::Done => "/System/Library/Sounds/Hero.aiff",
    };
    let mut afplay = command("/usr/bin/afplay");
    afplay.arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    afplay
}
