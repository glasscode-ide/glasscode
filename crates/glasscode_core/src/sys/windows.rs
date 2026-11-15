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

/// The way from the `glasscode` command into the app: a named pipe only this user can open
/// (`\\.\pipe\glasscode-control-…`). Each command gets an instance of the pipe of its own, and
/// the app learns the process on the other end.
pub mod control {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::{AsRawHandle, FromRawHandle as _, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    use sha2::{Digest, Sha256};
    use windows::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
        PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{PCWSTR, PWSTR};

    /// Every pipe of this machine has a name that starts so; anything else is not a pipe here.
    const PIPES: &str = r"\\.\pipe\";

    /// The pipe the app listens on: `GLASSCODE_CONTROL_SOCKET`, else one named for this user and
    /// Glasscode's folder, so that a test copy with a folder of its own has a pipe of its own.
    pub fn socket_path() -> PathBuf {
        if let Some(path) = std::env::var_os("GLASSCODE_CONTROL_SOCKET").filter(|p| !p.is_empty()) {
            return PathBuf::from(path);
        }
        let user = user_sid().unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_default());
        pipe_for(&user, &crate::paths::root())
    }

    /// The name of the pipe for a user and a Glasscode folder. One folder has more spellings
    /// (`C:/Users/x` from Git Bash, `c:\users\X\`), and each gives the same name.
    pub(crate) fn pipe_for(user: &str, root: &Path) -> PathBuf {
        let folder = root.to_string_lossy().replace('/', "\\").to_lowercase();
        let mut hash = Sha256::new();
        hash.update(user.as_bytes());
        hash.update(b"\0");
        hash.update(folder.trim_end_matches('\\').as_bytes());
        let digest = hash.finalize();
        let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        PathBuf::from(format!("{PIPES}glasscode-control-{id}"))
    }

