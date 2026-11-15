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
