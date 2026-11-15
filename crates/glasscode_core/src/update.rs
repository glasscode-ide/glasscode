//! Updates for Glasscode's own programs (the Mac app, the Windows app, glasscoded), published to a releases endpoint.
//!
//! A release is a manifest (product, build, files with their SHA-256) signed with the
//! Glasscode release key, which stays on the publisher's Mac. Every program has the public
//! key built in and installs only what that key signed, so neither the server nor the
//! network can slip in a binary. The server only stores and serves releases (to signed-in
//! devices).
//!
//! Builds are numbered at build time (`GLASSCODE_BUILD`, e.g. 202610161800): a program
//! takes a release only when its build is higher than its own. Development builds (no
//! number) do not update themselves.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Release keys this build trusts (raw Ed25519 public keys, base64). A new key ships in a
/// build signed by the old one, then releases move to it.
pub const RELEASE_KEYS: &[&str] = &[RELEASE_KEY_1];
const RELEASE_KEY_1: &str = "GeiSPiM4Gi92DeVG2iZnYHaQFmWYPShBLKZjLXFMQcs=";

/// What the signature covers comes first, so these bytes mean nothing anywhere else.
const SIGNED_PREFIX: &[u8] = b"glasscode-release-v1\n";

/// This build's number (0 for a development build).
pub const BUILD: u64 = parse_build(option_env!("GLASSCODE_BUILD"));
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const fn parse_build(text: Option<&str>) -> u64 {
    let Some(text) = text else { return 0 };
    let bytes = text.as_bytes();
    let mut n: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < b'0' || b > b'9' {
            return 0;
        }
        n = n * 10 + (b - b'0') as u64;
        i += 1;
    }
    n
}

/// "0.1.0 (202610161800)", or "0.1.0 (development build)".
pub fn describe(version: &str, build: u64) -> String {
    if build == 0 { format!("{version} (development build)") } else { format!("{version} ({build})") }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReleaseFile {
    pub name: String,
    /// `macos`, `linux` or `windows`.
    pub os: String,
    /// `aarch64` or `x86_64`.
    pub arch: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// `glasscode-mac`, `glasscode-windows` or `glasscoded`.
    pub product: String,
    pub version: String,
    pub build: u64,
    pub channel: String,
    #[serde(default)]
    pub notes: String,
    pub published_at: u64,
    pub files: Vec<ReleaseFile>,
}

impl Manifest {
    /// The file for this machine.
    pub fn file_for(&self, os: &str, arch: &str) -> Option<&ReleaseFile> {
        self.files.iter().find(|f| f.os == os && f.arch == arch)
    }
}

/// A manifest exactly as signed, with its signature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedRelease {
    /// The manifest's JSON text (the signed bytes).
    pub manifest: String,
    /// Ed25519 signature, base64.
    pub signature: String,
}

/// Check the signature against the built-in keys, then read the manifest.
pub fn verify(release: &SignedRelease, keys: &[&str]) -> anyhow::Result<Manifest> {
    let signature = B64.decode(release.signature.trim()).context("the release signature is not base64")?;
    let mut message = SIGNED_PREFIX.to_vec();
    message.extend_from_slice(release.manifest.as_bytes());
    let trusted = keys.iter().filter_map(|k| B64.decode(k).ok()).any(|key| {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key).verify(&message, &signature).is_ok()
    });
    if !trusted {
        bail!("the release is not signed with the Glasscode release key; not installing it");
    }
    Ok(serde_json::from_str(&release.manifest).context("the signed release manifest is not valid")?)
}

/// Sign a manifest (publishing, on the Mac that holds the key: a PKCS#8 Ed25519 key).
pub fn sign(manifest: &Manifest, pkcs8: &[u8]) -> anyhow::Result<SignedRelease> {
    let key = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| anyhow::anyhow!("the release key file is damaged"))?;
    let text = serde_json::to_string(manifest)?;
    let mut message = SIGNED_PREFIX.to_vec();
    message.extend_from_slice(text.as_bytes());
    Ok(SignedRelease { manifest: text, signature: B64.encode(key.sign(&message).as_ref()) })
}

