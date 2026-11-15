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
    use windows::Win32::Foundation::{ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SUCCESS, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{EqualSid, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
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
        connect_to_owner(path, &user_sid()?)
    }

    /// `connect`, to a pipe that belongs to the account `owner`. Anyone can work out the name of
    /// the pipe, and another account could make it before the app does: the command would then
    /// tell that account what it asks, and print its answers.
    pub(crate) fn connect_to_owner(path: &Path, owner: &str) -> io::Result<Stream> {
        let name = pipe_name(path)?;
        let started = Instant::now();
        loop {
            // The app may know who calls, but may not act as the caller.
            match std::fs::OpenOptions::new().read(true).write(true).security_qos_flags(SECURITY_IDENTIFICATION.0).open(path) {
                // Not a byte goes out before the owner is known; on an error the pipe closes here.
                Ok(pipe) => match owned_by(&pipe, owner) {
                    Ok(true) => return Ok(Stream::new(pipe, End::Command)),
                    // Not "denied": the request files would not reach an app that has no pipe,
                    // and the reason would be lost.
                    Ok(false) => return Err(io::Error::other(format!("the control pipe {} belongs to a different account", path.display()))),
                    Err(e) => return Err(io::Error::other(format!("could not tell who owns the control pipe {} ({e})", path.display()))),
                },
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) && started.elapsed() < Duration::from_secs(5) => {
                    // SAFETY: `name` ends with a 0. The open that comes next tells the result.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 500) };
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Whether the pipe of `pipe` has the account `owner` (a SID in text) as its owner.
    fn owned_by(pipe: &std::fs::File, owner: &str) -> io::Result<bool> {
        let mut found = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the handle is open (and may read the security: GENERIC_READ has READ_CONTROL);
        // `found` points into `descriptor`, which is freed below after the last use.
        let error = unsafe { GetSecurityInfo(raw(pipe), SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut found), None, None, None, Some(&mut descriptor)) };
        if error != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(error.0 as i32));
        }
        let wanted = wide(OsStr::new(owner));
        let mut sid = PSID::default();
        // SAFETY: `wanted` ends with a 0; the SID that the call makes is freed below.
        let same = unsafe { ConvertStringSidToSidW(PCWSTR(wanted.as_ptr()), &mut sid) }.map_err(io_error).map(|()| {
            // SAFETY: two valid SIDs; the system made `sid` with LocalAlloc.
            let same = unsafe { EqualSid(found, sid) }.is_ok();
            unsafe { LocalFree(Some(HLOCAL(sid.0))) };
            same
        });
        // SAFETY: the system made the descriptor with LocalAlloc, and nothing uses `found` now.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        same
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
        // This user owns the pipe, which `connect` checks, and a protected list (nothing
        // inherited) lets this user alone do everything. The owner is named, because an app
        // that runs as administrator would otherwise give the pipe to the Administrators group,
        // which `connect` refuses: other administrators are other accounts.
        let sid = user_sid()?;
        let security = wide(OsStr::new(&format!("O:{sid}D:P(A;;GA;;;{sid})")));
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Read as _;

        fn test_pipe() -> PathBuf {
            let mut id = [0u8; 8];
            crate::sys::random_bytes(&mut id);
            PathBuf::from(format!("{PIPES}glasscode-owner-test-{}", id.iter().map(|b| format!("{b:02x}")).collect::<String>()))
        }

        #[test]
        fn the_pipe_of_the_app_belongs_to_this_user() {
            let path = test_pipe();
            let _app = listen(&path).unwrap();
            let stream = connect(&path).unwrap();
            assert!(owned_by(&stream.pipe, &user_sid().unwrap()).unwrap());
        }

        #[test]
        fn a_pipe_of_a_different_account_is_refused_before_a_byte_goes_out() {
            // A test has no second account, and Windows lets it name no one else as the owner of
            // a new pipe (error 1307); so here the command expects another owner (SYSTEM).
            let path = test_pipe();
            let listener = listen(&path).unwrap();
            let Err(error) = connect_to_owner(&path, "S-1-5-18") else { panic!("connected to the pipe of another account") };
            assert!(error.to_string().contains("belongs to a different account"), "{error}");
            assert!(!is_denied(&error), "{error}");
            // The command went away without a word.
            if let Ok(mut app) = listener.accept() {
                let mut got = Vec::new();
                app.read_to_end(&mut got).unwrap();
                assert!(got.is_empty(), "{got:?}");
            }
        }
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

// ----- secrets and network (S12) ---------------------------------------------------------------

/// The curl that Windows 10 and 11 have in System32 (`net::http`). With the extension in the
/// name, `command` cannot start a `curl.cmd` or `curl.bat` from earlier on PATH: a batch file
/// would not get the arguments of `net::http` intact.
pub const CURL: &str = "curl.exe";

/// Secrets in Windows Credential Manager: a generic credential named `<service>/<account>`, the
/// name that the Credential Manager window shows. Only this user can read it, and it stays on
/// this PC (it does not roam with the profile).
pub mod secrets {
    use anyhow::Context as _;
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW};
    use windows::core::{HRESULT, PCWSTR, PWSTR};

    /// The store, as messages name it.
    pub const NAME: &str = "Windows Credential Manager";

    /// A string as Windows takes it: UTF-16 with a zero at the end.
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    fn target(service: &str, account: &str) -> Vec<u16> {
        wide(&format!("{service}/{account}"))
    }

    /// No credential has the name: nothing to read or delete, which is not a failure.
    fn is_missing(error: &windows::core::Error) -> bool {
        error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
    }

    pub fn read(service: &str, account: &str) -> Option<String> {
        let target = target(service, account);
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` ends with a zero. On success the system gives a credential that
        // only CredFree releases.
        if let Err(error) = unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut credential) } {
            if !is_missing(&error) {
                log::warn!("could not read {service}/{account} from {NAME}: {error}");
            }
            return None;
        }
        // SAFETY: the blob has `CredentialBlobSize` bytes, which live until CredFree.
        let blob = unsafe {
            let c = &*credential;
            let blob = if c.CredentialBlob.is_null() { Vec::new() } else { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec() };
            CredFree(credential as *const _);
            blob
        };
        // UTF-16, as `write` keeps it and as the Credential Manager window keeps a password that a
        // person types there.
        let units: Vec<u16> = blob.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let secret = String::from_utf16(&units).ok()?.trim().to_string();
        (!secret.is_empty()).then_some(secret)
    }

    /// Saves the secret, in place of one that has the same name.
    pub fn write(service: &str, account: &str, secret: &str) -> anyhow::Result<()> {
        let (mut target, mut user) = (target(service, account), wide(account));
        let mut blob: Vec<u8> = secret.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user.as_mut_ptr()),
            CredentialBlobSize: blob.len().try_into()?,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: each pointer in `credential` goes into a buffer that lives past the call.
        unsafe { CredWriteW(&credential, 0) }.with_context(|| format!("could not save it to {NAME}"))
    }

    pub fn delete(service: &str, account: &str) {
        let target = target(service, account);
        // SAFETY: `target` ends with a zero.
        if let Err(error) = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) }
            && !is_missing(&error)
        {
            log::warn!("could not delete {service}/{account} from {NAME}: {error}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Deletes the credential of the test at the end, also after a failed assertion.
        struct DeleteAtEnd<'a>(&'a str, &'a str);

        impl Drop for DeleteAtEnd<'_> {
            fn drop(&mut self) {
                delete(self.0, self.1);
            }
        }

        #[test]
        fn write_read_delete_and_read_again() {
            // A name with a random part, so that the test never touches a credential of the person.
            let mut random = [0u8; 8];
            crate::sys::random_bytes(&mut random);
            let service = format!("Glasscode test {}", random.iter().map(|b| format!("{b:02x}")).collect::<String>());
            let account = "api-key";
            let _delete = DeleteAtEnd(&service, account);
            assert_eq!(read(&service, account), None);
            write(&service, account, "test-value-1").unwrap();
            write(&service, account, "test-välue-2 ✓").unwrap();
            assert_eq!(read(&service, account).as_deref(), Some("test-välue-2 ✓"));
            delete(&service, account);
            assert_eq!(read(&service, account), None);
        }
    }
}

// ----- files, links and sounds (S10) -----------------------------------------------------------

/// File Explorer, by its full path: a file `explorer.exe` in the current folder must not start
/// in its place.
fn explorer() -> std::process::Command {
    let system = std::env::var_os("SystemRoot").map(|root| Path::new(&root).join("explorer.exe")).filter(|file| file.is_absolute() && file.is_file());
    command(system.as_deref().unwrap_or(Path::new("explorer.exe")))
}

/// A path as Explorer reads it from its command line: with backslashes (it takes `C:/a` for a
/// switch), without the `\\?\` prefix that it does not know, and between quotation marks, as it
/// splits its switches at commas.
fn explorer_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = match (text.strip_prefix(r"\\?\UNC\"), text.strip_prefix(r"\\?\")) {
        (Some(share), _) => format!(r"\\{share}"),
        (None, Some(local)) => local.to_string(),
        (None, None) => text,
    };
    // A backslash before the closing quotation mark would escape it. A drive (`D:\`) needs its
    // backslash, and has nothing to quote.
    let trimmed = text.trim_end_matches('\\');
    if trimmed.len() <= 2 {
        return text;
    }
    format!("\"{trimmed}\"")
}

/// `explorer.exe <path>`: a folder in File Explorer, a file in the program for its type. A file
/// that this program would run (a program, a script, a shortcut) is only shown in File Explorer
/// (`reveal_path_command`): a worktree holds code from other people and from agents, and to open
/// a file must never run it.
pub(crate) fn open_path_command(path: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    if runs_when_opened(path) {
        return reveal_path_command(path);
    }
    let mut explorer = explorer();
    // Raw, as the standard quoting would not quote a path with a comma.
    explorer.raw_arg(explorer_path(path));
    explorer
}

/// The types that Windows, or a program that people often install (Python, Git Bash, Java),
/// runs or installs when the file is opened, besides the program types of PATHEXT. Lower case.
const RUNS_WHEN_OPENED: &[&str] = &[
    // Programs, installers and parts of the system.
    "exe", "com", "scr", "pif", "cpl", "msc", "msi", "msp", "mst", "msix", "msixbundle", "appx", "appxbundle", "appinstaller", "application", "appref-ms", "gadget", "xbap", "xll", "dll", "ocx", "sys", "drv",
    "diagcab", "diagcfg", "diagpkg", "wsb", "job", "mof",
    // Scripts.
    "bat", "cmd", "ps1", "psm1", "psd1", "ps1xml", "psc1", "msh", "msh1", "msh2", "mshxml", "msh1xml", "msh2xml", "vb", "vbs", "vbe", "js", "jse", "wsf", "wsh", "ws", "wsc", "sct", "hta", "py", "pyw",
    "pyz", "pyzw", "pyc", "pyo", "rb", "rbw", "pl", "tcl", "lua", "php", "sh", "bash", "ksh", "csh", "zsh", "jar", "reg", "inf", "ins", "isp", "scf", "shb", "shs", "ade", "adp", "mda", "mde", "accde", "chm", "hlp",
    // Shortcuts and what Windows follows or mounts.
    "lnk", "url", "website", "library-ms", "search-ms", "searchconnector-ms", "settingcontent-ms", "theme", "themepack", "deskthemepack", "rdp", "vhd", "vhdx", "iso", "img",
];

/// Whether the program that Windows has for the file at `path` would run it, or would follow
/// it somewhere else. A folder only opens in File Explorer.
fn runs_when_opened(path: &Path) -> bool {
    if path.is_dir() {
        return false;
    }
    let Some(name) = path.file_name().map(|n| n.to_string_lossy()) else { return false };
    // Windows drops dots and spaces at the end of a name (`run.exe.` is `run.exe`), and a colon
    // names a stream of a file (`notes.txt:run.exe`): neither is in a name that is only a file.
    let name = name.trim_end_matches(['.', ' ']);
    if name.contains(':') {
        return true;
    }
    let Some((_, extension)) = name.rsplit_once('.') else { return false };
    let extension = extension.to_ascii_lowercase();
    RUNS_WHEN_OPENED.contains(&extension.as_str()) || program_extensions().iter().any(|program| program.trim_start_matches('.') == extension)
}

/// `explorer.exe /select,<path>`: the file selected in a File Explorer window.
pub(crate) fn reveal_path_command(path: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    let mut explorer = explorer();
    // One argument with the switch: the standard quoting would put the switch in the quotation
    // marks too.
    explorer.raw_arg(format!("/select,{}", explorer_path(path)));
    explorer
}

/// The person's default browser, with an address that `open_url` checked. The shell opens it
/// itself: no command line (and no cmd.exe) reads the address.
pub(crate) fn open_web_address(url: &str) -> std::io::Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    // SAFETY: the strings live past the call, and no window is named.
    let result = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL) };
    // A number above 32 means it started; one at most 32 is the error.
    if result.0 as usize > 32 {
        return Ok(());
    }
    Err(std::io::Error::other(format!("Windows could not open {url} (error {})", result.0 as usize)))
}

/// Play a notification's sound; nothing waits for it to end. It goes to the volume of the system
/// sounds, and a person who chose no sound for the event hears nothing.
pub fn play_sound(sound: super::Sound) {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT, SND_SYSTEM};
    // SAFETY: the name is a static string that ends with a 0.
    if !unsafe { PlaySoundW(sound_alias(sound), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT | SND_SYSTEM) }.as_bool() {
        log::warn!("could not play the {sound:?} sound");
    }
}

/// The Windows sound event of each sound (Control Panel > Sound > Sounds).
pub(crate) fn sound_alias(sound: super::Sound) -> windows::core::PCWSTR {
    match sound {
        super::Sound::NeedsYou => windows::core::w!("Notification.Reminder"),
        super::Sound::Done => windows::core::w!("Notification.Default"),
    }
}
