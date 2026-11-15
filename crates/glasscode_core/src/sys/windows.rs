//! `sys` on Windows.

use std::path::Path;

use super::LocalTime;

// ----- time ------------------------------------------------------------------------------------

/// The local date and time of a Unix time; None when the system cannot say.
pub fn local_time(unix_seconds: i64) -> Option<LocalTime> {
    use chrono::{Datelike as _, TimeZone as _, Timelike as _};
    let t = chrono::Local.timestamp_opt(unix_seconds, 0).single()?;
    Some(LocalTime { year: t.year(), month: t.month(), day: t.day(), hour: t.hour(), minute: t.minute(), weekday: t.weekday().num_days_from_sunday() })
}

// ----- file lock -------------------------------------------------------------------------------

/// A lock between processes on the whole file, held until dropped.
pub struct FileLock(std::fs::File);

impl FileLock {
    pub fn take(path: &Path) -> anyhow::Result<FileLock> {
        use anyhow::Context as _;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        file.lock().with_context(|| format!("could not lock {}", path.display()))?;
        Ok(FileLock(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Closing the file releases the lock as well; unlocking first frees it for the next
        // holder without waiting for the system to close the handle.
        let _ = self.0.unlock();
    }
}

// ----- control channel -------------------------------------------------------------------------

/// The way from the `glasscode` command into the app. Windows has no listener yet, so every
/// call finds the app not running; the request files of `control::call` still work.
pub mod control {
    use std::convert::Infallible;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// Where the app would listen: `GLASSCODE_CONTROL_SOCKET`, else `control.sock` in
    /// Glasscode's folder.
    pub fn socket_path() -> PathBuf {
        if let Some(path) = std::env::var_os("GLASSCODE_CONTROL_SOCKET").filter(|p| !p.is_empty()) {
            return PathBuf::from(path);
        }
        crate::paths::root().join("control.sock")
    }

    /// Nothing to guard: no folder of this user's own is made for the channel on Windows.
    pub fn prepare_socket_dir(_socket: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    /// One connection. None can exist yet (`connect` and `listen` always fail).
    pub struct Stream(Infallible);

    impl Stream {
        pub fn try_clone(&self) -> io::Result<Stream> {
            match self.0 {}
        }

        pub fn set_read_timeout(&self, _limit: Option<Duration>) -> io::Result<()> {
            match self.0 {}
        }

        pub fn peer_pid(&self) -> Option<u32> {
            match self.0 {}
        }
    }

    impl io::Read for Stream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            match self.0 {}
        }
    }

    impl io::Write for Stream {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            match self.0 {}
        }

        fn flush(&mut self) -> io::Result<()> {
            match self.0 {}
        }
    }

    /// The app does not listen on Windows yet: Glasscode counts as not running.
    pub fn connect(_path: &Path) -> io::Result<Stream> {
        Err(io::Error::new(io::ErrorKind::NotFound, "the app does not take glasscode commands on Windows yet"))
    }

    pub struct Listener(Infallible);

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            match self.0 {}
        }
    }

    pub fn listen(_path: &Path) -> io::Result<Listener> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "the app cannot take glasscode commands on Windows yet"))
    }

    /// The system itself refused the connection, as opposed to nobody listening.
    pub fn is_denied(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::PermissionDenied
    }
}
