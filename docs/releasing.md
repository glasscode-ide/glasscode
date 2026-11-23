# Releasing Glasscode

How a version of Glasscode gets to the people who use it, and how an installed Glasscode updates
itself. For the owners.

## In short

1. Set the version in `Cargo.toml` (`[workspace.package]`, one line) and give the "Unreleased"
   section of `CHANGELOG.md` that version: `## [0.2.0] - 2026-12-01`. Merge that.
2. Tag that commit and push the tag: `git tag v0.2.0`, `git push origin v0.2.0`.
3. The workflow "Release" builds the Windows files and makes a **draft** release.
4. Read the draft on GitHub (Releases), install its setup file on a test computer, then press
   **Publish release**. From then on installed copies update to it.

A version with a suffix (`0.2.0-beta.1`) becomes a pre-release: only copies on the Beta channel
(Settings › Updates) take it.

## What a release has

| File | For |
|---|---|
| `Glasscode-<version>-setup.exe` | A person who installs Glasscode (per user, no administrator rights). |
| `Glasscode-<version>-windows-x86_64.zip` | The same programs without an installation, and the file an installed Glasscode updates from. |
| `*.sha256` | Checking a download by hand: `sha256sum -c <file>.sha256`. |
| `glasscode-windows.json` | The signed manifest. Only with the secret `GLASSCODE_RELEASE_KEY` of the environment `release` (below). |

The Mac files are not built by the workflow: no runner signs and notarizes them yet. Whoever
builds the Mac app adds them to the draft: `script/bundle-mac.sh`, then
`gh release upload v0.2.0 target/release/Glasscode.dmg`. The Mac app does not update itself yet
(`update::mac_app::stage` is not written).

## How an installed Glasscode updates

`glasscode_core::update`, `crates/glasscode/src/update_ui.rs`.

1. Every six hours (and at "Check now") the Windows app asks
   `https://github.com/glasscode-ide/glasscode/releases/latest/download/glasscode-windows.json`.
   GitHub's "latest" is the newest published release that is no pre-release. The Beta channel
   reads the list of releases instead and takes the newest one, pre-release or not.
2. It checks the signature of the manifest against the public keys built into the program
   (`RELEASE_KEYS`). A manifest with another signature, for another product, for a build that is
   not higher than its own, or a beta manifest on the Stable channel is not taken.
3. It downloads the zip the manifest names from the release `v<version>`, and checks its size and
   SHA-256 against the manifest.
4. It unpacks the zip into `~/.glasscode/updates/windows-<build>` and offers "Restart to update".
   At that click, or when Glasscode quits (Settings › Updates), the running programs step aside as
   `*.old.exe` and the new ones take their place; the other files (the server daemons, the
   licence texts) are written over.

So GitHub and the network only carry the files. What decides is the release key.

A development build (no build number), and a copy that does not run as `Glasscode.exe` beside
`glasscode-cli.exe`, do not update. While the repository is private, the address of step 1 answers
"not found" and Glasscode reports "Up to date": **updates work only for a public repository.**

`GLASSCODE_UPDATE_URL=https://host/folder` makes Glasscode read `<folder>/glasscode-windows.json`
and the zip from there instead, for a team that serves its own builds. The signature is checked
all the same.

## The release key

An Ed25519 key. The private key signs manifests; the public key is in the source
(`RELEASE_KEYS` in `crates/glasscode_core/src/update.rs`). Whoever has the private key can make
every installed Glasscode install a program, so:

- it is in one place that builds use: the secret `GLASSCODE_RELEASE_KEY` of the GitHub
  **environment** `release` (Settings › Environments), not a secret of the whole repository;
- that environment has **required reviewers** (the owners) and a rule that only the tags `v*`
  may use it, and the tags `v*` are protected (Settings › Rules), so that nobody signs a release
  alone and no run from a branch reaches the key. Only the last job of the workflow, "Publish",
  runs in that environment; the jobs that build the programs never see the key;
- a copy is where only the owners reach it (a password manager), because a lost key means no
  installed copy can be updated any more;
- it is never in a file of the repository, in a message or in a terminal that is recorded.

### Making the key (once)

`RELEASE_KEY_1` in the source came with the code. **Check who has its private key.** If nobody
does, make a new one:

```powershell
cargo run -p glasscode_core --example release -- key $env:TEMP\glasscode-release.key
```

It prints the public key and writes the private key into the file. Then:

1. Make the environment `release` with its reviewers and its tag rule (above). Put the text of
   the file into its secret `GLASSCODE_RELEASE_KEY`
   (`gh secret set GLASSCODE_RELEASE_KEY --env release < $env:TEMP\glasscode-release.key`), and
   into the owners' password manager.
2. Remove the file.
3. Put the printed public key into `RELEASE_KEYS` in place of `RELEASE_KEY_1`, in a pull request.
   Only builds made after that take releases signed with the new key.

The workflow refuses to sign with a key that is not in `RELEASE_KEYS`: such a release could not
be installed by anybody.

### Changing the key later

Add the new public key to `RELEASE_KEYS` beside the old one and release that build, signed with
the **old** key. When the copies in use have it, change the secret to the new key. Remove the old
public key some releases later.

If the private key is lost or stolen, there is no such way: publish a release, say so in its
text, and people install it by hand with the setup file.

## Signing the programs (SmartScreen)

Not the release key: a code signing certificate, which Windows checks. Without one, Windows warns
before the installer runs ("Windows protected your PC"). The secrets `WINDOWS_SIGN_CERT_BASE64`
(a `.pfx` in base64) and `WINDOWS_SIGN_CERT_PASSWORD` make the workflow sign the programs and
the installer. No certificate exists yet, and that path was never used.

## Before the first release

- The licence: `LICENSE`, `NOTICE`, and the agreement of everybody who wrote code.
- The licences the programs carry. cargo-about writes the Rust packages' ones
  (`cargo install --locked cargo-about --features cli`). The Swift packages' ones are committed
  files, written by `script/swift-notices.py mac` and `script/swift-notices.py glasses`: run them
  again whenever a `Package.resolved` or `LIVEKIT_VERSION` in `script/bundle-mac.sh` changes. A
  signed Mac build (`SIGN_IDENTITY`) stops when cargo-about is missing or the Mac file does not
  match the packages built. The glasses app shows its file in Settings › Licences.
- The repository is public, or updates do not work (above).
- The release key (above).
- Run the workflow by hand once (Actions › Release › Run workflow): it builds everything and
  makes no release.
- The installer on a computer that never had Glasscode: install, start, install a newer build
  over it, uninstall ([`windows-acceptance.md`](windows-acceptance.md)).
- One update for real: publish a release, install it, publish a second one, and see the first
  offer "Restart to update".

## Building the files by hand

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File script\bundle-windows.ps1
```

makes the folder, the zip and the installer in `target\release` ([`windows.md`](windows.md)). The
file `THIRD-PARTY-NOTICES.html` needs cargo-about
(`cargo install --locked cargo-about --features cli`); `about.toml` lists the licences a package
may have, and the build of a release stops at another one.
