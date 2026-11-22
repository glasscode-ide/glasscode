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
- A session's finish is told at once: within seconds of a Claude or Codex turn ending, its dot
  turns orange when it asks you something (and the notification says the question), or blue,
  "finished", until you open it. A small model reads only the endings the rules cannot (it runs on
  the session's own account, without tools). The manager knows it too: when it sent the session a
  message, it says the answer in the chat and aloud.

### Changed

- Claude's reminder a minute after each turn ("Claude is waiting for your input") no longer turns
  the session orange or sends "needs you"; neither does its terminal bell. Messages to a finished
  session are typed in instead of waiting for you, and `session send` says "Sent" when it is typed
  in at once.
- Codex sessions on macOS show working, finished and needs-you as on Windows.
- A session's dot is green whenever it works: also for a turn another session's message began, a
  turn a Stop hook keeps going, a very short turn, and work going on after you answered a prompt
  (here, on the phone or the glasses). It stays green for 2 s after a turn ends, so it does not
  blink, and while a turn's work goes on by itself (up to 30 minutes).
- A message you send to a Claude session through the manager, its voice or `session send` goes in
  while the session works, as one typed in its chat does: Claude reads it at its next step, and
  several can be sent in a row. They no longer wait for the turn to end, nor 10 s after the one
  before. A long message is no longer left unsent in the session's box. Messages from another
  session, the CI fix and Codex sessions still wait for the turn to end.
- The phone's and the glasses' voice finds sessions with the Mac's search again (it searched
  nothing: the Mac refused it), and a place opened from the phone opens on the Mac.

- Glasscode is licensed under the Apache License 2.0.
- The interface is new: one shell with Today, Sessions, Board, Pull requests and Settings, the
  Jump field, themes, backdrops and a scale for the whole interface.

No version is released yet. The builds so far call themselves 0.1.0.
