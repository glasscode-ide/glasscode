# Maturity plan: releases, updates, and what to adopt from Shob

| | |
|---|---|
| Status | Part A is written (2026-11-20) on the local branch `baseline`, not pushed: the licence, A1 to A4, and the code of A5. What only a person or a published release can show is in section 4a. Parts B to D are not started. |
| Date | 2026-11-20 |
| Branch for the work | `windows-port` (not `main`) |
| Compared with | [olovalabs/shob](https://github.com/olovalabs/shob) at 2026-11-19 (README, manifest, release workflow, changelog; not its source code) |

## 1. Aim

Glasscode works, but only for people who build it themselves. This plan makes it something a
person can download, install, keep current, and trust. Part A is the baseline and comes first.
Parts B to D follow in that order. UI bugs are a separate list (section 7).

## 2. What exists today

| Area | State |
|---|---|
| Windows installer | `script/bundle-windows.ps1` makes `Glasscode-<version>-setup.exe` (Inno Setup, per user, no admin rights). Built by hand and by the CI job "Windows installer", kept 30 days as a workflow artifact. Never run on a clean computer. |
| Signing of the programs | The script and the CI job sign when a certificate is given (`SIGN_CERT`). No certificate exists, so everything is unsigned and SmartScreen warns. |
| Update code | `glasscode_core::update` is mostly written: a release is a manifest (product, build, files with SHA-256) signed with an Ed25519 key; the public key is built in; `windows_app` and `mac_app` can install a staged release at restart or quit and start again. `update_ui.rs` has the "Restart to update" strip. |
| What is missing in the update code | A place to ask. `update::newer` always answers "no update" and `stage` fails with "auto-update is not configured in this build", because there is no release server. |
| Version | `0.1.0` in every crate; a build number from the commit count (`GLASSCODE_BUILD`). No tag, no GitHub release, no changelog. |
| Project files | `LICENSE`, `NOTICE`, `README.md`, `CONTRIBUTING.md`. No `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.md`, issue or pull request templates. |
| Licence | Apache-2.0 change is prepared on the local branch `licence-apache`, not pushed: it waits for the agreement of every contributor. |

## 3. What Shob does that is worth knowing

| Shob | Take it? | Why |
|---|---|---|
| A release workflow started by a `v*` tag: each platform builds, checks that the tag matches the Cargo version, and uploads its files with `.sha256` files to a GitHub release. A manual run builds everything without uploading. | Yes (A2) | Simple, free, and GitHub is the download server. |
| The Windows packages run as a job of their own that needs only the Windows build, so a queued macOS runner cannot hold the Windows release back. | Yes (A2) | We have no macOS runner at all. |
| Updates: checks at launch; `shob update` from the shell; Settings, Updates in the app. | Partly (A3) | We keep our own, stricter design: a signed manifest. Shob trusts the download plus a checksum from the same place. |
| Binary deltas between releases, with a fall-back to the full file. | Later (D) | Our programs are 40 MB; the installer is 12 MB. Not worth the work for a first release. |
| `CHANGELOG.md` in the "Keep a Changelog" form, `SECURITY.md`, `CODE_OF_CONDUCT.md`. | Yes (A4) | A sponsor looks for these. |
| `UPSTREAM.md` for each imported part: source, commit, what was removed. | Yes (A4) | We have it for GPUI in `vendor/README.md`; make it the rule. |
| Linux packages (AppImage, deb, rpm, Arch). | Later (D) | Needs the Linux port of the app first. |
| More agents (Pi, OpenCode, and others) beside Claude and Codex. | Later (C) | Useful, but each agent needs its own status reading. |
| Split terminal panes. | Later (C) | A real feature gap; a layout change. |
| A built-in agent, vendored from another project. | No | `manager-v2` deliberately uses the official `claude` and `codex` programs as the brain. |
| Zed's file icons. | No | They are GPL-3.0 and would end the Apache-2.0 licence of the distributed app. |
| The `gpui-component` library. | No | Our components exist and match the design. |

## 4. Part A: the release and update baseline

Five slices. A1 to A3 depend on each other in that order; A4 and A5 can run beside them.

### A1 Version, build number and changelog (S)

- One version for all crates through `[workspace.package]`, so a release changes one line.
- `CHANGELOG.md` with an "Unreleased" section. Each pull request that changes behaviour adds a line.
- Settings, Updates and `glasscode --version` show the version and the build number (they partly do).
- **Done when:** the version is in one place, and the installer, the app and the CLI show the same one.

### A2 Release workflow (M)

- New `.github/workflows/release.yml`, started by a tag `v<version>` and by hand.
  - It fails when the tag is not the Cargo version.
  - Windows job: `script/bundle-windows.ps1`, then the setup file, a zip of the folder
    `target\release\Glasscode`, and a `.sha256` for each.
  - It makes the signed manifest of A3 and uploads everything to a **draft** GitHub release. A person publishes it.
  - A manual run builds everything and uploads nothing (a smoke test).
- The macOS build stays outside the workflow until a runner exists: the friend who builds the Mac
  version adds the Mac files to the same draft release with a script (`script/bundle-mac.sh` plus an upload step).
- **Done when:** pushing a tag gives a draft release with the Windows files, and the manual run passes.
- **Decision needed (D1):** is the repository public before the first release? A private
  repository's releases need a signed-in download, which changes A3.

### A3 Updates from GitHub releases (M)

The existing design stays: the app installs only what the release key signed.

- `update::newer` asks the GitHub releases of the repository for the latest published release,
  downloads `manifest.json` and `manifest.sig`, and checks the signature against the built-in key.
  It answers "no update" for a development build (no build number), as today.
- `windows_app::stage` downloads the file the manifest names, checks its size and SHA-256, and
  unpacks it into the updates folder. `install` and `relaunch_after_exit` exist.
- An installed copy (Inno Setup) and a portable copy (the zip) both update by replacing the
  program files; the entry in "Installed apps" gets the new version number.
- The release key: `update::generate_key` exists. The private key is a repository secret used
  only by the release workflow; the public key is `RELEASE_KEY_1`. Write down how to replace a lost key.
- Settings, Updates: the version, "Check now", the last check, and the switch for the automatic
  check. The "Restart to update" strip exists.
- It never installs while sessions run without asking: that rule exists and stays.
- **Tests:** a manifest with a wrong signature, a wrong hash, a wrong size, an older build, and a
  file for another system are each refused; an update is staged and installed in a temporary
  folder.
- **Done when:** version N installed from the setup file offers and installs version N+1 that the workflow published.
- **Decision needed (D2):** check automatically at start (as Shob), or only on "Check now" for the first releases?

### A4 Project files (S)

- `SECURITY.md` (how to report, which versions get fixes), `CODE_OF_CONDUCT.md`, issue and pull
  request templates.
- A third-party notice list made by `cargo about`, shipped in the installer.
- The rule for imported code: each imported part has a note with its source, commit and licence.
- README: a download section that points at the releases, and the screenshots of the new interface (still the old ones).
- **Needs first:** the licence decision (the `licence-apache` branch and the contributors' agreement).

### A5 The installer on a clean computer (S, by a person)

- Run the installer on a Windows computer that never had Glasscode or Rust: install, start,
  upgrade over an older version, uninstall. The steps are in `docs/windows-acceptance.md`.
- **Decision needed (D3):** a code signing certificate. Without one, every download warns
  ("Windows protected your PC"). Options: none for now; a paid certificate; a signing service for
  open-source projects. The script and the workflow are ready for a certificate.

## 4a. Part A: what is done, and what is not

| Slice | Done | Not done, and why |
|---|---|---|
| Licence | `LICENSE` is Apache-2.0; `NOTICE` names GPUI, the fonts and the packages; every manifest says Apache-2.0. The 22 crates in `vendor/` were compared with Zed at the copied commit: each is Apache-2.0 there. `about.toml` lists the licences a package may have, and the release build stops at another one. The installer and the zip carry the licence texts. | The sentence "takes no code from the GPL parts of Zed" rests on a search of our crates for attributions and on `docs/architecture.md`, not on a comparison of the code: the people who wrote the terminal, the editor and the diff confirm it. GitHub shows "GPL-3.0" until the change is on `main`. |
| A1 | One version in `Cargo.toml` (`[workspace.package]`); `CHANGELOG.md`; `glasscode --version` shows version and build. | |
| A2 | `.github/workflows/release.yml`; `script/bundle-windows.ps1` makes the zip and has `-Publish`; `docs/releasing.md`. The bundle script ran on the owner's computer. | The workflow never ran: it needs a push. Its Linux job (glasscoded with Docker) and the upload to a draft release are untested. |
| A3 | `update::newer`, `download`, `windows_app::stage`; the other files of an update are written over; "Installed apps" gets the new version; Settings › Updates has "Look for updates by itself" (on) and the time of the last check. Tests run the whole way against a web server on the same computer: publish, find, download, unpack, install, and each refusal. | No update from a real GitHub release: that needs a public repository, the release key and two published releases. Not seen on screen. The Mac app and glasscoded do not update (they ask nobody). |
| A4 | `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue and pull request templates, the rule for imported code in `CONTRIBUTING.md`, the notices from cargo-about. | The README's screenshots are still the old ones. GitHub's "private vulnerability reporting" must be switched on in the repository's settings, or `SECURITY.md` points at nothing. |
| A5 | | By a person: the installer on a computer that never had Glasscode. |

Decided while writing, to be confirmed: D2 is "automatic, with a switch" (what the code did before, now with the switch). D1, D3 and D5 are open. The release key: `RELEASE_KEY_1` came with the code; find out who has its private key, or make a new one (`docs/releasing.md`).

## 5. Part B: what is open in the app

Not new features. Things the redesign and the merge left open, which a first release should not carry.

| Item | From |
|---|---|
| The Board scroll fix is on `windows-port` only, not on `main`. | commit `4728f18` |
| About 14 Manager texts say "Mac", "⌘O" or "Keychain" on Windows; some point at "Settings › Automation" for rules that moved to Permissions. | merge of the Manager |
| Reads on the main thread: the Manager's voice-key lookup and its transcript check every 3 s; `control_ui` `is_tracked`/`exclude`; some command handlers. | U23, merge |
| A Manager walk changes the view, against the rule that background events never leave Today or Settings. | merge |
| The dots at the voice orb's glow. | U19, U23 |
| Today at 900 x 560 and 130 % cuts the date line. | U23 |
| The acceptance list (`docs/ui-redesign-acceptance.md`) was never gone through by a person. | U23 |
| The UI bugs of section 7. | owner |

## 6. Parts C and D: later

- **C, features:** more agents; split panes; search over session titles across projects.
- **D, reach:** Linux; binary deltas; a signed and notarized Mac release from a workflow when a runner exists.

`manager-v2` is its own plan (`docs/manager-v2-slices.md` on the branch `manager-v2`) and is not
part of this one. One point of contact: its slice M25 (export, backup, uninstall) and A3 both
touch what happens to data on an update or an uninstall; agree on that before either is built.

## 7. UI bugs

The owner adds them here one by one.

| # | Screen | What is wrong | State |
|---|---|---|---|
| 1 | Board | The columns did not scroll sideways with a mouse wheel. | Corrected on `windows-port` (`4728f18`); not seen on screen by the agent. |
| 2 | Today | Cards with many rows cut them ("2 more"); the layout was uneven; usage as thin bars; the start box did not stand out. | Corrected 2026-11-20: each card scrolls in itself, one gap and one card height in a row, usage as rings, a stronger start box. Seen on screenshots with made-up data, dark and light; not with real input. |
| 3 | Board | The toolbar wrapped without order; the ticket opened beside the board and squeezed it; the toolbar changed its width. | Corrected 2026-11-20: three groups of one height; the ticket is a panel over the page with a dimmed page under it. Clicks, the drag of the edge and the wheel were not tried. |
| 4 | Pull requests | The column stood at the left edge. | Corrected 2026-11-20: in the middle; a skeleton and a spinner while it loads. |
| 5 | Settings | One card for each setting. | Corrected 2026-11-20: rows in a few titled cards for each section. |
| 6 | Title bar | The Jump field moved with the page and the side panel; the breadcrumb said "Linear" on a GitHub board. | Corrected 2026-11-20: the field is in the middle of the window. |
| 7 | Everywhere | No motion at all. | `motion.rs` (2026-11-20): short one-time fades and slides, a skeleton and a spinner only while something loads, the switch knob slides. Nothing repeats in a steady state (the cost that made the redesign remove animations). The motion itself was not seen: the checks are still pictures. |

## 8. Decisions for the owners

| # | Question | Needed for |
|---|---|---|
| D1 | Is the repository public before the first release? | A2, A3 |
| D2 | Automatic update check at start, or only "Check now" at first? | A3 |
| D3 | Code signing certificate: none, paid, or an open-source signing service? | A5 |
| D4 | Do all contributors agree to Apache-2.0? | A4 |
| D5 | Does this work go into `main` through `windows-port` as before, and who merges? | all |
