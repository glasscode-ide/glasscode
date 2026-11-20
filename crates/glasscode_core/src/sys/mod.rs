//! The operating system under Glasscode. The rest of the core calls these and keeps no
//! platform conditions of its own; `unix.rs` and `windows.rs` give the same names.

pub mod power;
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

// ----- resources and ports (U08) ---------------------------------------------------------------

/// What one running program uses (`process_use`).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessUse {
    pub pid: u32,
    /// 0 when it has none, when the system does not say, when its parent is not in the list (it
    /// ended), or when the number is now another program's: one that started after this one
    /// cannot be its parent.
    pub parent: u32,
    pub name: String,
    /// The processor since the call before, in percent of one core: 200 is two cores in full use.
    pub cpu_percent: f32,
    /// The memory it has in use (the working set on Windows, the resident size on Unix).
    pub memory_bytes: u64,
}

/// A program as one read of the process list gives it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sampled {
    pid: u32,
    parent: u32,
    name: String,
    /// Seconds since 1970; 0 when the system does not say (a program Windows does not let us open).
    started: u64,
    /// The processor time it used since it started, in milliseconds.
    cpu_ms: u64,
    memory_bytes: u64,
}

/// One read of the process list; with `usage`, the processor time and the memory of each too.
fn sample(usage: bool) -> Vec<Sampled> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    // Threads are not programs (Linux lists them as tasks).
    let kind = ProcessRefreshKind::nothing().without_tasks();
    let kind = if usage { kind.with_cpu().with_memory() } else { kind };
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    system
        .processes()
        .values()
        .map(|p| Sampled { pid: p.pid().as_u32(), parent: p.parent().map_or(0, |parent| parent.as_u32()), name: p.name().to_string_lossy().into_owned(), started: p.start_time(), cpu_ms: p.accumulated_cpu_time(), memory_bytes: p.memory() })
        .collect()
}

/// The processor time of each program at one moment, which the next `process_use` counts from.
/// Plain numbers: a kept `sysinfo::System` would hold a handle on each program of Windows.
struct CpuBaseline {
    at: std::time::Instant,
    /// The same moment in seconds since 1970, to tell which programs started after it.
    wall: u64,
    /// The processor time (ms) of each program, by its number and its start time.
    used: std::collections::HashMap<(u32, u64), u64>,
}

impl CpuBaseline {
    fn new(at: std::time::Instant, wall: u64, rows: &[Sampled]) -> CpuBaseline {
        CpuBaseline { at, wall, used: rows.iter().map(|p| ((p.pid, p.started), p.cpu_ms)).collect() }
    }
}

/// The last read of `process_use`, in this process.
static CPU_BASELINE: std::sync::Mutex<Option<CpuBaseline>> = std::sync::Mutex::new(None);

/// A baseline older than this is read again: the average over a longer time says little about now.
const CPU_BASELINE_AGE: std::time::Duration = std::time::Duration::from_secs(10);

/// The time between the two reads of a call that has no baseline.
const CPU_FIRST_WINDOW: std::time::Duration = std::time::Duration::from_millis(250);

/// One read of the process list with its time, for `process_use`.
fn timed_sample() -> (std::time::Instant, u64, Vec<Sampled>) {
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    (std::time::Instant::now(), wall, sample(true))
}

/// Every running program with its processor and memory use. The processor use is the average
/// since the call before, when that was at most 10 seconds ago (as for a refresh each 2 seconds).
/// A call without one (the first) reads the list two times, 0.25 seconds apart, and gives the
/// use between the two reads. A call can take that long, so make it off the UI thread.
pub fn process_use() -> Vec<ProcessUse> {
    let mut baseline = CPU_BASELINE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = match baseline.take().filter(|b| b.at.elapsed() <= CPU_BASELINE_AGE) {
        Some(before) => before,
        None => {
            let (at, wall, rows) = timed_sample();
            std::thread::sleep(CPU_FIRST_WINDOW);
            CpuBaseline::new(at, wall, &rows)
        }
    };
    let (at, wall, rows) = timed_sample();
    let list = use_since(&before, &rows, at.saturating_duration_since(before.at));
    *baseline = Some(CpuBaseline::new(at, wall, &rows));
    list
}

/// The use of each program of `now`, `elapsed` after `before`. A program that `before` does not
/// have counts all its processor time when it started after `before` (all of it is in the
/// window), and none when it is older: the system did not let `before` read it, and its time
/// since it started is not the time of the window.
fn use_since(before: &CpuBaseline, now: &[Sampled], elapsed: std::time::Duration) -> Vec<ProcessUse> {
    let window_ms = elapsed.as_secs_f64() * 1000.0;
    let by_pid: std::collections::HashMap<u32, &Sampled> = now.iter().map(|p| (p.pid, p)).collect();
    now.iter()
        .map(|p| {
            let used = match before.used.get(&(p.pid, p.started)) {
                Some(&then) => p.cpu_ms.saturating_sub(then),
                // Whole seconds: only a start in a later second than `before` is surely after it.
                None if p.started > before.wall => p.cpu_ms,
                None => 0,
            };
            let cpu_percent = if window_ms > 0.0 { (used as f64 * 100.0 / window_ms) as f32 } else { 0.0 };
            let parent = by_pid.get(&p.parent).filter(|parent| may_be_parent(parent, p, true)).map_or(0, |parent| parent.pid);
            ProcessUse { pid: p.pid, parent, name: p.name.clone(), cpu_percent, memory_bytes: p.memory_bytes }
        })
        .collect()
}

