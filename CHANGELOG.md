# Changelog

What changed in Glasscode, for the people who use it. The form is
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

A pull request that changes what a person sees or does adds a line under "Unreleased". A release
gives that section its version and date ([`docs/releasing.md`](docs/releasing.md)); its text
becomes the text of the GitHub release.

## [Unreleased]

### Added

- Windows: Glasscode updates itself from its GitHub releases. It looks every few hours (Settings ›
  Updates turns that off; "Check now" looks at once), downloads a newer release, checks it against
  the release key, and installs it at "Restart to update" or when it quits.
- Windows: each release has a zip beside the installer, a copy that needs no installation.
- The installer and the zip carry the licence texts: Glasscode's, the fonts', and those of every
  Rust package in the programs (`THIRD-PARTY-NOTICES.html`).
- `glasscode --version` shows the version beside the build number.
- The Board scrolls sideways with the mouse wheel, and a GitHub board shows all its columns.

### Changed

- Glasscode is licensed under the Apache License 2.0.
- The interface is new: one shell with Today, Sessions, Board, Pull requests and Settings, the
  Jump field, themes, backdrops and a scale for the whole interface.

### Fixed

- `Glasscode --version` and `Glasscode --help` print and exit, as `glasscode --version` does. They
  used to start a second copy of the app beside the one already running.

No version is released yet. The builds so far call themselves 0.1.0.
