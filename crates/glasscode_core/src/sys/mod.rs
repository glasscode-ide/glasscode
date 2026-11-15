//! The operating system under Glasscode. The rest of the core calls these and keeps no
//! platform conditions of its own; `unix.rs` and `windows.rs` give the same names.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// ----- time ------------------------------------------------------------------------------------

/// A moment in this machine's time zone (`local_time`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
    /// 0–23.
    pub hour: u32,
    /// 0–59.
    pub minute: u32,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u32,
}

// ----- random bytes ----------------------------------------------------------------------------

/// Random bytes for secrets and tokens, from the operating system's source (never the clock,
/// which anyone can guess).
pub fn random_bytes(out: &mut [u8]) {
    use rand::{RngCore as _, SeedableRng as _};
    rand::rngs::StdRng::from_os_rng().fill_bytes(out);
}

// ----- programs and processes (S03) ------------------------------------------------------------

/// The shell for terminal sessions and for one command (`default_shell`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub program: String,
    /// The arguments that start an interactive session.
    pub session_args: Vec<String>,
    /// The arguments that come before the text of one command.
    pub command_args: Vec<String>,
}

/// One running program (`processes`).
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    /// 0 when it has none, or the system does not say.
    pub parent: u32,
    pub name: String,
    /// Its work folder; None when the system does not show it (a process of another user).
    pub dir: Option<std::path::PathBuf>,
}