/// Whether `parent` can be the parent of `child`. Not when the child names no parent (0) or
/// itself (process 0 of Windows does), and not when the child started before it: the real
/// parent ended, and a new program got its number. `unknown` tells what a start time that the
/// system does not give (0) counts as.
fn may_be_parent(parent: &Sampled, child: &Sampled, unknown: bool) -> bool {
    if child.parent == 0 || child.parent != parent.pid || child.pid == parent.pid {
        return false;
    }
    if parent.started == 0 || child.started == 0 {
        return unknown;
    }
    child.started >= parent.started
}

/// A program that waits for connections on a TCP port of this computer (`listeners`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    pub pid: u32,
    /// Its name; empty when the process list does not have it.
    pub process: String,
}

/// The TCP ports that programs listen on (IPv4 and IPv6), one entry for each port and program,
/// sorted by port. Empty when the system does not say (the reason goes to the log). On macOS
/// and Linux it asks `lsof`, which shows the programs of other users only to an administrator.
pub fn listeners() -> Vec<Listener> {
    let rows = match listener_rows() {
        Ok(rows) => rows,
        Err(error) => {
            log::warn!("could not list the ports that programs listen on: {error}");
            return Vec::new();
        }
    };
    let names = sample(false).into_iter().map(|p| (p.pid, p.name)).collect();
    unique_listeners(rows, &names)
}

/// One entry for each port and program, sorted by port and then program, with the name from
/// `names` (one read of the process list), else the one the table gave, else none.
fn unique_listeners(rows: Vec<Listener>, names: &std::collections::HashMap<u32, String>) -> Vec<Listener> {
    let mut list: Vec<Listener> = Vec::new();
    for row in rows {
        let name = names.get(&row.pid).filter(|name| !name.is_empty()).cloned().unwrap_or(row.process);
        match list.iter_mut().find(|l| l.port == row.port && l.pid == row.pid) {
            Some(known) if known.process.is_empty() => known.process = name,
            Some(_) => {}
            None => list.push(Listener { port: row.port, pid: row.pid, process: name }),
        }
    }
    list.sort_by_key(|l| (l.port, l.pid));
    list
}

/// The listening ports in the output of `lsof -F pcn`: a `p` line starts each program, a `c`
/// line has its name and an `n` line each address (`*:3000`, `127.0.0.1:8080`, `[::1]:5173`).
/// Run on Windows too, by the tests.
#[cfg(any(unix, test))]
fn parse_lsof_listeners(text: &str) -> Vec<Listener> {
    let (mut pid, mut name) = (0u32, String::new());
    let mut rows = Vec::new();
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => (pid, name) = (value.parse().unwrap_or(0), String::new()),
            "c" => name = value.to_string(),
            // The port is after the last colon; an address that a connection has (`->`) is not a listener.
            "n" if pid != 0 && !value.contains("->") => {
                if let Some(port) = value.rsplit_once(':').and_then(|(_, port)| port.parse::<u16>().ok()) {
                    rows.push(Listener { port, pid, process: name.clone() });
                }
            }
            _ => {}
        }
    }
    rows
}

/// The processes that `end_tree` refuses before it looks at anything: 0 (no process, or the
/// idle process of Windows), 4 (the System process of Windows) and Glasscode itself; on Unix also
/// 1 (launchd, init) and a number that is not a positive `pid_t` (`kill` would reach a group).
fn protected(pid: u32, me: u32) -> bool {
    pid == 0 || pid == 4 || pid == me || (cfg!(unix) && (pid == 1 || i32::try_from(pid).is_err()))
}

fn refused(pid: u32, why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("process {pid} was not ended: {why}"))
}

/// End the program `pid` and each program under it, the children first, at once and without a
/// chance to save (TerminateProcess on Windows, SIGKILL on Unix). It refuses `protected`
/// numbers and any program that Glasscode runs under, and then ends nothing. The tree comes from
/// one read of the process list: a program that ends meanwhile is not an error, and a number
/// that a new program got meanwhile is left alone.
pub fn end_tree(pid: u32) -> std::io::Result<()> {
    let me = std::process::id();
    if protected(pid, me) {
        return Err(refused(pid, "the system or Glasscode itself"));
    }
    let snapshot = sample(false);
    let order = end_order(pid, me, &snapshot)?;
    let mut failed: Vec<(u32, std::io::Error)> = Vec::new();
    for p in &order {
        if let Err(error) = end_process(p.pid, p.started) {
            failed.push((p.pid, error));
        }
    }
    match failed.first() {
        None => Ok(()),
        Some((first, error)) => Err(std::io::Error::new(error.kind(), format!("{} of the {} processes of {pid} did not end; process {first}: {error}", failed.len(), order.len()))),
    }
}

/// The programs that `end_tree(pid)` ends, each child before its parent, from one read of the
/// process list; or why it ends none.
fn end_order(pid: u32, me: u32, snapshot: &[Sampled]) -> std::io::Result<Vec<&Sampled>> {
    if protected(pid, me) {
        return Err(refused(pid, "the system or Glasscode itself"));
    }
    if !snapshot.iter().any(|p| p.pid == me) {
        return Err(refused(pid, "Glasscode is not in the process list, so it cannot tell what Glasscode runs under"));
    }
    if ancestors(me, snapshot).contains(&pid) {
        return Err(refused(pid, "Glasscode runs under it"));
    }
    let order = descendants(pid, snapshot);
    if order.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("process {pid} is not running")));
    }
    // Not reachable after the checks above; a second guard, as these end programs.
    if let Some(p) = order.iter().find(|p| protected(p.pid, me)) {
        return Err(refused(pid, &format!("process {} is under it", p.pid)));
    }
    Ok(order)
}