/// A new release key: (PKCS#8 for the key file, public key for RELEASE_KEYS).
pub fn generate_key() -> anyhow::Result<(Vec<u8>, String)> {
    use ring::signature::KeyPair as _;
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| anyhow::anyhow!("could not make a key"))?;
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| anyhow::anyhow!("could not read the new key"))?;
    Ok((pkcs8.as_ref().to_vec(), B64.encode(pair.public_key().as_ref())))
}

pub fn sha256_file(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut file = std::fs::File::open(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex(&hasher.finalize()), size))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The newest release of `product` on `channel` that this build should take, checked.
///
/// TODO(glasscode): the standalone fork has no release server yet, so this reports "no
/// update" without any network call. Wire it to a self-hosted releases endpoint to restore
/// auto-update.
pub fn newer(product: &str, channel: &str, current: u64) -> anyhow::Result<Option<Manifest>> {
    let _ = (product, channel, current);
    Ok(None)
}

/// Where updates are kept while they wait to be installed.
pub fn updates_dir() -> PathBuf {
    crate::paths::root().join("updates")
}

/// This machine as release files name it.
pub fn this_os() -> &'static str {
    if cfg!(target_os = "macos") { "macos" } else if cfg!(windows) { "windows" } else { "linux" }
}

pub fn this_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" }
}

/// The Mac app: staged in the background, installed by swapping the bundle when the person
/// restarts or quits (the running app keeps its files until then).
#[cfg(target_os = "macos")]
pub mod mac_app {
    use super::*;

    pub const PRODUCT: &str = "glasscode-mac";
    const BUNDLE_ID: &str = "dev.glasscode";

    /// The .app this process runs from; None for a development binary.
    pub fn current_install() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
        (bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then_some(bundle)
    }

    fn staging(build: u64) -> PathBuf {
        updates_dir().join(format!("mac-{build}"))
    }