/// Every running program, with its parent and its work folder.
pub fn processes() -> Vec<Process> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    // Threads are not programs (Linux lists them as tasks).
    let kind = ProcessRefreshKind::nothing().without_tasks().with_cwd(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    system.processes().values().map(|p| Process { pid: p.pid().as_u32(), parent: p.parent().map_or(0, |parent| parent.as_u32()), name: p.name().to_string_lossy().into_owned(), dir: p.cwd().map(std::path::Path::to_path_buf) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_time_fields_are_in_range() {
        // 2026-10-20 09:00 UTC: whatever the time zone, the 19th or the 20th.
        let t = local_time(1_792_486_800).expect("the system knows this time");
        assert_eq!((t.year, t.month), (2026, 10), "{t:?}");
        assert!((19..=20).contains(&t.day), "{t:?}");
        assert!(t.hour < 24 && t.minute < 60 && t.weekday < 7, "{t:?}");
        // 1970-01-01 00:00 UTC: Thursday the 1st, or still Wednesday the 31st west of Greenwich.
        let epoch = local_time(0).expect("the system knows the epoch");
        assert!(matches!((epoch.day, epoch.weekday), (1, 4) | (31, 3)), "{epoch:?}");
    }

    #[test]
    fn file_lock_waits_for_the_first_holder() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks").join("one.lock");
        let first = FileLock::take(&path).unwrap();
        let got = Arc::new(AtomicBool::new(false));
        let second = std::thread::spawn({
            let (path, got) = (path.clone(), got.clone());
            move || {
                let lock = FileLock::take(&path).unwrap();
                got.store(true, Ordering::SeqCst);
                drop(lock);
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(!got.load(Ordering::SeqCst), "the second lock did not wait");
        drop(first);
        second.join().unwrap();
        assert!(got.load(Ordering::SeqCst));
    }

    #[test]
    fn random_bytes_differ_and_are_not_zero() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        random_bytes(&mut a);
        random_bytes(&mut b);
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
        assert_ne!(b, [0u8; 32]);
    }

    // ----- control channel (S05) -----

    /// A channel for one test alone: a socket in a new folder, or a pipe with a new name.
    fn test_channel() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mut id = [0u8; 8];
        random_bytes(&mut id);
        let id: String = id.iter().map(|b| format!("{b:02x}")).collect();
        let path = if cfg!(windows) { std::path::PathBuf::from(format!(r"\\.\pipe\glasscode-test-{id}")) } else { dir.path().join("control.sock") };
        (dir, path)
    }

    #[test]
    fn connect_without_a_listener_is_not_running() {
        let (dir, path) = test_channel();
        let Err(error) = control::connect(&path) else { panic!("connected to nothing") };
        assert!(!control::is_denied(&error), "{error}");
        // A path that is not a pipe on Windows (a socket path from the Mac, say) is no app either.
        let Err(error) = control::connect(&dir.path().join("nobody.sock")) else { panic!("connected to nothing") };
        assert!(!control::is_denied(&error), "{error}");
    }

    #[test]
    fn control_carries_a_request_line_and_two_replies() {
        use std::io::{BufRead as _, BufReader, Write as _};
        let (_dir, path) = test_channel();
        let listener = control::listen(&path).unwrap();
        let app = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let peer = stream.peer_pid();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut out = stream;
            writeln!(out, "waiting").unwrap();
            writeln!(out, "done {}", line.trim()).unwrap();
            peer
        });
        let mut stream = control::connect(&path).unwrap();
        assert_eq!(stream.peer_pid(), Some(std::process::id()));
        writeln!(stream, "board").unwrap();
        let lines: Vec<String> = BufReader::new(stream).lines().map(Result::unwrap).collect();
        assert_eq!(lines, ["waiting", "done board"]);
        assert_eq!(app.join().unwrap(), Some(std::process::id()));
    }

    #[test]
    fn control_read_times_out_when_the_other_end_is_silent() {
        use std::time::{Duration, Instant};
        let (_dir, path) = test_channel();
        let listener = control::listen(&path).unwrap();
        let mut stream = control::connect(&path).unwrap();
        let _silent = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let started = Instant::now();
        let error = std::io::Read::read(&mut stream, &mut [0u8; 8]).unwrap_err();
        assert!(matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn control_read_with_a_time_limit_gets_the_data_and_the_end() {
        use std::io::{Read as _, Write as _};
        use std::time::Duration;
        let (_dir, path) = test_channel();
        let listener = control::listen(&path).unwrap();
        let stream = control::connect(&path).unwrap();
        let mut app = listener.accept().unwrap();
        // The time limit is on the clone that reads, as the app sets it before it clones.
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = stream.try_clone().unwrap();
        app.write_all(b"done\n").unwrap();
        drop(app);
        let mut text = String::new();
        reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "done\n");
    }

    #[cfg(windows)]
    #[test]
    fn control_pipe_serves_the_next_command_while_one_is_open() {
        use std::io::{Read as _, Write as _};
        let (_dir, path) = test_channel();
        let listener = control::listen(&path).unwrap();
        let mut first = control::connect(&path).unwrap();
        let mut first_app = listener.accept().unwrap();
        // The first command is still open: the second gets an instance of its own.
        let mut second = control::connect(&path).unwrap();
        let mut second_app = listener.accept().unwrap();
        first.write_all(b"1").unwrap();
        second.write_all(b"2").unwrap();
        let (mut a, mut b) = ([0u8; 1], [0u8; 1]);
        first_app.read_exact(&mut a).unwrap();
        second_app.read_exact(&mut b).unwrap();
        assert_eq!((a, b), (*b"1", *b"2"));
    }

    #[cfg(windows)]
    #[test]
    fn control_pipe_has_one_listener() {
        let (_dir, path) = test_channel();
        let _app = control::listen(&path).unwrap();
        assert!(control::listen(&path).is_err(), "a second app took the same pipe");
    }

    #[cfg(windows)]
    #[test]
    fn control_pipe_is_named_for_the_user_and_the_folder() {
        let one = control::pipe_for("S-1-5-21-1", std::path::Path::new(r"C:\Users\alex\.glasscode"));
        assert!(one.to_string_lossy().starts_with(r"\\.\pipe\glasscode-control-"), "{}", one.display());
        assert_eq!(one, control::pipe_for("S-1-5-21-1", std::path::Path::new(r"C:\Users\alex\.glasscode")));
        assert_ne!(one, control::pipe_for("S-1-5-21-2", std::path::Path::new(r"C:\Users\alex\.glasscode")));
        assert_ne!(one, control::pipe_for("S-1-5-21-1", std::path::Path::new(r"C:\Temp\test-home")));
        // The same folder as Git Bash or a person may spell it.
        assert_eq!(one, control::pipe_for("S-1-5-21-1", std::path::Path::new("C:/Users/alex/.glasscode")));
        assert_eq!(one, control::pipe_for("S-1-5-21-1", std::path::Path::new(r"c:\users\Alex\.glasscode\")));
    }

    // ----- programs and processes (S03) -----

    #[test]
    fn find_program_looks_in_each_folder_of_a_path() {
        let (empty, dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let path = std::env::join_paths([empty.path(), dir.path()]).unwrap();
        std::fs::write(dir.path().join("tool"), "#!/bin/sh\n").unwrap();
        #[cfg(windows)]
        {
            // npm puts a shell script `tool` next to `tool.cmd`; only the second one starts here.
            std::fs::write(dir.path().join("tool.cmd"), "@echo off\r\n").unwrap();
            assert_eq!(find_program("tool", &path), Some(dir.path().join("tool.cmd")));
            assert_eq!(find_program("tool.cmd", &path), Some(dir.path().join("tool.cmd")));
        }
        #[cfg(unix)]
        assert_eq!(find_program("tool", &path), Some(dir.path().join("tool")));
    }

    #[test]
    fn find_program_finds_nothing_for_a_name_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_program("glasscode-no-such-tool", dir.path()), None);
        assert_eq!(find_program("glasscode-no-such-tool", std::env::var_os("PATH").unwrap_or_default()), None);
    }

    #[cfg(windows)]
    #[test]
    fn find_program_never_looks_in_the_current_folder() {
        // Cargo runs the tests in the crate folder, which has Cargo.toml and src\lib.rs.
        assert!(std::path::Path::new("Cargo.toml").is_file() && std::path::Path::new(r"src\lib.rs").is_file());
        for path in ["", ";", ".", r".\", "src\\.."] {
            assert_eq!(find_program("Cargo.toml", path), None, "PATH {path:?}");
        }
        assert_eq!(find_program("lib.rs", "src"), None);
    }

    #[test]
    fn processes_has_this_test_and_its_parent() {
        let list = processes();
        let me = list.iter().find(|p| p.pid == std::process::id()).expect("this test is in the process list");
        assert_ne!(me.parent, 0, "{me:?}");
        assert!(!me.name.is_empty(), "{me:?}");
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(me.dir.as_deref().map(|d| d.canonicalize().unwrap()), Some(here), "{me:?}");
    }

    #[test]
    fn command_starts_git() {
        let status = command("git").arg("--version").stdout(std::process::Stdio::null()).status().expect("git starts");
        assert!(status.success());
    }

    #[cfg(windows)]
    #[test]
    fn command_starts_a_cmd_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.cmd"), "@exit /b 3\r\n").unwrap();
        let status = command(dir.path().join("tool")).status().expect("tool.cmd starts");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn the_default_shell_runs_one_command() {
        let shell = default_shell();
        assert!(!shell.program.is_empty() && !shell.session_args.is_empty(), "{shell:?}");
        let status = command(&shell.program).args(&shell.command_args).arg("exit 3").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).status().expect("the shell starts");
        assert_eq!(status.code(), Some(3), "{shell:?}");
    }

    #[cfg(windows)]
    #[test]
    fn the_default_shell_is_a_full_path_on_windows() {
        let program = std::path::PathBuf::from(default_shell().program);
        assert!(program.is_absolute() && program.is_file(), "{program:?}");
    }

    #[test]
    fn computer_name_is_never_empty() {
        assert!(!computer_name().trim().is_empty());
    }
}
