//! Updates for Glasscode's own programs (the Mac app, glasscoded), published to a releases endpoint.
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
    /// `glasscode-mac` or `glasscoded`.
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
    pub fn current_bundle() -> Option<PathBuf> {
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
        std::process::Command::new("/bin/sh").arg("-c").arg(script).arg(bundle).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
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
}