/// The programs above `pid`: its parent, the parent's parent, and on. A start time that the
/// system does not give counts as a parent: here more is safer.
fn ancestors(pid: u32, snapshot: &[Sampled]) -> Vec<u32> {
    let by_pid: std::collections::HashMap<u32, &Sampled> = snapshot.iter().map(|p| (p.pid, p)).collect();
    let mut seen = std::collections::HashSet::from([pid]);
    let mut list = Vec::new();
    let mut child = by_pid.get(&pid).copied();
    while let Some(p) = child {
        let Some(parent) = by_pid.get(&p.parent).copied().filter(|parent| may_be_parent(parent, p, true)) else { break };
        if !seen.insert(parent.pid) {
            break;
        }
        list.push(parent.pid);
        child = Some(parent);
    }
    list
}

/// `pid` and each program under it, each child before its parent; empty when `pid` is not in
/// the list. A start time that the system does not give does not count as a parent: here less
/// is safer.
fn descendants(pid: u32, snapshot: &[Sampled]) -> Vec<&Sampled> {
    let mut children: std::collections::HashMap<u32, Vec<&Sampled>> = std::collections::HashMap::new();
    for p in snapshot {
        children.entry(p.parent).or_default().push(p);
    }
    let Some(root) = snapshot.iter().find(|p| p.pid == pid) else { return Vec::new() };
    let mut seen = std::collections::HashSet::from([pid]);
    let mut order = Vec::new();
    // Depth first; a program goes into the order once all under it are in.
    let mut stack = vec![(root, false)];
    while let Some((p, opened)) = stack.pop() {
        if opened {
            order.push(p);
            continue;
        }
        stack.push((p, true));
        for &child in children.get(&p.pid).into_iter().flatten() {
            if may_be_parent(p, child, false) && seen.insert(child.pid) {
                stack.push((child, false));
            }
        }
    }
    order
}

// ----- files, links and sounds (S10) -----------------------------------------------------------

/// The sound of a notification (`play_sound`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// A session waits for the person: a question, or a request to allow.
    NeedsYou,
    /// A session finished its turn.
    Done,
}

/// Open a folder in the file manager, or a file in the program the system has for its type. On
/// Windows a file that this program would run (a program, a script, a shortcut) is only shown
/// in File Explorer.
pub fn open_path(path: &std::path::Path) -> std::io::Result<()> {
    open_path_command(path).spawn().map(drop)
}

/// Show a file in the file manager, selected in its folder.
pub fn reveal_path(path: &std::path::Path) -> std::io::Result<()> {
    reveal_path_command(path).spawn().map(drop)
}

/// Open a link in the program the system has for it. On macOS `open` gets every address as it is,
/// as before the port: a file path, `file:`, a `mailto:` with a subject and the schemes of other
/// apps open too. On Windows only http, https and `mailto:` open (`openable_link`): links come
/// from agents and from other people's text, and the shell could start a program or a script for
/// anything else. A local path goes to `open_path`, which on Windows never runs one.
pub fn open_url(url: &str) -> std::io::Result<()> {
    open_link(url)
}

/// The address that `open_url` opens on Windows for `url`, or why it refuses it. A `mailto:` link
/// keeps only its recipients: what follows them (`?subject=`, `?attach=`…) could fill in or attach
/// more than the person sees in the link. macOS does not check links.
#[cfg(any(windows, test))]
fn openable_link(url: &str) -> std::io::Result<String> {
    let refuse = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("the link {url:?} does not open: {why}"));
    // A program that reads its arguments would take `-…` for an option.
    if url.is_empty() || url.starts_with('-') || url.starts_with(char::is_whitespace) || url.chars().any(char::is_control) {
        return Err(refuse("it is empty, or starts with a space or `-`, or has a control character"));
    }
    let (scheme, rest) = url.split_once(':').unwrap_or_default();
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        if !rest.starts_with("//") || rest.len() == 2 {
            return Err(refuse("it is not a web address"));
        }
        return Ok(url.to_string());
    }
    if scheme.eq_ignore_ascii_case("mailto") {
        let recipients = rest.split(['?', '#']).next().unwrap_or_default();
        let address = |r: &str| r.split_once('@').is_some_and(|(name, domain)| !name.is_empty() && !domain.is_empty()) && !r.contains(char::is_whitespace);
        if recipients.is_empty() || !recipients.split(',').all(address) {
            return Err(refuse("it names no e-mail address"));
        }
        return Ok(format!("{scheme}:{recipients}"));
    }
    Err(refuse("only http, https and mailto links open"))
}

