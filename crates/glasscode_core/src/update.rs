//! Updates for Glasscode's own programs (the Mac app, the Windows app, glasscoded), published as
//! GitHub releases of [`REPOSITORY`] (`docs/releasing.md`).
//!
//! A release is a manifest (product, build, files with their SHA-256) signed with the
//! Glasscode release key, which only the release workflow has. Every program has the public
//! key built in and installs only what that key signed, so neither GitHub nor the network can
//! slip in a binary: they only store and serve the files.
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

/// The GitHub repository whose releases the programs update from.
pub const REPOSITORY: &str = "glasscode-ide/glasscode";

/// The products that have releases so far. The others (the Mac app, glasscoded) ask nobody: a
/// program must not call a server that has nothing for it.
const PUBLISHED: &[&str] = &["glasscode-windows"];

/// The most a signed manifest or a list of releases can be.
const TEXT_LIMIT: u64 = 1 << 20;

/// Where releases come from. Each release has `<product>.json` (a [`SignedRelease`]) beside the
/// files that its manifest names.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// The releases of a GitHub repository (`owner/name`); the tag of a release is `v<version>`.
    GitHub(String),
    /// A folder on a web server with one release: `<url>/<product>.json` and the files. For tests
    /// and for a team that serves its own builds (`GLASSCODE_UPDATE_URL`).
    Folder(String),
}

impl Source {
    /// `GLASSCODE_UPDATE_URL` when it is set, else the releases of [`REPOSITORY`]. What comes from
    /// either is installed only with the signature of a built-in key.
    pub fn configured() -> Source {
        match std::env::var("GLASSCODE_UPDATE_URL").ok().map(|url| url.trim().trim_end_matches('/').to_string()).filter(|url| !url.is_empty()) {
            Some(url) => Source::Folder(url),
            None => Source::GitHub(REPOSITORY.to_string()),
        }
    }

    /// The signed manifest of the newest release for `channel`; None when there is none (no
    /// release yet, none with this product, or a repository that is not public).
    fn signed_release(&self, product: &str, channel: &str) -> anyhow::Result<Option<String>> {
        let url = match self {
            Source::Folder(base) => format!("{base}/{product}.json"),
            // GitHub's "latest" is the newest release that is neither a draft nor a pre-release.
            Source::GitHub(repository) if channel != "beta" => format!("https://github.com/{repository}/releases/latest/download/{product}.json"),
            // Beta takes pre-releases too, which only the list of releases shows.
            Source::GitHub(repository) => {
                let Some(list) = get_text(&format!("https://api.github.com/repos/{repository}/releases?per_page=20"))? else { return Ok(None) };
                let Some(tag) = newest_tag(&list)? else { return Ok(None) };
                format!("https://github.com/{repository}/releases/download/{tag}/{product}.json")
            }
        };
        get_text(&url)
    }

    fn file_url(&self, release: &Manifest, name: &str) -> String {
        match self {
            Source::Folder(base) => format!("{base}/{name}"),
            Source::GitHub(repository) => format!("https://github.com/{repository}/releases/download/v{}/{name}", release.version),
        }
    }
}

/// The tag of the newest release in GitHub's list of releases (newest first) that is no draft.
fn newest_tag(list: &str) -> anyhow::Result<Option<String>> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        #[serde(default)]
        draft: bool,
    }
    let releases: Vec<Release> = serde_json::from_str(list).context("GitHub's list of releases is not valid")?;
    Ok(releases.into_iter().find(|r| !r.draft).map(|r| r.tag_name).filter(|tag| plain(tag)))
}

/// A version, a tag or a file name that is safe in a URL and as a file name: nothing that leaves
/// the folder. A manifest is signed, and still its words are not trusted as paths.
fn plain(word: &str) -> bool {
    !word.is_empty() && !word.starts_with('.') && word.len() <= 200 && word.chars().all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
}

fn agent() -> ureq::Agent {
    let seconds = std::time::Duration::from_secs;
    ureq::AgentBuilder::new().timeout_connect(seconds(15)).timeout_read(seconds(60)).user_agent(concat!("glasscode/", env!("CARGO_PKG_VERSION"))).build()
}

