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
    pub(super) fn io_error(error: windows::core::Error) -> io::Error {
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
    super::safe_git(program, &mut command);
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

/// Whether an entry (from `symlink_metadata`, which does not follow it) leads somewhere else: a
/// symbolic link, or any reparse point (a junction, a mount point, a cloud placeholder).
pub fn is_link(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
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

// ----- paths and agents (S06) ------------------------------------------------------------------

/// A path as people and programs write it: without the `\\?\` that `canonicalize` adds, with `\`
/// only, no doubled `\` or `\` at the end, and the drive letter in upper case. It keeps the case
/// of each name and does not look at the disk.
pub fn clean_path(path: &Path) -> std::path::PathBuf {
    let Some(text) = path.to_str() else { return path.to_path_buf() };
    let text = text.replace('/', "\\");
    let text = match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.get(..4).is_some_and(|start| start.eq_ignore_ascii_case(r"UNC\")) => format!(r"\\{}", &rest[4..]),
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') && rest.as_bytes()[0].is_ascii_alphabetic() => rest.to_string(),
        _ => text,
    };
    let mut clean: String = Path::new(&text).components().collect::<std::path::PathBuf>().to_string_lossy().into_owned();
    if clean.as_bytes().get(1) == Some(&b':') {
        clean[..1].make_ascii_uppercase();
    }
    clean.into()
}

/// Whether two paths name the same folder as Windows sees it: the same text without case, `\\?\`,
/// `/` or a `\` at the end; or, for folders that exist, the same real folder (through a junction
/// or a short 8.3 name).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let key = |path: &Path| clean_path(path).to_string_lossy().to_lowercase();
    key(a) == key(b) || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if key(&a) == key(&b))
}

/// Git Bash, where Claude Code looks for it (2.1): `CLAUDE_CODE_GIT_BASH_PATH`, the folders the
/// installer of Git for Windows uses, then next to the `git` on PATH.
fn git_bash() -> Option<std::path::PathBuf> {
    if let Some(set) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(std::path::PathBuf::from) {
        let name = set.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if ["bash.exe", "sh.exe", "bash", "sh"].contains(&name.as_str()) && set.is_file() {
            return Some(set);
        }
    }
    let installed = [r"C:\Program Files\Git\bin\bash.exe", r"C:\Program Files (x86)\Git\bin\bash.exe"].map(std::path::PathBuf::from);
    if let Some(bash) = installed.into_iter().find(|file| file.is_file()) {
        return Some(bash);
    }
    let git = find_program("git", std::env::var_os("PATH").unwrap_or_default())?;
    let bash = git.parent()?.parent()?.join("bin").join("bash.exe");
    bash.is_file().then_some(bash)
}

/// A command that runs `text` in the shell Claude Code runs the commands of its hooks and status
/// line with, for a command a person wrote for Claude Code: Git Bash when there is one, as Claude
/// Code itself prefers, else PowerShell.
pub fn claude_shell(text: &str) -> std::process::Command {
    match git_bash() {
        Some(bash) => {
            let mut command = command(bash);
            command.args(["-c", text]);
            command
        }
        None => {
            let shell = default_shell();
            let mut command = command(&shell.program);
            command.args(&shell.command_args).arg(text);
            command
        }
    }
}

/// The program that an npm `.cmd` file starts, and the arguments that come before the person's,
/// so that it can start without cmd.exe in between: cmd.exe reads `&`, `%` and `"` in an argument
/// (a first message to an agent) as its own language, and passes such an argument on cut or
/// changed. It reads the line that npm and pnpm end such a file with, `"<program>" "<script>" %*`
/// (`node` or `%_prog%` is node.exe next to the file, else on `path`). None for any other file,
/// and when a program is not there: that file then starts through cmd.exe, as before.
pub fn script_target(script: &Path, path: &std::ffi::OsStr) -> Option<(std::path::PathBuf, Vec<String>)> {
    let extension = script.extension()?.to_string_lossy().to_ascii_lowercase();
    if extension != "cmd" && extension != "bat" {
        return None;
    }
    let target = read_script_target(script, path);
    if target.is_none() {
        log::warn!("{} is a batch file that Glasscode cannot read; it starts through cmd.exe, which can change the arguments", script.display());
    }
    target
}

fn read_script_target(script: &Path, path: &std::ffi::OsStr) -> Option<(std::path::PathBuf, Vec<String>)> {
    let text = std::fs::read_to_string(script).ok()?;
    let line = text.lines().rev().find(|line| line.contains("%*"))?;
    let mut rest = line.trim_end().strip_suffix("%*")?.trim_end();
    // The words before `%*`, from the end: each in quotation marks, and pnpm's bare `node` first.
    let mut words = Vec::new();
    loop {
        if let Some(body) = rest.strip_suffix('"') {
            let start = body.rfind('"')?;
            words.push(&body[start + 1..]);
            rest = body[..start].trim_end();
        } else if rest.strip_suffix("node").is_some_and(|before| before.is_empty() || before.ends_with(char::is_whitespace)) {
            words.push("node");
            break;
        } else {
            break;
        }
    }
    words.reverse();
    let dir = script.parent()?;
    let file = |word: &str| -> Option<std::path::PathBuf> {
        if word == "node" || word == "%_prog%" {
            let next_to = dir.join("node.exe");
            return if next_to.is_file() { Some(next_to) } else { find_program("node", path) };
        }
        // `%~dp0` and npm's `%dp0%` are the folder of the file, with a `\` at the end.
        if word.replace("%~dp0", "").replace("%dp0%", "").contains('%') {
            return None;
        }
        let folder = format!("{}\\", dir.display());
        let file = clean_path(Path::new(&word.replace("%~dp0", &folder).replace("%dp0%", &folder)));
        file.is_file().then_some(file)
    };
    let mut files = words.into_iter().map(file).collect::<Option<Vec<_>>>()?;
    if files.is_empty() {
        return None;
    }
    let program = files.remove(0);
    // A program, not one more batch file (a `node.cmd` of a version manager).
    if !program.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        return None;
    }
    Some((program, files.into_iter().map(|file| file.to_string_lossy().into_owned()).collect()))
}

