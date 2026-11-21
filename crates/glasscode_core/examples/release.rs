//! The publisher's tool for Glasscode releases (`docs/releasing.md`). The release workflow runs
//! it; a person runs it once to make the release key.
//!
//!   cargo run -p glasscode_core --example release -- key <file>
//!       Make a new release key. The private key goes into <file> (base64, for the repository
//!       secret GLASSCODE_RELEASE_KEY); the public key is printed, for `update::RELEASE_KEYS`.
//!
//!   cargo run -p glasscode_core --example release -- sign --product glasscode-windows \
//!       --version 0.2.0 --build 1234 --channel stable --os windows --arch x86_64 \
//!       --file Glasscode-0.2.0-windows-x86_64.zip --out glasscode-windows.json [--notes-file notes.md]
//!       Write the signed manifest of a release. The private key comes from the environment
//!       variable GLASSCODE_RELEASE_KEY, never from an argument. It fails when the key is not one
//!       that this source trusts: nobody could install such a release.
//!
//!   cargo run -p glasscode_core --example release -- check <signed.json> <file>
//!       Check a signed manifest and a release file as an installed Glasscode does.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use glasscode_core::update::{self, SignedRelease};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("key") if args.len() == 2 => key(Path::new(&args[1])),
        Some("sign") => sign(&args[1..]),
        Some("check") if args.len() == 3 => check(Path::new(&args[1]), Path::new(&args[2])),
        _ => Err(anyhow::anyhow!("usage: release key <file> | sign --product … --version … --build … --channel … --os … --arch … --file … --out … [--notes-file …] | check <signed.json> <file>")),
    };
    if let Err(error) = result {
        eprintln!("release: {error:#}");
        std::process::exit(1);
    }
}

fn key(file: &Path) -> anyhow::Result<()> {
    use std::io::Write as _;
    let (pkcs8, public) = update::generate_key()?;
    // A new file only (a release key is never written over), and on Unix one that only its owner
    // reads. On Windows a file has the rights of its folder: write it into your own profile.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = match options.open(file) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => bail!("{} exists already; a release key is never written over", file.display()),
        other => other.with_context(|| format!("could not write {}", file.display()))?,
    };
    out.write_all(B64.encode(pkcs8).as_bytes()).with_context(|| format!("could not write {}", file.display()))?;
    println!("The private key is in {}. Put its text into the repository secret GLASSCODE_RELEASE_KEY,", file.display());
    println!("keep a copy where only the owners reach it, and remove the file. Never commit it.");
    println!("The public key, for RELEASE_KEYS in crates/glasscode_core/src/update.rs:");
    println!("{public}");
    Ok(())
}

fn sign(args: &[String]) -> anyhow::Result<()> {
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let need = |name: &str| value(name).with_context(|| format!("{name} is missing"));
    let key = std::env::var("GLASSCODE_RELEASE_KEY").ok().filter(|k| !k.trim().is_empty()).context("GLASSCODE_RELEASE_KEY is not set")?;
    let pkcs8 = B64.decode(key.trim()).context("GLASSCODE_RELEASE_KEY is not base64")?;
    let notes = match value("--notes-file") {
        Some(file) => std::fs::read_to_string(&file).with_context(|| format!("could not read {file}"))?,
        None => String::new(),
    };
    let build: u64 = need("--build")?.parse().context("--build is not a number")?;
    let file = PathBuf::from(need("--file")?);
    let manifest = update::manifest_for(&need("--product")?, &need("--version")?, build, &need("--channel")?, notes.trim(), &[(file.clone(), need("--os")?, need("--arch")?)])?;
    let signed = update::sign(&manifest, &pkcs8)?;
    update::verify(&signed, update::RELEASE_KEYS).context("GLASSCODE_RELEASE_KEY is not a key of RELEASE_KEYS in crates/glasscode_core/src/update.rs, so no installed Glasscode would take this release")?;
    let out = need("--out")?;
    std::fs::write(&out, serde_json::to_string(&signed)?).with_context(|| format!("could not write {out}"))?;
    println!("Signed {} {} (build {build}) with {}: {out}", manifest.product, manifest.version, manifest.files[0].name);
    Ok(())
}

fn check(signed: &Path, file: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(signed).with_context(|| format!("could not read {}", signed.display()))?;
    let signed: SignedRelease = serde_json::from_str(&text).context("not a signed release")?;
    let manifest = update::verify(&signed, update::RELEASE_KEYS)?;
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let listed = manifest.files.iter().find(|f| f.name == name).with_context(|| format!("the manifest does not name {name}"))?;
    let (sha256, size) = update::sha256_file(file)?;
    if size != listed.size || !sha256.eq_ignore_ascii_case(&listed.sha256) {
        bail!("{name} is not the file of the manifest");
    }
    println!("{} {} (build {}, {}): the signature and {name} are right", manifest.product, manifest.version, manifest.build, manifest.channel);
    Ok(())
}