    /// An update downloaded earlier and waiting: (build, version, staged .app).
    pub fn staged() -> Option<(u64, String, PathBuf)> {
        let mut best: Option<(u64, String, PathBuf)> = None;
        for entry in std::fs::read_dir(updates_dir()).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(build) = name.strip_prefix("mac-").and_then(|b| b.parse::<u64>().ok()) else { continue };
            let Ok(version) = std::fs::read_to_string(entry.path().join("ready")) else { continue };
            let app = entry.path().join("Glasscode.app");
            if build > BUILD && app.is_dir() && best.as_ref().is_none_or(|(b, _, _)| build > *b) {
                best = Some((build, version.trim().to_string(), app));
            }
        }
        best
    }

    /// Download, unpack and check an update; returns the staged .app.
    ///
    /// TODO(glasscode): staging needs the self-hosted releases endpoint that `newer` is
    /// waiting on. Until that exists there is nothing to fetch, so this is a no-op; the
    /// local install path (`staged`, `install`) is kept for when a release is staged by
    /// other means.
    pub fn stage(release: &Manifest) -> anyhow::Result<PathBuf> {
        let _ = (release, BUNDLE_ID);
        bail!("auto-update is not configured in this build");
    }

    /// Put the staged app where the running one is (the running process keeps its files).
    pub fn install(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
        // Nothing is touched unless a complete update is there to take its place.
        if !staged.join("Contents/MacOS/Glasscode").is_file() {
            bail!("the downloaded update is gone; it downloads again");
        }
        let old = bundle.with_file_name(".Glasscode-replaced.app");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(bundle, &old).with_context(|| format!("could not replace {} (move Glasscode to a folder you can write to)", bundle.display()))?;
        if let Err(error) = std::fs::rename(staged, bundle) {
            let _ = std::fs::rename(&old, bundle);
            return Err(error).context("could not put the update in place");
        }
        if let Some(dir) = staged.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    /// After an update: the replaced app and older downloads go.
    pub fn clean_up(bundle: &Path) {
        let _ = std::fs::remove_dir_all(bundle.with_file_name(".Glasscode-replaced.app"));
        if let Ok(entries) = std::fs::read_dir(updates_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.strip_prefix("mac-").and_then(|b| b.parse::<u64>().ok()).is_some_and(|b| b <= BUILD) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    /// Open the app again once this process has exited.
    pub fn relaunch_after_exit(bundle: &Path) -> anyhow::Result<()> {
        let pid = std::process::id();
        // -n: this copy, even when another one with the same bundle id runs.
        let script = format!("while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; /usr/bin/open -n \"$0\"");
        crate::sys::command("/bin/sh").arg("-c").arg(script).arg(bundle).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
        Ok(())
    }
}

/// The Windows app: staged in the background, put in place when the person restarts or quits.
/// Windows does not let the file of a running program be replaced or removed, but it lets it be
/// renamed: the running programs step aside as `*.old.exe` and go at the next start.
#[cfg(windows)]
pub mod windows_app {
    use super::*;

    pub const PRODUCT: &str = "glasscode-windows";
    /// The window program, as the installer names it (cargo names it `glasscode.exe`).
    const APP: &str = "Glasscode.exe";
    /// The console program next to it. An update replaces the two together, as the app copies
    /// this one for the `glasscode` command at each start.
    const CONSOLE: &str = "glasscode-cli.exe";

    /// The installed Glasscode.exe this process runs from; None for a development binary.
    pub fn current_install() -> Option<PathBuf> {
        // The real path has the name as it is on the disk, whatever name started the program.
        let exe = crate::sys::clean_path(&std::env::current_exe().ok()?.canonicalize().ok()?);
        (exe.file_name()? == APP && exe.with_file_name(CONSOLE).is_file()).then_some(exe)
    }

    /// An update downloaded earlier and waiting: (build, version, staged folder with the programs).
    pub fn staged() -> Option<(u64, String, PathBuf)> {
        let mut best: Option<(u64, String, PathBuf)> = None;
        for entry in std::fs::read_dir(updates_dir()).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(build) = name.strip_prefix("windows-").and_then(|b| b.parse::<u64>().ok()) else { continue };
            let Ok(version) = std::fs::read_to_string(entry.path().join("ready")) else { continue };
            if is_newer(build) && entry.path().join(APP).is_file() && best.as_ref().is_none_or(|(b, _, _)| build > *b) {
                best = Some((build, version.trim().to_string(), entry.path()));
            }
        }
        best
    }

    /// Whether a downloaded build comes after this one; an older one is of no use any more.
    fn is_newer(build: u64) -> bool {
        build > BUILD
    }

    /// Download and check an update; returns the staged folder.
    ///
    /// TODO(glasscode): as on the Mac, staging needs the self-hosted releases endpoint that
    /// `newer` is waiting on; `staged` and `install` work for a release staged by other means.
    pub fn stage(release: &Manifest) -> anyhow::Result<PathBuf> {
        let _ = release;
        bail!("auto-update is not configured in this build");
    }

    /// Put the staged programs where the running ones are (`program` is the running Glasscode.exe).
    pub fn install(staged: &Path, program: &Path) -> anyhow::Result<()> {
        // Nothing is touched unless a complete update is there to take its place.
        if !staged.join(APP).is_file() {
            bail!("the downloaded update is gone; it downloads again");
        }
        // The console program first: it is the easy one to put back.
        let console = program.with_file_name(CONSOLE);
        let console_had = if staged.join(CONSOLE).is_file() { Some(swap(&staged.join(CONSOLE), &console)?) } else { None };
        if let Err(error) = swap(&staged.join(APP), program) {
            if let Some(had) = console_had {
                unswap(&staged.join(CONSOLE), &console, had);
            }
            return Err(error);
        }
        let _ = std::fs::remove_dir_all(staged);
        Ok(())
    }

    /// The name a replaced program has until the next start: `Glasscode.old.exe`.
    fn old_file(current: &Path) -> PathBuf {
        current.with_extension("old.exe")
    }

    /// Move `new` to `current`; the file there steps aside to `old_file`. Gives whether one did.
    fn swap(new: &Path, current: &Path) -> anyhow::Result<bool> {
        let old = old_file(current);
        let _ = std::fs::remove_file(&old);
        let had = current.exists();
        if had {
            std::fs::rename(current, &old).with_context(|| format!("could not replace {} (install Glasscode in a folder you can write to)", current.display()))?;
        }
        // A file cannot be renamed to another drive: the update is copied then.
        if let Err(error) = std::fs::rename(new, current).or_else(|_| std::fs::copy(new, current).map(drop)) {
            if had {
                let _ = std::fs::rename(&old, current);
            }
            return Err(error).context("could not put the update in place");
        }
        Ok(had)
    }

    /// Undo `swap`: the new file goes back to `new`, so the update stays whole for the next try,
    /// and the old one comes back.
    fn unswap(new: &Path, current: &Path, had: bool) {
        let _ = std::fs::rename(current, new);
        if had {
            let _ = std::fs::rename(old_file(current), current);
        }
    }

    /// After an update: the replaced programs and older downloads go. A replaced program that
    /// still runs keeps its file until a later start.
    pub fn clean_up(program: &Path) {
        for name in [APP, CONSOLE] {
            let _ = std::fs::remove_file(old_file(&program.with_file_name(name)));
        }
        if let Ok(entries) = std::fs::read_dir(updates_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.strip_prefix("windows-").and_then(|b| b.parse::<u64>().ok()).is_some_and(|b| !is_newer(b)) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    /// Open the app again once this process has exited. Windows PowerShell waits for it, as `sh`
    /// does on the Mac, and runs on its own, so it outlives this process.
    pub fn relaunch_after_exit(program: &Path) -> anyhow::Result<()> {
        let pid = std::process::id();
        let root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
        let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        // The path comes in the environment, where no quotation rules of PowerShell apply; the
        // new app does not keep the variable.
        let script = format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; $app = $env:GLASSCODE_RELAUNCH; Remove-Item Env:GLASSCODE_RELAUNCH; Start-Process -FilePath $app");
        let mut command = crate::sys::command(&powershell);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script]).env("GLASSCODE_RELAUNCH", program);
        crate::sys::spawn_detached(&mut command)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_release_key_counts() {
        let (pkcs8, public) = generate_key().unwrap();
        let manifest = Manifest {
            product: "glasscoded".into(),
            version: "0.1.0".into(),
            build: 7,
            channel: "stable".into(),
            notes: String::new(),
            published_at: 1,
            files: vec![ReleaseFile { name: "glasscoded-linux-x86_64".into(), os: "linux".into(), arch: "x86_64".into(), size: 3, sha256: "ab".into() }],
        };
        let signed = sign(&manifest, &pkcs8).unwrap();
        assert_eq!(verify(&signed, &[public.as_str()]).unwrap(), manifest);
        // Another key, a changed manifest, a changed signature: refused.
        let (_, other) = generate_key().unwrap();
        assert!(verify(&signed, &[other.as_str()]).is_err());
        let mut changed = signed.clone();
        changed.manifest = changed.manifest.replace("\"build\":7", "\"build\":8");
        assert!(verify(&changed, &[public.as_str()]).is_err());
        let mut bad = signed.clone();
        bad.signature = B64.encode([0u8; 64]);
        assert!(verify(&bad, &[public.as_str()]).is_err());
        assert_eq!(manifest.file_for("linux", "x86_64").unwrap().name, "glasscoded-linux-x86_64");
        assert!(manifest.file_for("macos", "aarch64").is_none());
    }

    #[test]
    fn build_numbers() {
        assert_eq!(parse_build(None), 0);
        assert_eq!(parse_build(Some("202610161800")), 202610161800);
        assert_eq!(parse_build(Some("12a")), 0);
        assert_eq!(describe("0.1.0", 0), "0.1.0 (development build)");
    }

    #[test]
    fn this_os_is_a_release_name() {
        assert!(["macos", "linux", "windows"].contains(&this_os()));
        assert_eq!(this_os() == "windows", cfg!(windows));
    }

    /// The file as a running program holds it: others can rename it, not write it, and with
    /// `rename: false` not rename it either.
    #[cfg(windows)]
    fn hold(path: &Path, rename: bool) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_DELETE: u32 = 0x4;
        let share = if rename { FILE_SHARE_READ | FILE_SHARE_DELETE } else { FILE_SHARE_READ };
        std::fs::OpenOptions::new().read(true).share_mode(share).open(path).unwrap()
    }

    /// A folder with the two programs, each with "<text> app" or "<text> console" in it.
    #[cfg(windows)]
    fn programs(folder: &Path, text: &str) {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("Glasscode.exe"), format!("{text} app")).unwrap();
        std::fs::write(folder.join("glasscode-cli.exe"), format!("{text} console")).unwrap();
    }

    #[cfg(windows)]
    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_replaces_the_running_programs_and_keeps_the_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = (dir.path().join("Glasscode"), dir.path().join("updates").join("windows-9"));
        programs(&app, "old");
        programs(&staged, "new");
        let program = app.join("Glasscode.exe");
        let running = hold(&program, true);
        assert!(std::fs::write(&program, "new app").is_err(), "a running program cannot be written over");
        windows_app::install(&staged, &program).unwrap();
        drop(running);
        assert_eq!(read(&program), "new app");
        assert_eq!(read(&app.join("Glasscode.old.exe")), "old app");
        assert_eq!(read(&app.join("glasscode-cli.exe")), "new console");
        assert_eq!(read(&app.join("glasscode-cli.old.exe")), "old console");
        assert!(!staged.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_without_the_staged_program_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = (dir.path().join("Glasscode"), dir.path().join("updates").join("windows-9"));
        programs(&app, "old");
        let program = app.join("Glasscode.exe");
        // No staged folder, then one that has only the console program.
        assert!(windows_app::install(&staged, &program).is_err());
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("glasscode-cli.exe"), "new console").unwrap();
        assert!(windows_app::install(&staged, &program).is_err());
        assert_eq!(read(&program), "old app");
        assert_eq!(read(&app.join("glasscode-cli.exe")), "old console");
        assert_eq!(read(&staged.join("glasscode-cli.exe")), "new console");
        assert!(!app.join("Glasscode.old.exe").exists() && !app.join("glasscode-cli.old.exe").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_puts_the_console_program_back_when_the_app_cannot_move() {
        let dir = tempfile::tempdir().unwrap();
        let (app, staged) = (dir.path().join("Glasscode"), dir.path().join("updates").join("windows-9"));
        programs(&app, "old");
        programs(&staged, "new");
        let program = app.join("Glasscode.exe");
        let locked = hold(&program, false);
        assert!(windows_app::install(&staged, &program).is_err());
        drop(locked);
        assert_eq!(read(&program), "old app");
        assert_eq!(read(&app.join("glasscode-cli.exe")), "old console");
        assert!(!app.join("Glasscode.old.exe").exists() && !app.join("glasscode-cli.old.exe").exists());
        // The update is still whole, for the next try.
        assert_eq!(read(&staged.join("Glasscode.exe")), "new app");
        assert_eq!(read(&staged.join("glasscode-cli.exe")), "new console");
    }
}