// ----- files, links and sounds (S10) -----------------------------------------------------------

/// File Explorer, by its full path: a file `explorer.exe` in the current folder must not start
/// in its place.
fn explorer() -> std::process::Command {
    let system = std::env::var_os("SystemRoot").map(|root| Path::new(&root).join("explorer.exe")).filter(|file| file.is_absolute() && file.is_file());
    command(system.as_deref().unwrap_or(Path::new("explorer.exe")))
}

/// A path as Explorer reads it from its command line: `clean_path` (it takes `C:/a` for a switch,
/// and does not know the `\\?\` prefix), between quotation marks, as it splits its switches at
/// commas.
fn explorer_path(path: &Path) -> String {
    // `clean_path` keeps a path that is not Unicode as it is; Explorer gets its text, cleaned.
    let text = clean_path(Path::new(path.to_string_lossy().as_ref())).to_string_lossy().into_owned();
    // A drive (`D:\`) needs its backslash, and has nothing to quote.
    if let [drive, b':', b'\\'] = text.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return text;
    }
    // A backslash before the closing quotation mark would escape it.
    format!("\"{}\"", text.trim_end_matches('\\'))
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

/// Open a link that `openable_link` lets through (http, https, the recipients of `mailto:`), and
/// refuse any other. The shell opens it itself: no command line (and no cmd.exe) reads the address.
pub(crate) fn open_link(url: &str) -> std::io::Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    let url = super::openable_link(url)?;
    // SAFETY: the strings live past the call, and no window is named.
    let result = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(url.as_str()), None, None, SW_SHOWNORMAL) };
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

// ----- agent browser (S17) ---------------------------------------------------------------------

