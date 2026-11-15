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

// ----- programs and processes (S03) ------------------------------------------------------------

/// The extensions that make a file a program (`PATHEXT`), in its order and in lower case.
fn program_extensions() -> Vec<String> {
    let list = std::env::var("PATHEXT").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    list.split(';').map(str::trim).filter(|e| e.len() > 1 && e.starts_with('.')).map(str::to_ascii_lowercase).collect()
}

/// The files that can be the program `base`: itself when it has an extension, then `base` with
/// each program extension.
fn program_files(base: &Path, extensions: &[String]) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = base.extension().map(|_| base.to_path_buf()).into_iter().collect();
    for extension in extensions {
        let mut file = base.as_os_str().to_owned();
        file.push(extension);
        files.push(file.into());
    }
    files
}

/// The first program called `name` in the folders of a PATH value. `tool` is `tool.exe`,
/// `tool.cmd`… in the order of PATHEXT, never the file `tool` itself: npm puts a shell script
/// of that name next to `tool.cmd`, and Windows cannot start it. A name with a folder in it is
/// looked for only there.
pub fn find_program(name: impl AsRef<std::ffi::OsStr>, path: impl AsRef<std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let name = Path::new(name.as_ref());
    if name.as_os_str().is_empty() {
        return None;
    }
    let extensions = program_extensions();
    if name.parent().is_some_and(|dir| !dir.as_os_str().is_empty()) {
        return program_files(name, &extensions).into_iter().find(|file| file.is_file());
    }
    std::env::split_paths(path.as_ref())
        // An empty entry is the current folder, which a program search must not use: a file
        // there could take the place of the real program.
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| program_files(&dir.join(name), &extensions))
        .find(|file| file.is_file())
}

/// A command for `program` that opens no console window. Windows itself looks on PATH only for
/// `.exe` files, so the program is found here first, which also finds the `.cmd` and `.bat`
/// files that npm and other installers put there.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    // Without it, each console program that the app starts shows a console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let program = program.as_ref();
    let found = std::env::var_os("PATH").and_then(|path| find_program(program, path));
    let mut command = std::process::Command::new(found.as_deref().map_or(program, Path::as_os_str));
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// PowerShell 7 when it is installed, else the Windows PowerShell that each Windows has.
pub fn default_shell() -> super::Shell {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let program = if find_program("pwsh.exe", &path).is_some() { "pwsh.exe" } else { "powershell.exe" };
    super::Shell { program: program.into(), session_args: vec!["-NoLogo".into()], command_args: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()] }
}
