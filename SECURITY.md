# Security

## Reporting a vulnerability

Please do not open a public issue for a security problem.

Report it in private through GitHub: on the repository's **Security** tab, choose **Report a
vulnerability**. Only the maintainers see the report. Say what you found, how to reproduce it,
which version and build (Settings › Updates, or `glasscode --version`) and which system.

Please give us time to release a correction before you publish.

## Which versions get corrections

The newest release. Glasscode updates itself to it (Settings › Updates), so a correction is a new
release, not a patch for an older one.

## Updates

An installed Glasscode installs only a release whose manifest is signed with the Glasscode
release key, and only the files whose size and SHA-256 that manifest names
([`docs/releasing.md`](docs/releasing.md)). GitHub stores the files; it cannot change them
unnoticed.

## What is out of scope

- What Claude Code, Codex or another program does inside a session: report that to its maker.
- A problem that needs a person to run a changed Glasscode, or to change the files of their own
  account.