/// Start `command` as a program of its own, as `open` starts one on the Mac: without a console,
/// in a process group of its own (a Ctrl+C for Glasscode does not reach it), out of Glasscode's
/// job when the job lets it go, and with none of Glasscode's handles (the standard library would
/// give it each inheritable one, such as the pipes of the agent whose MCP server starts it, and
/// keep them open while it runs). It lives on when Glasscode quits, and its first window shows
/// without taking the focus. Its standard streams are none. Gives its process id.
pub fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<u32> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTF_USESHOWWINDOW,
        STARTUPINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE;
    use windows::core::{PCWSTR, PWSTR};

    let program = Path::new(command.get_program());
    // A bare name would be looked for in the current folder as well, and Windows would start a
    // batch file through cmd.exe, which reads the arguments as its own language.
    if !program.is_absolute() || !program.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not the full path of a program (.exe)", program.display())));
    }
    let application: Vec<u16> = program.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut line = Vec::new();
    push_argument(&mut line, program.as_os_str());
    for arg in command.get_args() {
        line.push(u16::from(b' '));
        push_argument(&mut line, arg);
    }
    line.push(0);
    let environment = environment_block(command);
    let folder: Option<Vec<u16>> = command.get_current_dir().map(|dir| dir.as_os_str().encode_wide().chain(Some(0)).collect());
    let startup = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, dwFlags: STARTF_USESHOWWINDOW, wShowWindow: SW_SHOWNOACTIVATE.0 as u16, ..Default::default() };
    let start = |flags: PROCESS_CREATION_FLAGS| {
        // The system may write into the command line, so each try has a copy of its own.
        let mut line = line.clone();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: each string ends with a 0, the environment block with two, and all of them and
        // `startup` live past the call; no handle is inherited.
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                flags | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ref().map(|block| block.as_ptr().cast()),
                folder.as_ref().map_or(PCWSTR::null(), |folder| PCWSTR(folder.as_ptr())),
                &startup,
                &mut info,
            )
        }
        .map(|()| info)
    };
    // A job that keeps its processes (`cargo run` makes one) refuses to let one go.
    let info = match start(CREATE_BREAKAWAY_FROM_JOB) {
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => start(PROCESS_CREATION_FLAGS(0)),
        started => started,
    }
    .map_err(control::io_error)?;
    // SAFETY: two new handles that only this function has; nothing waits for the process.
    unsafe {
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// One argument as a program on Windows reads it back from its command line (the rules of the C
/// runtime and of CommandLineToArgvW): in quotation marks when it has a space, a tab or nothing
/// in it, and each `"` with the `\` before it doubled and one more.
fn push_argument(line: &mut Vec<u16>, arg: &std::ffi::OsStr) {
    use std::os::windows::ffi::OsStrExt as _;
    let (quote, backslash) = (u16::from(b'"'), u16::from(b'\\'));
    let quoted = arg.is_empty() || arg.encode_wide().any(|c| c == u16::from(b' ') || c == u16::from(b'\t'));
    if quoted {
        line.push(quote);
    }
    let mut backslashes = 0;
    for c in arg.encode_wide() {
        if c == backslash {
            backslashes += 1;
        } else {
            if c == quote {
                line.extend(std::iter::repeat_n(backslash, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(c);
    }
    if quoted {
        line.extend(std::iter::repeat_n(backslash, backslashes));
        line.push(quote);
    }
}

/// The environment of `command` for CreateProcessW, sorted by name without case as Windows wants
/// it: this process's, with the command's changes. None when the command changes nothing.
fn environment_block(command: &std::process::Command) -> Option<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt as _;
    let changes: Vec<_> = command.get_envs().collect();
    if changes.is_empty() {
        return None;
    }
    let key = |name: &std::ffi::OsStr| name.to_string_lossy().to_uppercase();
    let mut vars: std::collections::BTreeMap<String, (std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().map(|(name, value)| (key(&name), (name, value))).collect();
    for (name, value) in changes {
        match value {
            Some(value) => vars.insert(key(name), (name.to_owned(), value.to_owned())),
            None => vars.remove(&key(name)),
        };
    }
    let mut block: Vec<u16> = Vec::new();
    for (name, value) in vars.values() {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Some(block)
}

/// `std::fs::remove_dir_all`, for a folder that a program had open until a moment ago: Windows
/// lets a file go only when each process that had it has ended (a browser's profile; once Edge
/// took more than ten seconds to end after it closed), so it tries again for half a minute.
pub fn remove_dir_all(dir: &Path) -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut tried = false;
    loop {
        match std::fs::remove_dir_all(dir) {
            // Its last files were waiting to go, and went.
            Err(e) if tried && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            done => return done,
        }
        tried = true;
    }
}

// ----- resources and ports (U08) ---------------------------------------------------------------

/// The listening TCP ports of the IPv4 and the IPv6 table of Windows, each with the program
/// that owns it; `listeners` adds the names. Needs no administrator: the tables have the
/// ports of all users and of the system.
pub(crate) fn listener_rows() -> std::io::Result<Vec<super::Listener>> {
    use windows::Win32::NetworkManagement::IpHelper::{MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID};
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    // The port is in network byte order in the low 16 bits.
    let port = |value: u32| u16::from_be(value as u16);
    let mut rows = Vec::new();
    let v4 = tcp_listener_table(AF_INET.0.into())?;
    // SAFETY: the rows are in the buffer that the system filled (`table_rows` checks the count).
    for row in unsafe { table_rows::<MIB_TCPROW_OWNER_PID>(&v4, std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table)) } {
        rows.push(super::Listener { port: port(row.dwLocalPort), pid: row.dwOwningPid, process: String::new() });
    }
    let v6 = tcp_listener_table(AF_INET6.0.into())?;
    // SAFETY: as above.
    for row in unsafe { table_rows::<MIB_TCP6ROW_OWNER_PID>(&v6, std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table)) } {
        rows.push(super::Listener { port: port(row.dwLocalPort), pid: row.dwOwningPid, process: String::new() });
    }
    Ok(rows)
}

/// The listener table of GetExtendedTcpTable for one address family, with the program of each
/// row; in u32s, which keeps the rows aligned.
fn tcp_listener_table(family: u32) -> std::io::Result<Vec<u32>> {
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, TCP_TABLE_OWNER_PID_LISTENER};
    let mut size = 0u32;
    // A port can open between the call that tells the size and the one that fills the table.
    for _ in 0..8 {
        let mut buffer = vec![0u32; (size as usize).div_ceil(4).max(1)];
        size = (buffer.len() * 4) as u32;
        // SAFETY: the buffer has `size` bytes; the system writes at most that many.
        let error = unsafe { GetExtendedTcpTable(Some(buffer.as_mut_ptr().cast()), &mut size, false, family, TCP_TABLE_OWNER_PID_LISTENER, 0) };
        match error {
            e if e == NO_ERROR.0 => return Ok(buffer),
            e if e == ERROR_INSUFFICIENT_BUFFER.0 => {}
            e => return Err(std::io::Error::from_raw_os_error(e as i32)),
        }
    }
    Err(std::io::Error::other("the table of listening ports kept growing"))
}

/// The rows of a table that starts with its count (`dwNumEntries`) and has the rows from
/// `offset`; never more than the buffer holds.
///
/// SAFETY: `buffer` holds a table of rows of type `Row`, as the system filled it.
unsafe fn table_rows<Row: Copy>(buffer: &[u32], offset: usize) -> Vec<Row> {
    let bytes = buffer.len() * 4;
    let count = buffer.first().copied().unwrap_or(0) as usize;
    let room = bytes.saturating_sub(offset) / std::mem::size_of::<Row>();
    let start = buffer.as_ptr().cast::<u8>();
    // SAFETY: at most `room` rows from `offset`, inside the buffer, aligned as u32 is enough
    // for these rows; read without a reference to the table's one-row array.
    unsafe { std::slice::from_raw_parts(start.add(offset).cast::<Row>(), count.min(room)).to_vec() }
}

/// End the program `pid` at once, when it is still the one that started at `started` (seconds
/// since 1970, from the read of `end_tree`): its number may be another program's now. A program
/// that has ended already is not an error; one whose start time is not known is not ended.
/// Waits up to 5 seconds for it to be gone, so that its children do not outlive it in the order.
pub(crate) fn end_process(pid: u32, started: u64) -> std::io::Result<()> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows::Win32::Foundation::{ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
    if started == 0 {
        return Err(std::io::Error::other(format!("process {pid} was not ended: the system does not tell when it started, so it may be another program")));
    }
    // SAFETY: OpenProcess only reads its arguments; its new handle is the OwnedHandle's to close.
    let process = match unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => unsafe { OwnedHandle::from_raw_handle(handle.0) },
        // No program has the number now: it ended.
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(()),
        Err(error) => return Err(control::io_error(error)),
    };
    let handle = HANDLE(process.as_raw_handle());
    // SAFETY: the handle is open for as long as `process`, and may wait.
    let ended = || unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
    if start_time(handle)? != started || ended() {
        // Another program has the number now, or it ended: the one meant is gone.
        return Ok(());
    }
    // SAFETY: the handle may terminate; the exit code is that of Task Manager's "End task".
    let terminated = unsafe { TerminateProcess(handle, 1) };
    // TerminateProcess only starts the end, and the handle is signalled once the process is gone.
    // A process that is ending already (a shell whose program `end_tree` has just ended exits on
    // its own) makes it fail with "access denied", as for one that has ended, and is not
    // signalled yet either: so wait in both cases.
    // SAFETY: the handle may wait.
    let gone = unsafe { WaitForSingleObject(handle, 5000) } == WAIT_OBJECT_0;
    end_result(pid, terminated.map_err(control::io_error), gone)
}

/// What `end_process` gives once it has waited: a process that is gone has ended, whatever
/// TerminateProcess said; one that still runs after a refusal may not be ended by this user (it
/// runs as administrator, say).
fn end_result(pid: u32, terminated: std::io::Result<()>, gone: bool) -> std::io::Result<()> {
    match (terminated, gone) {
        (_, true) => Ok(()),
        (Err(error), false) => Err(error),
        (Ok(()), false) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("process {pid} did not end in 5 seconds"))),
    }
}