/// GET; None when the server says there is no such thing (404).
fn get(url: &str) -> anyhow::Result<Option<ureq::Response>> {
    match agent().get(url).call() {
        Ok(response) => Ok(Some(response)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => bail!("the release server answered {code} for {url}"),
        Err(error) => Err(anyhow::anyhow!("could not reach the release server: {error}")),
    }
}

fn get_text(url: &str) -> anyhow::Result<Option<String>> {
    let Some(response) = get(url)? else { return Ok(None) };
    let mut text = String::new();
    response.into_reader().take(TEXT_LIMIT + 1).read_to_string(&mut text).context("could not read the release server's answer")?;
    if text.len() as u64 > TEXT_LIMIT {
        bail!("the release server's answer is too long");
    }
    Ok(Some(text))
}

/// The newest release of `product` on `channel` that this build should take, checked. A
/// development build (`current` 0) and a product without releases ask nobody.
pub fn newer(product: &str, channel: &str, current: u64) -> anyhow::Result<Option<Manifest>> {
    if current == 0 || !PUBLISHED.contains(&product) {
        return Ok(None);
    }
    newer_from(&Source::configured(), RELEASE_KEYS, product, channel, current)
}

/// [`newer`] from a given source, trusting the given keys.
pub fn newer_from(source: &Source, keys: &[&str], product: &str, channel: &str, current: u64) -> anyhow::Result<Option<Manifest>> {
    let Some(text) = source.signed_release(product, channel)? else { return Ok(None) };
    let signed: SignedRelease = serde_json::from_str(&text).context("the release file is not a signed release")?;
    let release = verify(&signed, keys)?;
    if release.product != product {
        bail!("the release is {}, not {product}; not installing it", release.product);
    }
    if !plain(&release.version) || release.files.iter().any(|f| !plain(&f.name)) {
        bail!("the release has a version or a file name that is not plain; not installing it");
    }
    // A stable program never takes a beta release, and nobody goes back to an older build: an
    // old release that is served again is signed too.
    let wanted = release.channel != "beta" || channel == "beta";
    Ok((wanted && release.build > current).then_some(release))
}

/// Download one file of a release to `to` and check it: its size and its SHA-256 are in the
/// signed manifest. Nothing stays at `to` when it is not the file.
pub fn download(source: &Source, release: &Manifest, file: &ReleaseFile, to: &Path) -> anyhow::Result<()> {
    let result = download_unchecked(source, release, file, to).and_then(|()| {
        let (sha256, size) = sha256_file(to)?;
        if size != file.size || !sha256.eq_ignore_ascii_case(&file.sha256) {
            bail!("{} is not the file that was released (its size or its SHA-256 differs); not installing it", file.name);
        }
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result
}

fn download_unchecked(source: &Source, release: &Manifest, file: &ReleaseFile, to: &Path) -> anyhow::Result<()> {
    let url = source.file_url(release, &file.name);
    let response = get(&url)?.with_context(|| format!("the release has no {}", file.name))?;
    let mut out = std::fs::File::create(to).with_context(|| format!("could not write {}", to.display()))?;
    // One byte more than the manifest says is enough to know that it is another file.
    std::io::copy(&mut response.into_reader().take(file.size + 1), &mut out).with_context(|| format!("could not download {}", file.name))?;
    Ok(())
}

/// Publishing: the manifest of a release with one file for each system, as (file, os, arch);
/// the name in the manifest is the file's name.
pub fn manifest_for(product: &str, version: &str, build: u64, channel: &str, notes: &str, files: &[(PathBuf, String, String)]) -> anyhow::Result<Manifest> {
    if build == 0 {
        bail!("a release needs a build number");
    }
    let mut list = Vec::new();
    for (path, os, arch) in files {
        let name = path.file_name().and_then(|n| n.to_str()).filter(|n| plain(n)).with_context(|| format!("{} has no plain file name", path.display()))?;
        let (sha256, size) = sha256_file(path)?;
        list.push(ReleaseFile { name: name.to_string(), os: os.clone(), arch: arch.clone(), size, sha256 });
    }
    if !plain(version) || list.is_empty() {
        bail!("a release needs a plain version and a file");
    }
    let published_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(Manifest { product: product.to_string(), version: version.to_string(), build, channel: channel.to_string(), notes: notes.to_string(), published_at, files: list })
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
    /// In a staged folder: the version, written last, so a folder with it is whole.
    const READY: &str = "ready";

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
            let Ok(version) = std::fs::read_to_string(entry.path().join(READY)) else { continue };
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

    /// Download, check and unpack an update; returns the staged folder.
    pub fn stage(release: &Manifest) -> anyhow::Result<PathBuf> {
        stage_from(&Source::configured(), release, &updates_dir())
    }

    /// [`stage`] from a given source into a given updates folder. The release file is a zip with
    /// the programs at its top, as `script/bundle-windows.ps1` makes it.
    pub fn stage_from(source: &Source, release: &Manifest, updates: &Path) -> anyhow::Result<PathBuf> {
        let file = release.file_for("windows", this_arch()).with_context(|| format!("the release has no file for Windows on {}", this_arch()))?;
        let staged = updates.join(format!("windows-{}", release.build));
        // `staged` does not take a folder for an update before it has its last name.
        let work = updates.join(format!("windows-{}.part", release.build));
        for folder in [&staged, &work] {
            if folder.exists() {
                std::fs::remove_dir_all(folder).with_context(|| format!("could not remove {}", folder.display()))?;
            }
        }
        std::fs::create_dir_all(&work).with_context(|| format!("could not make {}", work.display()))?;
        let result = (|| {
            let archive = work.join("update.zip");
            download(source, release, file, &archive)?;
            unpack(&archive, &work)?;
            std::fs::remove_file(&archive)?;
            if !work.join(APP).is_file() || !work.join(CONSOLE).is_file() {
                bail!("the update has no {APP} and {CONSOLE}; not installing it");
            }
            std::fs::write(work.join(READY), &release.version)?;
            std::fs::rename(&work, &staged).with_context(|| format!("could not make {}", staged.display()))
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&work);
        }
        result.map(|()| staged)
    }

    /// Unpack a zip with the `tar.exe` of Windows (there since Windows 10 1803), which reads zip
    /// files and writes nothing outside the folder.
    fn unpack(archive: &Path, into: &Path) -> anyhow::Result<()> {
        let root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
        let tar = Path::new(&root).join(r"System32\tar.exe");
        let output = crate::sys::command(&tar).arg("-xf").arg(archive).arg("-C").arg(into).stdin(std::process::Stdio::null()).output().with_context(|| format!("could not start {}", tar.display()))?;
        if !output.status.success() {
            bail!("could not unpack the update: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(())
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
        // The programs are in place, so the update is done; what is left of it must not undo that.
        if let Some(folder) = program.parent()
            && let Err(error) = copy_rest(staged, folder, true)
        {
            log::warn!("update: could not replace every other file in {}: {error}", folder.display());
        }
        let _ = std::fs::remove_dir_all(staged);
        Ok(())
    }

    /// The other files of an update (the server daemons, the licence texts) go over the installed
    /// ones: none of them runs on this computer, so each can be written.
    fn copy_rest(from: &Path, to: &Path, top: bool) -> std::io::Result<()> {
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if top && [APP, CONSOLE, READY].iter().any(|own| name == *own) {
                continue;
            }
            let target = to.join(&name);
            if entry.file_type()?.is_dir() {
                std::fs::create_dir_all(&target)?;
                copy_rest(&entry.path(), &target, false)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }

    /// After an update of an installed copy: "Installed apps" shows the new version. The entry is
    /// the installer's (`script/windows/installer.iss`, its AppId); a copy from the zip has none,
    /// and then nothing is written.
    pub fn note_installed(version: &str) {
        const ENTRY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{6F41BEC8-88F7-46B2-AFDC-1EB89617F216}_is1";
        if !plain(version) {
            return;
        }
        let Some(root) = std::env::var_os("SystemRoot") else { return };
        let reg = Path::new(&root).join(r"System32\reg.exe");
        let run = |arguments: &[&str]| crate::sys::command(&reg).args(arguments).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success());
        if run(&["query", ENTRY, "/v", "DisplayVersion"]) && !run(&["add", ENTRY, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version, "/f"]) {
            log::warn!("update: could not write the version to the entry in Installed apps");
        }
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
                // A download that did not finish (`windows-<build>.part`) goes too: it starts again.
                let Some(rest) = name.strip_prefix("windows-") else { continue };
                if rest.ends_with(".part") || rest.parse::<u64>().is_ok_and(|b| !is_newer(b)) {
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

    /// A web server on this computer that serves the files of `folder`, as GitHub serves the files
    /// of a release; gives its URL. It answers until the test process ends.
    fn serve(folder: &Path) -> String {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let folder = folder.to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                    request.push(byte[0]);
                }
                let request = String::from_utf8_lossy(&request).to_string();
                let name = request.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
                let answer = match std::fs::read(folder.join(&name)) {
                    Ok(body) if plain(&name) => [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes(), body].concat(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&answer);
            }
        });
        url
    }

    /// Publish a release into `folder` as the release workflow does: the file, and the signed
    /// manifest `<product>.json`.
    fn publish(folder: &Path, key: &[u8], product: &str, build: u64, channel: &str, file: &Path) -> Manifest {
        let manifest = manifest_for(product, "0.2.0", build, channel, "", &[(file.to_path_buf(), this_os().to_string(), this_arch().to_string())]).unwrap();
        std::fs::write(folder.join(format!("{product}.json")), serde_json::to_string(&sign(&manifest, key).unwrap()).unwrap()).unwrap();
        manifest
    }

    #[test]
    fn a_release_is_taken_only_when_it_is_signed_newer_and_for_this_program() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("glasscode-0.2.0.bin");
        std::fs::write(&file, "the new program").unwrap();
        let (key, public) = generate_key().unwrap();
        let keys = [public.as_str()];
        let source = Source::Folder(serve(dir.path()));

        // Nothing is published yet: no update, and no error.
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "stable", 5).unwrap(), None);

        let release = publish(dir.path(), &key, "glasscode-windows", 9, "stable", &file);
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "stable", 5).unwrap(), Some(release.clone()));
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "beta", 5).unwrap(), Some(release.clone()));
        // The same build and a later one have nothing to take.
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "stable", 9).unwrap(), None);
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "stable", 12).unwrap(), None);
        // Another key signed it: refused.
        let (_, other) = generate_key().unwrap();
        assert!(newer_from(&source, &[other.as_str()], "glasscode-windows", "stable", 5).is_err());

        // The file comes down and is checked against the manifest.
        let to = dir.path().join("download");
        download(&source, &release, &release.files[0], &to).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "the new program");
        // A file that changed after the release: refused, and nothing stays.
        std::fs::write(&file, "another program!").unwrap();
        assert!(download(&source, &release, &release.files[0], &to).is_err());
        std::fs::write(&file, "the new program, and more").unwrap();
        assert!(download(&source, &release, &release.files[0], &to).is_err());
        assert!(!to.exists());
        // A file that the release does not have.
        std::fs::remove_file(&file).unwrap();
        assert!(download(&source, &release, &release.files[0], &to).is_err());

        // A beta release is for the beta channel only.
        std::fs::write(&file, "the beta program").unwrap();
        let beta = publish(dir.path(), &key, "glasscode-windows", 10, "beta", &file);
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "stable", 5).unwrap(), None);
        assert_eq!(newer_from(&source, &keys, "glasscode-windows", "beta", 5).unwrap(), Some(beta));

        // The manifest of another product under this product's name: refused.
        let daemon = publish(dir.path(), &key, "glasscoded", 11, "stable", &file);
        std::fs::copy(dir.path().join("glasscoded.json"), dir.path().join("glasscode-windows.json")).unwrap();
        assert!(newer_from(&source, &keys, "glasscode-windows", "stable", 5).is_err());
        assert_eq!(newer_from(&source, &keys, "glasscoded", "stable", 5).unwrap(), Some(daemon));

        // A file that is no signed release.
        std::fs::write(dir.path().join("glasscode-windows.json"), "<html>").unwrap();
        assert!(newer_from(&source, &keys, "glasscode-windows", "stable", 5).is_err());
    }

    #[test]
    fn a_development_build_and_a_product_without_releases_ask_nobody() {
        // Either would fail on the way to a server if it asked: the source is GitHub.
        assert_eq!(newer("glasscode-windows", "stable", 0).unwrap(), None);
        assert_eq!(newer("glasscode-mac", "stable", 5).unwrap(), None);
        assert_eq!(newer("glasscoded", "stable", 5).unwrap(), None);
    }

    #[test]
    fn names_from_a_manifest_stay_in_their_folder() {
        for good in ["0.2.0", "0.2.0-beta.1", "v0.2.0", "Glasscode-0.2.0-windows-x86_64.zip"] {
            assert!(plain(good), "{good}");
        }
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "a b", "c:x", "a?b", "a%2fb"] {
            assert!(!plain(bad), "{bad}");
        }
        let (key, public) = generate_key().unwrap();
        let mut manifest = Manifest { product: "glasscode-windows".into(), version: "0.2.0".into(), build: 9, channel: "stable".into(), notes: String::new(), published_at: 1, files: vec![ReleaseFile { name: "../evil.zip".into(), os: "windows".into(), arch: "x86_64".into(), size: 1, sha256: "ab".into() }] };
        let dir = tempfile::tempdir().unwrap();
        let source = Source::Folder(serve(dir.path()));
        let put = |manifest: &Manifest| std::fs::write(dir.path().join("glasscode-windows.json"), serde_json::to_string(&sign(manifest, &key).unwrap()).unwrap()).unwrap();
        put(&manifest);
        assert!(newer_from(&source, &[public.as_str()], "glasscode-windows", "stable", 5).is_err());
        manifest.files[0].name = "good.zip".into();
        manifest.version = "0.2.0/../..".into();
        put(&manifest);
        assert!(newer_from(&source, &[public.as_str()], "glasscode-windows", "stable", 5).is_err());
    }

    #[test]
    fn the_newest_release_of_githubs_list_that_is_no_draft() {
        let list = r#"[{"tag_name":"v0.3.0","draft":true,"prerelease":false},{"tag_name":"v0.3.0-beta.1","draft":false,"prerelease":true},{"tag_name":"v0.2.0","draft":false,"prerelease":false}]"#;
        assert_eq!(newest_tag(list).unwrap().as_deref(), Some("v0.3.0-beta.1"));
        assert_eq!(newest_tag("[]").unwrap(), None);
        assert_eq!(newest_tag(r#"[{"tag_name":"../x"}]"#).unwrap(), None);
        assert!(newest_tag(r#"{"message":"Not Found"}"#).is_err());
        let github = Source::GitHub("owner/name".into());
        let release = Manifest { product: "glasscode-windows".into(), version: "0.2.0".into(), build: 9, channel: "stable".into(), notes: String::new(), published_at: 1, files: Vec::new() };
        assert_eq!(github.file_url(&release, "a.zip"), "https://github.com/owner/name/releases/download/v0.2.0/a.zip");
    }

    /// The whole way of an update on Windows: a release as the bundle script and the release
    /// workflow make it is found, downloaded, unpacked and put in place of the running programs.
    #[cfg(windows)]
    #[test]
    fn windows_update_from_a_release_to_the_installed_programs() {
        let dir = tempfile::tempdir().unwrap();
        let (app, release_folder, updates) = (dir.path().join("Glasscode"), dir.path().join("release"), dir.path().join("updates"));
        programs(&app, "old");
        std::fs::create_dir_all(app.join("glasscoded")).unwrap();
        std::fs::write(app.join("glasscoded").join("glasscoded-linux-x86_64"), "old daemon").unwrap();
        // The new build, zipped with the programs at the top (bundle-windows.ps1 does the same).
        let new = dir.path().join("new");
        programs(&new, "new");
        std::fs::create_dir_all(new.join("glasscoded")).unwrap();
        std::fs::write(new.join("glasscoded").join("glasscoded-linux-x86_64"), "new daemon").unwrap();
        std::fs::write(new.join("LICENSE.txt"), "the licence").unwrap();
        std::fs::create_dir_all(&release_folder).unwrap();
        let zip = release_folder.join(format!("Glasscode-0.2.0-windows-{}.zip", this_arch()));
        let tar = Path::new(&std::env::var_os("SystemRoot").unwrap()).join(r"System32\tar.exe");
        assert!(std::process::Command::new(&tar).arg("-a").arg("-cf").arg(&zip).arg("-C").arg(&new).arg(".").status().unwrap().success());

        let (key, public) = generate_key().unwrap();
        publish(&release_folder, &key, windows_app::PRODUCT, 9, "stable", &zip);
        let source = Source::Folder(serve(&release_folder));
        let release = newer_from(&source, &[public.as_str()], windows_app::PRODUCT, "stable", 5).unwrap().expect("a newer release");
        let staged = windows_app::stage_from(&source, &release, &updates).unwrap();
        assert_eq!(staged, updates.join("windows-9"));
        assert_eq!(read(&staged.join("ready")), "0.2.0");
        assert!(!staged.join("update.zip").exists() && !updates.join("windows-9.part").exists());

        let program = app.join("Glasscode.exe");
        let running = hold(&program, true);
        windows_app::install(&staged, &program).unwrap();
        drop(running);
        assert_eq!(read(&program), "new app");
        assert_eq!(read(&app.join("glasscode-cli.exe")), "new console");
        assert_eq!(read(&app.join("glasscoded").join("glasscoded-linux-x86_64")), "new daemon");
        assert_eq!(read(&app.join("LICENSE.txt")), "the licence");
        assert!(!app.join("ready").exists() && !staged.exists());

        // A zip that is not the released one, and one without the programs: nothing is staged.
        std::fs::write(&zip, "not a zip").unwrap();
        assert!(windows_app::stage_from(&source, &release, &updates).is_err());
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(empty.join("readme.txt"), "no programs").unwrap();
        std::fs::remove_file(&zip).unwrap();
        assert!(std::process::Command::new(&tar).arg("-a").arg("-cf").arg(&zip).arg("-C").arg(&empty).arg(".").status().unwrap().success());
        publish(&release_folder, &key, windows_app::PRODUCT, 10, "stable", &zip);
        let release = newer_from(&source, &[public.as_str()], windows_app::PRODUCT, "stable", 5).unwrap().unwrap();
        assert!(windows_app::stage_from(&source, &release, &updates).is_err());
        assert!(std::fs::read_dir(&updates).unwrap().next().is_none(), "nothing half done stays");
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