/// The app's (and glasscoded's) own git calls pass `profile::GIT_OVERRIDES` once this is on (the
/// v2 engine, M22, trust-m7): a session can write its worktree's git configuration, and git must
/// not run the hooks, fsmonitor or ssh command it names for Glasscode.
static SAFE_GIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn the overrides of Glasscode's own git calls on or off.
pub fn use_safe_git(on: bool) {
    SAFE_GIT.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn safe_git(program: &std::ffi::OsStr, command: &mut std::process::Command) {
    let name = std::path::Path::new(program).file_stem().map(|s| s.to_string_lossy().to_lowercase());
    if SAFE_GIT.load(std::sync::atomic::Ordering::Relaxed) && name.as_deref() == Some("git") {
        command.args(crate::profile::GIT_OVERRIDES);
    }
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

    // ----- resources and ports (U08) -----

    fn row(pid: u32, parent: u32, started: u64) -> Sampled {
        Sampled { pid, parent, name: format!("program {pid}"), started, ..Sampled::default() }
    }

    fn order_of(pid: u32, me: u32, snapshot: &[Sampled]) -> Vec<u32> {
        end_order(pid, me, snapshot).unwrap().iter().map(|p| p.pid).collect()
    }

    /// Glasscode (10) under a shell (5) under the first program (1); a dev server (20) that
    /// Glasscode started, with children (21, 22) and a grandchild (23).
    fn made_up_list() -> Vec<Sampled> {
        vec![
            row(1, 0, 100),
            row(5, 1, 200),
            row(10, 5, 300),
            row(20, 10, 400),
            row(21, 20, 410),
            row(22, 20, 420),
            row(23, 21, 430),
            // It names 20 as its parent, but started before 20: its parent ended, and the dev
            // server got the old number. Its child is not under 20 either.
            row(30, 20, 350),
            row(31, 30, 360),
            // Windows does not tell when it started: it is not counted under 20.
            row(40, 20, 0),
        ]
    }

    #[test]
    fn end_order_takes_the_children_first_and_leaves_a_reused_number_alone() {
        let list = made_up_list();
        let order = order_of(20, 10, &list);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, [20, 21, 22, 23], "{order:?}");
        let at = |pid: u32| order.iter().position(|&p| p == pid).unwrap();
        assert!(at(23) < at(21) && at(21) < at(20) && at(22) < at(20), "{order:?}");
        assert_eq!(order_of(23, 10, &list), [23]);
        assert_eq!(order_of(30, 10, &list), [31, 30]);
        assert_eq!(end_order(99, 10, &list).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn end_order_refuses_the_system_glasscode_and_what_it_runs_under() {
        let list = made_up_list();
        // 0, 4 and Glasscode itself; then the shell and the first program, which it runs under.
        for pid in [0, 4, 10, 5, 1] {
            let error = end_order(pid, 10, &list).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{pid}: {error}");
        }
        // A parent whose start time is not known still counts above Glasscode.
        let list = vec![row(7, 0, 0), row(10, 7, 300), row(20, 10, 400)];
        assert_eq!(end_order(7, 10, &list).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(order_of(20, 10, &list), [20]);
        // Without Glasscode in the list, what it runs under is not known: nothing ends.
        assert_eq!(end_order(20, 11, &list).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_parent_loop_in_the_process_list_does_not_hang() {
        // Read while programs started and ended: 60 and 61 name each other, 62 itself.
        let list = vec![row(10, 0, 100), row(60, 61, 500), row(61, 60, 500), row(62, 62, 500)];
        assert_eq!(order_of(60, 10, &list), [61, 60]);
        assert_eq!(order_of(62, 10, &list), [62]);
        assert!(ancestors(62, &list).is_empty());
        // Glasscode in such a loop: the other one is above it, and refused.
        let list = vec![row(70, 71, 500), row(71, 70, 500)];
        assert_eq!(end_order(71, 70, &list).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn processor_use_counts_only_the_time_between_the_reads() {
        let before = CpuBaseline { at: std::time::Instant::now(), wall: 1_000, used: std::collections::HashMap::from([((1, 900), 5_000), ((2, 950), 100), ((3, 990), 0)]) };
        let now = [
            // 1 s of processor in 2 s.
            Sampled { cpu_ms: 6_000, ..row(1, 0, 900) },
            // 4 s in 2 s: two cores.
            Sampled { cpu_ms: 4_100, ..row(2, 1, 950) },
            // A new program with the number 3, started after `before`: all its time is new.
            Sampled { cpu_ms: 300, ..row(3, 1, 1_001) },
            // Not in `before` and older than it: its time since it started is not the window's.
            Sampled { cpu_ms: 80_000, ..row(4, 2, 999) },
            // Its parent's number is the new program's, which started after it.
            Sampled { cpu_ms: 0, memory_bytes: 4096, ..row(5, 3, 1_000) },
            // Its parent ended: no longer in the list.
            row(6, 77, 950),
        ];
        let list = use_since(&before, &now, std::time::Duration::from_secs(2));
        let got: Vec<(u32, u32, f32)> = list.iter().map(|p| (p.pid, p.parent, p.cpu_percent)).collect();
        assert_eq!(got, [(1, 0, 50.0), (2, 1, 200.0), (3, 1, 15.0), (4, 2, 0.0), (5, 0, 0.0), (6, 0, 0.0)]);
        assert_eq!((list[4].name.as_str(), list[4].memory_bytes), ("program 5", 4096));
        assert!(use_since(&before, &now, std::time::Duration::ZERO).iter().all(|p| p.cpu_percent == 0.0));
    }

    #[test]
    fn process_use_has_this_test_with_its_memory_and_processor() {
        let me = std::process::id();
        let first = process_use();
        let mine = first.iter().find(|p| p.pid == me).expect("this test is in the list");
        assert!(mine.memory_bytes > 0 && !mine.name.is_empty(), "{mine:?}");
        // The first call reads two times: nothing is far above what the cores can do, as it would
        // be with the time since each program started.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        for p in &first {
            assert!(p.cpu_percent.is_finite() && (0.0..=cores * 200.0).contains(&p.cpu_percent), "process {}: {}", p.pid, p.cpu_percent);
        }
        // Busy on one core for 0.6 seconds; the next call sees it.
        let busy = std::time::Instant::now();
        let mut n = 0u64;
        while busy.elapsed() < std::time::Duration::from_millis(600) {
            n = std::hint::black_box(n.wrapping_add(1));
        }
        let second = process_use();
        let mine = second.iter().find(|p| p.pid == me).expect("this test is in the list");
        assert!(mine.cpu_percent >= 25.0, "{mine:?}");
    }

    #[test]
    fn listeners_have_one_entry_for_each_port_and_program() {
        let found = |port: u16, pid: u32, process: &str| Listener { port, pid, process: process.to_string() };
        let rows = vec![
            found(5173, 7, ""),
            found(3000, 7, ""),
            // The same port on IPv6.
            found(3000, 7, ""),
            // A second program on the same port (SO_REUSEPORT).
            found(3000, 8, "node"),
            // A program the process list does not have: the table's name stays.
            found(22, 9, "sshd"),
        ];
        let names = std::collections::HashMap::from([(7, "node.exe".to_string()), (8, String::new())]);
        assert_eq!(unique_listeners(rows, &names), [found(22, 9, "sshd"), found(3000, 7, "node.exe"), found(3000, 8, "node"), found(5173, 7, "node.exe")]);
    }

    #[test]
    fn lsof_output_gives_the_listening_ports() {
        // `lsof -iTCP -sTCP:LISTEN -P -n -Fpcn`, as macOS prints it.
        let text = "p501\ncnode\nf23\nn*:3000\nf24\nn[::1]:3000\np777\ncMy App\nf5\nn127.0.0.1:8000\nf6\nn[fe80::1%lo0]:9000\nf7\nnnot-a-port\n";
        let found = |port: u16, pid: u32, process: &str| Listener { port, pid, process: process.to_string() };
        assert_eq!(parse_lsof_listeners(text), [found(3000, 501, "node"), found(3000, 501, "node"), found(8000, 777, "My App"), found(9000, 777, "My App")]);
        assert!(parse_lsof_listeners("").is_empty());
    }

    #[test]
    fn listeners_has_a_port_this_test_opens() {
        #[cfg(unix)]
        if !lsof().is_file() {
            // Past the test harness, which keeps back what a test that passes prints.
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "skipped listeners_has_a_port_this_test_opens: `listeners` asks lsof, which this computer does not have");
            return;
        }
        let me = std::process::id();
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = v4.local_addr().unwrap().port();
        // The same port on IPv6 too, where this computer has IPv6: still one entry.
        let v6 = std::net::TcpListener::bind(("::1", port)).ok();
        // A port on IPv6 alone (the second table of Windows).
        let v6_only = std::net::TcpListener::bind("[::1]:0").ok();
        let list = listeners();
        let mine = |port: u16| list.iter().filter(|l| l.port == port && l.pid == me).collect::<Vec<_>>();
        let entries = mine(port);
        assert_eq!(entries.len(), 1, "port {port}, also on IPv6: {}", v6.is_some());
        assert!(!entries[0].process.is_empty(), "{:?}", entries[0]);
        if let Some(v6_only) = &v6_only {
            assert_eq!(mine(v6_only.local_addr().unwrap().port()).len(), 1, "a port on IPv6 alone");
        }
        assert!(list.windows(2).all(|w| (w[0].port, w[0].pid) < (w[1].port, w[1].pid)), "not sorted, or an entry twice");
    }

    /// A program that the test starts, which starts one more and waits for it: `cmd` with
    /// `ping` on Windows, `sh` with `sleep` on Unix, for at most a minute. All end when this is
    /// dropped, also after a failed assertion, each only while it is still that program.
    struct TestPrograms {
        child: std::process::Child,
        /// What the child started (on Windows also the host of its hidden console), found by the
        /// chain of parents, never by name.
        under: Vec<Sampled>,
    }

    impl TestPrograms {
        fn start() -> TestPrograms {
            use std::process::Stdio;
            #[cfg(windows)]
            let (program, args) = ("cmd", ["/d", "/c", "ping", "-n", "60", "127.0.0.1"]);
            #[cfg(unix)]
            let (program, args) = ("/bin/sh", ["-c", "sleep 60 & wait"]);
            let child = command(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("the test's program starts");
            let mut programs = TestPrograms { child, under: Vec::new() };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while programs.under.is_empty() {
                assert!(std::time::Instant::now() < deadline, "the test's program started nothing");
                std::thread::sleep(std::time::Duration::from_millis(50));
                programs.under = programs.read_under();
            }
            // The rest that it starts at once comes in a moment.
            std::thread::sleep(std::time::Duration::from_millis(500));
            for p in programs.read_under() {
                if !programs.under.contains(&p) {
                    programs.under.push(p);
                }
            }
            programs
        }

        fn read_under(&self) -> Vec<Sampled> {
            let pid = self.child.id();
            descendants(pid, &sample(false)).into_iter().filter(|p| p.pid != pid).cloned().collect()
        }

        /// How many of the programs under the child still run.
        fn running_under(&self) -> usize {
            use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
            let pids: Vec<Pid> = self.under.iter().map(|p| Pid::from_u32(p.pid)).collect();
            let mut system = System::new();
            system.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing());
            let runs = |p: &Sampled| system.process(Pid::from_u32(p.pid)).is_some_and(|q| q.start_time() == p.started && q.status() != ProcessStatus::Zombie);
            self.under.iter().filter(|p| runs(p)).count()
        }
    }

    impl Drop for TestPrograms {
        fn drop(&mut self) {
            for p in &self.under {
                let _ = end_process(p.pid, p.started);
            }
            // By its handle on Windows; on Unix not read yet, so the number is still its own.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn end_tree_refuses_the_system_and_glasscode_and_ends_nothing() {
        let me = std::process::id();
        // What this test runs under (cargo, a shell…) is refused by the plan alone: the test never
        // asks to end it for real.
        let list = sample(false);
        let parent = list.iter().find(|p| p.pid == me).expect("this test is in the process list").parent;
        assert_ne!(parent, 0);
        assert_eq!(end_order(parent, me, &list).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        // So are, on Unix, the first program and numbers that `kill` reads as a group.
        #[cfg(unix)]
        for pid in [1, u32::MAX, i32::MAX as u32 + 1] {
            assert_eq!(end_order(pid, me, &list).unwrap_err().kind(), std::io::ErrorKind::InvalidInput, "{pid}");
        }
        // The protected numbers of the slice are refused for real, before the process list is read.
        let mut programs = TestPrograms::start();
        for pid in [0, 4, me] {
            let error = end_tree(pid).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{pid}: {error}");
        }
        assert!(programs.child.try_wait().unwrap().is_none(), "the test's program ended");
        assert_eq!(programs.running_under(), programs.under.len(), "a program it started ended");
    }

    #[test]
    fn end_tree_ends_a_program_and_the_one_it_started() {
        let mut programs = TestPrograms::start();
        assert_eq!(programs.running_under(), programs.under.len());        end_tree(programs.child.id()).expect("the test's programs end");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while programs.child.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline, "the test's program still runs");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        while programs.running_under() > 0 {
            assert!(std::time::Instant::now() < deadline, "a program it started still runs");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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

    // ----- files, links and sounds (S10) -----
    // These look at the command that would start, never start it: a test opens no window and
    // plays no sound on the person's computer.

    fn args(command: &std::process::Command) -> Vec<String> {
        command.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// Links that Windows does not open (`openable_link`); macOS gives them to `open`.
    const REFUSED_ON_WINDOWS: &[&str] = &[
        "",
        "-a",
        "-a Calculator https://example.com",
        " https://example.com",
        "\thttps://example.com",
        "https://example.com/\r\nnext",
        "https://example.com/a\u{0}b",
        "https://",
        "http:example.com",
        "example.com",
        "ftp://example.com",
        "file:///etc/passwd",
        "FILE:C:/Windows/System32/calc.exe",
        "/System/Applications/Calculator.app",
        r"C:\Windows\System32\calc.exe",
        r"\\server\share\run.cmd",
        "javascript:alert('://')",
        "ms-settings:privacy",
        "search-ms:query=x",
        "vscode://file/C:/a.txt",
        "x-apple.systempreferences:com.apple.preference.security",
        "mailto:",
        "mailto:?subject=Hi",
        "mailto:alex",
        "mailto:@example.com",
        "mailto:alex@",
        "mailto:alex@example.com bob@example.com",
    ];

    /// The Windows check alone, which starts nothing, so it runs on both systems.
    #[test]
    fn only_web_and_mail_links_pass_the_windows_check() {
        for (url, opened) in [
            ("https://github.com/acme/app/pull/12", "https://github.com/acme/app/pull/12"),
            ("http://localhost:3000/a?c=d&e=f", "http://localhost:3000/a?c=d&e=f"),
            ("HTTPS://linear.app/acme/issue/ENG-123", "HTTPS://linear.app/acme/issue/ENG-123"),
            ("mailto:alex@example.com", "mailto:alex@example.com"),
            ("MAILTO:alex@example.com,sam@example.org", "MAILTO:alex@example.com,sam@example.org"),
            // Only the recipients: a subject, a body or an attachment is left out.
            ("mailto:alex@example.com?subject=Hi&attach=C:/Users/alex/secret.txt", "mailto:alex@example.com"),
            ("mailto:alex@example.com#top", "mailto:alex@example.com"),
        ] {
            assert_eq!(openable_link(url).unwrap(), opened);
        }
        for &url in REFUSED_ON_WINDOWS {
            let error = openable_link(url).expect_err(url);
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{url:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn only_web_and_mail_links_open() {
        // Refused before anything starts, so these are safe to try for real.
        for &url in REFUSED_ON_WINDOWS {
            let error = open_url(url).expect_err(url);
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{url:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_mac_gives_every_link_to_open_as_it_is() {
        // As before the port: no check, and nothing of the address left out.
        for url in ["https://github.com/acme/app/pull/12", "mailto:alex@example.com?subject=Hi&body=Hello", "file:///tmp/a%20b/c.txt", "/tmp/a b/c.png"] {
            let open = open_url_command(url);
            assert_eq!((open.get_program(), args(&open)), (std::ffi::OsStr::new("open"), vec![url.to_string()]), "{url}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_open_in_finder() {
        let path = std::path::Path::new("/tmp/a b/c.png");
        let open = open_path_command(path);
        assert_eq!((open.get_program(), args(&open)), (std::ffi::OsStr::new("open"), vec!["/tmp/a b/c.png".to_string()]));
        let reveal = reveal_path_command(path);
        assert_eq!((reveal.get_program(), args(&reveal)), (std::ffi::OsStr::new("open"), vec!["-R".to_string(), "/tmp/a b/c.png".to_string()]));
    }

    #[cfg(unix)]
    #[test]
    fn the_two_sounds_are_the_mac_ones() {
        let needs_you = sound_command(Sound::NeedsYou);
        assert_eq!(needs_you.get_program(), "/usr/bin/afplay");
        assert_eq!(args(&needs_you), ["/System/Library/Sounds/Glass.aiff"]);
        assert_eq!(args(&sound_command(Sound::Done)), ["/System/Library/Sounds/Hero.aiff"]);
    }

    #[cfg(windows)]
    #[test]
    fn files_open_in_file_explorer() {
        let open = open_path_command(std::path::Path::new(r"C:\Users\alex\my project"));
        let program = std::path::Path::new(open.get_program());
        assert!(program.file_name().is_some_and(|name| name.eq_ignore_ascii_case("explorer.exe")), "{program:?}");
        assert_eq!(args(&open), [r#""C:\Users\alex\my project""#]);
        // Explorer takes `C:/…` for a switch, and does not know the `\\?\` prefix.
        assert_eq!(args(&open_path_command(std::path::Path::new("C:/Users/alex/app/"))), [r#""C:\Users\alex\app""#]);
        let reveal = reveal_path_command(std::path::Path::new(r"\\?\C:\Users\alex\app\src/main.rs"));
        assert_eq!(args(&reveal), [r#"/select,"C:\Users\alex\app\src\main.rs""#]);
        // A comma would end the path for Explorer, which reads its switches split by commas.
        assert_eq!(args(&reveal_path_command(std::path::Path::new(r"C:\a,b\c d.png"))), [r#"/select,"C:\a,b\c d.png""#]);
        assert_eq!(args(&reveal_path_command(std::path::Path::new(r"\\?\UNC\server\share\c.png"))), [r#"/select,"\\server\share\c.png""#]);
        // A drive keeps its backslash, which quotation marks would escape; any other path loses
        // the one at its end and is always quoted, so that a space or a comma cannot split it.
        let opened = |path: &str| args(&open_path_command(std::path::Path::new(path)));
        assert_eq!(opened(r"D:\"), [r"D:\"]);
        assert_eq!(opened(r"c:\"), [r"C:\"]);
        assert_eq!(opened(r"C:\a b\"), [r#""C:\a b""#]);
        assert_eq!(opened(r"C:\a,b\"), [r#""C:\a,b""#]);
        assert_eq!(opened(r"C:\plain\"), [r#""C:\plain""#]);
        // (Shown, not opened: to open, the test would ask the network whether the share is a folder.)
        assert_eq!(args(&reveal_path_command(std::path::Path::new(r"\\server\my share\"))), [r#"/select,"\\server\my share""#]);
        assert_eq!(opened(r"\\?\C:\a b\c,d\"), [r#""C:\a b\c,d""#]);
        // A path that is not Unicode is cleaned and quoted the same way.
        use std::os::windows::ffi::OsStringExt as _;
        let mut wide: Vec<u16> = "C:/a b/".encode_utf16().collect();
        wide.push(0xD800);
        wide.extend("x.png".encode_utf16());
        let odd = std::path::PathBuf::from(std::ffi::OsString::from_wide(&wide));
        assert_eq!(args(&open_path_command(&odd)), ["\"C:\\a b\\\u{FFFD}x.png\""]);
    }

    #[cfg(windows)]
    #[test]
    fn a_file_that_would_run_is_only_shown_in_file_explorer() {
        let open = |path: &str| args(&open_path_command(std::path::Path::new(path)));
        let shown = |path: &str| vec![format!(r#"/select,"{path}""#)];
        for file in [
            r"C:\w\setup.exe",
            r"C:\w\build.CMD",
            r"C:\w\tools\run.bat",
            r"C:\w\scripts\deploy.ps1",
            r"C:\w\src\index.js",
            r"C:\w\script.vbs",
            r"C:\w\page.hta",
            r"C:\w\make.py",
            r"C:\w\install.sh",
            r"C:\w\docs.lnk",
            r"C:\w\site.url",
            r"C:\w\fix.reg",
            r"C:\w\app.msi",
        ] {
            assert_eq!(open(file), shown(file), "{file}");
        }
        // Windows drops a dot or a space at the end of the name; a colon names a stream.
        assert_eq!(open(r"C:\w\setup.exe."), shown(r"C:\w\setup.exe."));
        assert_eq!(open(r"C:\w\setup.exe "), shown(r"C:\w\setup.exe "));
        assert_eq!(open(r"C:\w\notes.txt:run.exe"), shown(r"C:\w\notes.txt:run.exe"));
        // Documents, images and files without a type open in their program.
        for file in [r"C:\w\README.md", r"C:\w\notes.txt", r"C:\w\shot.png", r"C:\w\spec.pdf", r"C:\w\LICENSE"] {
            assert_eq!(open(file), [format!(r#""{file}""#)], "{file}");
        }
        // A folder only opens in File Explorer, whatever its name.
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("tool.exe");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(args(&open_path_command(&folder)), [format!(r#""{}""#, folder.display())]);
    }

    #[cfg(windows)]
    #[test]
    fn the_two_sounds_are_two_windows_sounds() {
        // Sound events of each Windows (Control Panel > Sound > Sounds), which the person can change.
        // SAFETY: static strings that end with a 0.
        let names = unsafe { (sound_alias(Sound::NeedsYou).to_string().unwrap(), sound_alias(Sound::Done).to_string().unwrap()) };
        assert_eq!(names, ("Notification.Reminder".to_string(), "Notification.Default".to_string()));
    }

    // ----- paths and agents (S06) -----

    #[test]
    fn same_path_is_true_for_a_real_folder_and_its_canonical_form() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        assert!(same_path(dir.path(), &real) && same_path(&real, dir.path()));
        assert!(!same_path(dir.path(), &dir.path().join("other")));
    }

    #[cfg(windows)]
    #[test]
    fn same_path_ignores_case_and_the_long_path_prefix_on_windows() {
        use std::path::Path;
        assert!(same_path(Path::new(r"C:\Users\Dev\App"), Path::new(r"\\?\c:\users\dev\app\")));
        assert!(same_path(Path::new("C:/Users/Dev/App"), Path::new(r"c:\users\dev\app")));
        assert!(!same_path(Path::new(r"C:\Users\Dev\App"), Path::new(r"C:\Users\Dev\App2")));
        assert_eq!(clean_path(Path::new(r"\\?\c:\Users\Dev\App\")), Path::new(r"C:\Users\Dev\App"));
        assert_eq!(clean_path(Path::new(r"\\?\UNC\server\share\dir")), Path::new(r"\\server\share\dir"));
        assert_eq!(clean_path(Path::new(r"C:\Users\\Dev\.\App")), Path::new(r"C:\Users\Dev\App"));
        assert_eq!(clean_path(Path::new(r"c:\")), Path::new(r"C:\"));
    }

    #[cfg(windows)]
    #[test]
    fn an_npm_cmd_file_is_read_for_the_program_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("npm");
        let script = bin.join("node_modules").join("@acme").join("tool").join("cli.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "").unwrap();
        // npm's file for a script: node.exe next to it, else the one on PATH.
        let shim = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@acme\\tool\\cli.js\" %*\r\n";
        std::fs::write(bin.join("tool.cmd"), shim).unwrap();
        let nodes = dir.path().join("nodejs");
        std::fs::create_dir_all(&nodes).unwrap();
        std::fs::write(nodes.join("node.exe"), "").unwrap();
        let on_path = std::env::join_paths([&nodes]).unwrap();
        let expected = (nodes.join("node.exe"), vec![clean_path(&script).to_string_lossy().into_owned()]);
        assert_eq!(script_target(&bin.join("tool.cmd"), &on_path), Some(expected));
        std::fs::write(bin.join("node.exe"), "").unwrap();
        assert_eq!(script_target(&bin.join("tool.cmd"), &on_path).map(|(program, _)| program), Some(bin.join("node.exe")));
        // npm's file for a program.
        let program = bin.join("node_modules").join("@acme").join("tool").join("bin").join("tool.exe");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "").unwrap();
        std::fs::write(bin.join("tool2.cmd"), "@ECHO off\r\n\"%~dp0node_modules\\@acme\\tool\\bin\\tool.exe\"   %*\r\n").unwrap();
        assert_eq!(script_target(&bin.join("tool2.cmd"), &on_path), Some((clean_path(&program), Vec::new())));
        // pnpm's file, whose last line starts a bare `node`.
        let pnpm = bin.join("node_modules").join(".bin");
        std::fs::create_dir_all(&pnpm).unwrap();
        std::fs::write(pnpm.join("tool.cmd"), "@SETLOCAL\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\..\\@acme\\tool\\cli.js\" %*\r\n) ELSE (\r\n  @SET PATHEXT=%PATHEXT:;.JS;=;%\r\n  node  \"%~dp0\\..\\@acme\\tool\\cli.js\" %*\r\n)\r\n").unwrap();
        let (node, first) = script_target(&pnpm.join("tool.cmd"), &on_path).expect("pnpm's file is read");
        assert_eq!(node, nodes.join("node.exe"));
        assert_eq!(first.iter().map(|f| std::path::Path::new(f).canonicalize().unwrap()).collect::<Vec<_>>(), [script.canonicalize().unwrap()]);
        // Any other batch file, and a program that is not there, start as before.
        std::fs::write(bin.join("other.cmd"), "@echo off\r\nset X=1\r\n\"%X%\\tool.exe\" %*\r\n").unwrap();
        assert_eq!(script_target(&bin.join("other.cmd"), &on_path), None);
        std::fs::write(bin.join("gone.cmd"), "@ECHO off\r\n\"%~dp0missing.exe\" %*\r\n").unwrap();
        assert_eq!(script_target(&bin.join("gone.cmd"), &on_path), None);
        assert_eq!(script_target(&nodes.join("node.exe"), &on_path), None);
    }

    #[test]
    fn claude_shell_runs_a_command() {
        let output = claude_shell("echo shown").stdin(std::process::Stdio::null()).output().expect("the shell starts");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "shown");
    }

    // ----- agent browser (S17) -----

    /// What `spawn_detached` starts in the next test: this test program again, running only this,
    /// which writes down what it got. Without the variable (a person runs `--ignored`) it does
    /// nothing.
    #[test]
    #[ignore = "started by spawn_detached_passes_the_arguments_the_environment_and_the_folder"]
    fn detached_child_writes_down_what_it_got() {
        let Some(out) = std::env::var_os("GLASSCODE_TEST_DETACHED_OUT") else { return };
        let got = serde_json::json!({ "args": std::env::args().collect::<Vec<_>>(), "dir": std::env::current_dir().unwrap() });
        let out = std::path::PathBuf::from(out);
        // In one step, so the test never reads half of it.
        std::fs::write(out.with_extension("tmp"), got.to_string()).unwrap();
        std::fs::rename(out.with_extension("tmp"), &out).unwrap();
    }

    #[test]
    fn spawn_detached_passes_the_arguments_the_environment_and_the_folder() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got.json");
        // Words a command line has to quote; to the test program they are names of no test.
        let words = ["two words", r#"a "quoted" word"#, r"ends in \", r"C:\Program Files\", r#"{"json":"C:\\x"}"#];
        let mut child = command(std::env::current_exe().unwrap());
        child.args(["sys::tests::detached_child_writes_down_what_it_got", "--exact", "--ignored", "--test-threads=1"]).args(words);
        child.env("GLASSCODE_TEST_DETACHED_OUT", &out).current_dir(dir.path());
        let pid = spawn_detached(&mut child).unwrap();
        assert_ne!(pid, 0);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !out.exists() {
            assert!(Instant::now() < deadline, "the detached program wrote nothing");
            std::thread::sleep(Duration::from_millis(100));
        }
        let got: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        let args: Vec<&str> = got["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
        assert_eq!(args[args.len() - words.len()..], words, "{args:?}");
        assert!(same_path(std::path::Path::new(got["dir"].as_str().unwrap()), dir.path()), "{got}");
        // The program may still be ending, in the folder.
        remove_dir_all(dir.path()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn spawn_detached_starts_no_batch_file_and_no_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool.cmd"), "@exit /b 0\r\n").unwrap();
        for program in [dir.path().join("tool.cmd"), "tool.exe".into()] {
            let error = spawn_detached(&mut std::process::Command::new(&program)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{}: {error}", program.display());
        }
    }

    #[cfg(windows)]
    #[test]
    fn remove_dir_all_waits_for_a_file_that_is_still_open() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        // Open as a browser has its files: no one else may delete it meanwhile.
        let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(profile.join("Default").join("Cookies")).unwrap();
        assert!(std::fs::remove_dir_all(&profile).is_err(), "Windows removed a file that is open");
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            drop(file);
        });
        remove_dir_all(&profile).unwrap();
        assert!(!profile.exists());
        closer.join().unwrap();
        assert_eq!(remove_dir_all(&profile).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }
}