    /// Nothing to guard: a pipe has no folder, and its own security keeps others out.
    pub fn prepare_socket_dir(_socket: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    /// This user's security identifier (S-1-5-21-…). It names the pipe and is the only one let
    /// in; unlike the user name, a rename of the account does not change it.
    fn user_sid() -> io::Result<String> {
        static SID: OnceLock<String> = OnceLock::new();
        if let Some(sid) = SID.get() {
            return Ok(sid.clone());
        }
        let mut token = HANDLE::default();
        // SAFETY: the token handle is ours to close (OwnedHandle does), and each buffer given to
        // the system is as large as the size passed with it.
        let sid = unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io_error)?;
            let token = OwnedHandle::from_raw_handle(token.0);
            // The first call only says how much room the answer needs.
            let mut size = 0u32;
            let _ = GetTokenInformation(raw(&token), TokenUser, None, 0, &mut size);
            // TOKEN_USER holds a pointer: u64 keeps the buffer aligned for it.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            GetTokenInformation(raw(&token), TokenUser, Some(buffer.as_mut_ptr().cast()), size, &mut size).map_err(io_error)?;
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(io_error)?;
            let sid = text.to_string();
            LocalFree(Some(HLOCAL(text.0.cast())));
            sid.map_err(io::Error::other)?
        };
        Ok(SID.get_or_init(|| sid).clone())
    }

    /// A Windows error with the kind the standard library gives the same error (not found,
    /// denied), which `is_denied` and the callers look at.
    fn io_error(error: windows::core::Error) -> io::Error {
        let code = error.code().0 as u32;
        // An HRESULT made from a Win32 error number is 0x8007 and the number.
        if code >> 16 == 0x8007 { io::Error::from_raw_os_error((code & 0xffff) as i32) } else { io::Error::other(error) }
    }

    fn raw(handle: &impl AsRawHandle) -> HANDLE {
        HANDLE(handle.as_raw_handle())
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// The name of a pipe as Windows takes it. Any other path is refused, so that a wrong
    /// `GLASSCODE_CONTROL_SOCKET` never opens a file, or a pipe on another computer.
    fn pipe_name(path: &Path) -> io::Result<Vec<u16>> {
        let text = path.as_os_str().to_string_lossy();
        if !text.get(..PIPES.len()).is_some_and(|start| start.eq_ignore_ascii_case(PIPES)) || text.len() == PIPES.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not the name of a pipe ({PIPES}…)", path.display())));
        }
        Ok(wide(path.as_os_str()))
    }

    /// Which end of the pipe a stream is.
    #[derive(Clone, Copy, Debug)]
    enum End {
        App,
        Command,
    }

    /// One connection, from either end.
    pub struct Stream {
        pipe: std::fs::File,
        end: End,
        /// Shared with the clones, as the time limit of a socket is.
        read_limit: Arc<Mutex<Option<Duration>>>,
    }

    impl Stream {
        fn new(pipe: std::fs::File, end: End) -> Stream {
            Stream { pipe, end, read_limit: Arc::default() }
        }

        /// A second handle on the same connection (one to read, one to write).
        pub fn try_clone(&self) -> io::Result<Stream> {
            Ok(Stream { pipe: self.pipe.try_clone()?, end: self.end, read_limit: self.read_limit.clone() })
        }

        /// How long a read may wait; past it, the read fails with `TimedOut`.
        pub fn set_read_timeout(&self, limit: Option<Duration>) -> io::Result<()> {
            if limit == Some(Duration::ZERO) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "a read time limit cannot be zero"));
            }
            *self.read_limit.lock().unwrap_or_else(PoisonError::into_inner) = limit;
            Ok(())
        }

        /// The process on the other end.
        pub fn peer_pid(&self) -> Option<u32> {
            let mut pid = 0u32;
            // SAFETY: the handle is open for as long as `self`, and the call writes one u32.
            let found = unsafe {
                match self.end {
                    End::App => GetNamedPipeClientProcessId(raw(&self.pipe), &mut pid),
                    End::Command => GetNamedPipeServerProcessId(raw(&self.pipe), &mut pid),
                }
            };
            (found.is_ok() && pid != 0).then_some(pid)
        }

        /// The bytes that wait in the pipe; a read of this many does not wait.
        fn waiting_bytes(&self) -> io::Result<u32> {
            let mut waiting = 0u32;
            // SAFETY: the handle is open for as long as `self`, and the call writes one u32.
            unsafe { PeekNamedPipe(raw(&self.pipe), None, 0, None, Some(&mut waiting), None) }.map_err(io_error)?;
            Ok(waiting)
        }
    }

    impl io::Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let limit = *self.read_limit.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(limit) = limit else { return self.pipe.read(buf) };
            // A read that waits cannot be stopped at the time limit, and meanwhile it holds up
            // each write on the same pipe (Windows does one call at a time on a handle without
            // overlapped I/O). So wait until there is data, and read only then.
            let started = Instant::now();
            let mut pause = Duration::from_millis(1);
            loop {
                match self.waiting_bytes() {
                    Ok(0) => {}
                    Ok(_) => return self.pipe.read(buf),
                    // The other end closed the pipe: the end of the data, as with a socket.
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                    Err(e) => return Err(e),
                }
                let left = limit.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the other end of the pipe sent nothing in time"));
                }
                std::thread::sleep(pause.min(left));
                pause = (pause * 2).min(Duration::from_millis(20));
            }
        }
    }

    impl io::Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.pipe.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.pipe.flush()
        }
    }

    /// Reach the app listening at `path`. When each instance serves a command, wait a moment
    /// for one to come free.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        let name = pipe_name(path)?;
        let started = Instant::now();
        loop {
            // The app may know who calls, but may not act as the caller.
            match std::fs::OpenOptions::new().read(true).write(true).security_qos_flags(SECURITY_IDENTIFICATION.0).open(path) {
                Ok(pipe) => return Ok(Stream::new(pipe, End::Command)),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) && started.elapsed() < Duration::from_secs(5) => {
                    // SAFETY: `name` ends with a 0. The open that comes next tells the result.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 500) };
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Where the app waits for the `glasscode` command.
    pub struct Listener {
        name: Vec<u16>,
        /// Who may open the pipe, in the security descriptor language (SDDL).
        security: Vec<u16>,
        /// The instance that waits for the next command.
        waiting: Mutex<Option<OwnedHandle>>,
    }

    impl Listener {
        /// The next connection.
        pub fn accept(&self) -> io::Result<Stream> {
            let waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner).take();
            let pipe = match waiting {
                Some(pipe) => pipe,
                None => instance(&self.name, &self.security, false)?,
            };
            // SAFETY: the handle is open; no overlapped I/O, so the call waits for a command.
            let connected = match unsafe { ConnectNamedPipe(raw(&pipe), None) } {
                Ok(()) => Ok(()),
                // The command came between making the instance and this call.
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
                Err(e) => Err(io_error(e)),
            };
            // A free instance for the next command before this one is served: a command that
            // finds no instance at all takes the app for closed.
            match instance(&self.name, &self.security, false) {
                Ok(next) => *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = Some(next),
                Err(e) => log::warn!("could not make the next instance of the control pipe: {e}"),
            }
            connected?;
            Ok(Stream::new(std::fs::File::from(pipe), End::App))
        }
    }

    /// Listen at `path`, for this user only and only on this computer. It fails when the pipe
    /// exists already (another copy of the app, or a program that took its name), so check that
    /// nobody answers (`connect`) before this.
    pub fn listen(path: &Path) -> io::Result<Listener> {
        let name = pipe_name(path)?;
        // A protected list (nothing inherited) that lets this user do everything.
        let security = wide(OsStr::new(&format!("D:P(A;;GA;;;{})", user_sid()?)));
        let first = instance(&name, &security, true)?;
        Ok(Listener { name, security, waiting: Mutex::new(Some(first)) })
    }

    /// A new instance of the pipe. The `first` one fails when the name is taken.
    fn instance(name: &[u16], security: &[u16], first: bool) -> io::Result<OwnedHandle> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `security` ends with a 0; the descriptor is freed below.
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(security.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }.map_err(io_error)?;
        let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into() };
        let open_mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { FILE_FLAGS_AND_ATTRIBUTES(0) };
        let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: `name` ends with a 0, and `attributes` and its descriptor live past the call.
        let handle = unsafe { CreateNamedPipeW(PCWSTR(name.as_ptr()), open_mode, pipe_mode, PIPE_UNLIMITED_INSTANCES, 64 * 1024, 64 * 1024, 0, Some(&raw const attributes)) };
        let error = io::Error::last_os_error();
        // SAFETY: the system made the descriptor with LocalAlloc, and nothing uses it now.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(error);
        }
        // SAFETY: a new handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }

    /// The system itself refused the connection (the pipe of another user, or a sandbox that
    /// may not open it), as opposed to nobody listening.
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
        // An empty or relative entry (`.`) means the current folder, which a program search must
        // not use: a file in a repository could take the place of the real program.
        .filter(|dir| dir.is_absolute())
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

/// PowerShell 7 when it is installed, else the Windows PowerShell that each Windows has. It is a
/// full path: Windows looks for a bare name in the current folder too, and a file `pwsh.exe` in
/// a worktree must not start in place of the shell.
pub fn default_shell() -> super::Shell {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let system = std::env::var_os("SystemRoot").map(|root| Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")).filter(|file| file.is_absolute() && file.is_file());
    let program = find_program("pwsh.exe", &path).or(system).or_else(|| find_program("powershell.exe", &path));
    let program = program.map_or_else(|| "powershell.exe".to_string(), |file| file.to_string_lossy().into_owned());
    super::Shell { program, session_args: vec!["-NoLogo".into()], command_args: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()] }
}

/// The name of this PC.
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME").ok().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "PC".into())
}