/// When the process of `handle` started, in seconds since 1970, as sysinfo counts it.
fn start_time(handle: windows::Win32::Foundation::HANDLE) -> std::io::Result<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetProcessTimes;
    let (mut created, mut unused) = (FILETIME::default(), [FILETIME::default(); 3]);
    // SAFETY: the caller's handle is open and may query; each FILETIME is written once.
    unsafe { GetProcessTimes(handle, &mut created, &mut unused[0], &mut unused[1], &mut unused[2]) }.map_err(control::io_error)?;
    // In 100 ns steps since 1601, which is 11 644 473 600 seconds before 1970.
    let since_1601 = ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)) / 10_000_000;
    Ok(since_1601.saturating_sub(11_644_473_600))
}

#[cfg(test)]
mod end_tests {
    use super::*;

    #[test]
    fn a_process_that_is_gone_has_ended_whatever_terminate_said() {
        let denied = || Err(std::io::Error::from_raw_os_error(5));
        assert!(end_result(7, Ok(()), true).is_ok());
        // Ending already, or ended: Windows refuses with "access denied", and then it is gone.
        assert!(end_result(7, denied(), true).is_ok());
        // Still running after the refusal: that stays an error.
        assert_eq!(end_result(7, denied(), false).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(end_result(7, Ok(()), false).unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_process_that_is_ending_already_is_not_an_error() {
        use std::os::windows::io::AsRawHandle as _;
        use std::process::Stdio;
        // The test's own program, ended by its handle (`Child::kill` calls TerminateProcess) and at
        // once asked to end again. The first end has only started: the second TerminateProcess is
        // refused with "access denied", while the process is not yet gone.
        for _ in 0..10 {
            let mut child = command("ping").args(["-n", "60", "127.0.0.1"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("ping starts");
            let started = start_time(windows::Win32::Foundation::HANDLE(child.as_raw_handle()));
            child.kill().unwrap();
            let ended = end_process(child.id(), started.unwrap());
            // The child holds its handle, so its number is not another program's meanwhile.
            child.wait().unwrap();
            ended.expect("a process that is ending already has ended");
        }
    }
}
