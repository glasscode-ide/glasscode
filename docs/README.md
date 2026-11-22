# Glasscode documentation

Start with the [README](../README.md) for what Glasscode is, how to build it and how to use it.

## Guides

| Guide | For |
|---|---|
| [manager.md](manager.md) | The manager: switching it on, what it does, its levels, what it cannot do, its data, the messenger program |
| [CONNECTIVITY.md](CONNECTIVITY.md) | Connecting the Mac, your server and the glasses app over Tailscale or the relay |
| [windows.md](windows.md) | Glasscode on Windows: build, run, the installer, where data lives, shortcuts, what is not ported |
| [architecture.md](architecture.md) | How Glasscode is built: processes, the window, the `glasscode` command, sessions, the manager, the server, sync, the glasses app |
| [releasing.md](releasing.md) | For maintainers: making a release, the release key, how installed copies update |
| [../relay/README.md](../relay/README.md) | The optional self-hosted relay |
| [../voice/README.md](../voice/README.md) | The manager's voice: the Mac helper and the voice room |
| [../glasses/README.md](../glasses/README.md) | The iPhone app for Meta Ray-Ban Display glasses |

## Design records

Glasscode was built in planned slices, one pull request each, from a written plan. These are
those plans, the measurements that chose between designs, and the
acceptance lists, kept as they were written. They explain why the code is the way it is, and the
code's comments refer to their decision numbers (D17, M11, U02 and so on). Where a record and
the code disagree, the code and the guides above are right.

| Record | What it holds |
|---|---|
| [manager-v2-design.md](manager-v2-design.md) | Manager v2: principles, decisions D1 to D30, interfaces |
| [manager-v2-slices.md](manager-v2-slices.md) | Manager v2: the slice plan (M00 to M62) |
| [manager-v2-proofs.md](manager-v2-proofs.md) | Manager v2: what was measured against the real CLIs before building (`script/manager-v2-proofs/`) |
| [manager-v2-acceptance.md](manager-v2-acceptance.md) | Manager v2: the acceptance list |
| [ui-redesign-slices.md](ui-redesign-slices.md) | The interface redesign: the slice plan (U01 to U25) and a result note for each |
| [ui-redesign-mock.html](ui-redesign-mock.html) | The interactive mock of the screens (open it in a browser) |
| [ui-redesign-proofs.md](ui-redesign-proofs.md) | The measurements that chose the backdrop, the glass and the scale |
| [ui-redesign-acceptance.md](ui-redesign-acceptance.md) | The interface: the list to check each screen, and the known limits |
| [windows-port-plan.html](windows-port-plan.html) | The Windows port: findings and design decisions (open it in a browser) |
| [windows-port-slices.html](windows-port-slices.html) | The Windows port: the 19 slices and their rules |
| [windows-acceptance.md](windows-acceptance.md) | The Windows port: what was checked for release 1 |
| [mobile-glasses-plan.md](mobile-glasses-plan.md) | The glasses app: the plan (G00 to G19) |
| [server-sync-plan.md](server-sync-plan.md) | Moving sessions between the Mac and the server, and keeping them in sync |
| [maturity-plan.md](maturity-plan.md) | Releases, updates and the path to a first public release |

## Screenshots

[screenshots/](screenshots/) is for pictures of the app. Take them from a demo home with made-up
projects (`script/journeys.sh` makes one); keep every screenshot free of real projects, tickets
and personal data.
