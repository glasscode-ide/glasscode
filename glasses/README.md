# Glasscode for Meta Ray-Ban Display

An iPhone app for Meta Ray-Ban Display glasses with the Neural Band. You talk to your Glasscode
manager from the glasses, and follow your sessions on the lens: what needs you, what finished, a
session's question and its answer. The manager runs on your Mac, or on your server while the Mac
sleeps; the phone reaches it over your tailnet.

> **Status: experimental.** The app is built and tested against the toolkit's simulated glasses,
> and its link to the Mac is tested end to end; a full run on real glasses is still to come.
> Meta's band input works only in Developer Mode or a private beta channel, so this is not an
> App Store app.

## How it works

```
glasses ◀── DAT (Bluetooth) ──▶ iPhone app ── device link (tailnet) ──▶ Glasscode on the Mac
                                    │                                    (the device door)
                                    └──── SSH (tailnet) ────────────────▶ glasscoded on your server
```

- **Speech stays on the phone.** What you say is turned into text on the iPhone, the question goes
  to the manager, and the answer is spoken to the glasses with the phone's voices.
- **Pairing** pins keys at both ends: the phone makes its key in the Secure Enclave, and the Mac
  pins it. A phone may do less than you can in the app; a person-only action asks in the app.
- **The lens** shows cards; a pinch talks or picks, swipes move and open, left goes back.

The full design is in [`docs/architecture.md`](../docs/architecture.md#the-glasses-app), and the
setup of the tailnet and pairing in [`docs/CONNECTIVITY.md`](../docs/CONNECTIVITY.md).

## What you need

- An iPhone with iOS 18, and Xcode 16 or newer on a Mac
- Meta Ray-Ban Display glasses with the Neural Band, and Developer Mode switched on in the Meta
  AI app
- Tailscale on the Mac and the iPhone, signed in to the same account
- An Apple developer team to sign the app (a free personal team works, without push)

## Build and run

The Xcode project is generated from `GlasscodeGlasses/project.yml` with
[XcodeGen](https://github.com/yonaskolb/XcodeGen); the generated project is checked in too.

```sh
cd glasses/GlasscodeGlasses

# Tests against the toolkit's simulated glasses (no hardware needed)
xcodebuild test -project GlasscodeGlasses.xcodeproj -scheme GlasscodeGlasses \
  -destination 'platform=iOS Simulator,name=iPhone 17'

# On your iPhone: open the project in Xcode, choose your team under Signing, and run.
open GlasscodeGlasses.xcodeproj
```

Pass your team on the command line (`DEVELOPMENT_TEAM=<team id>`) or choose it in Xcode; do not
commit it. With a free personal team, push notifications are not available: build with an
entitlements file that has no `aps-environment`.

`MetaAppID` and `ClientToken` in `Info.plist` stay empty in Developer Mode. For a beta channel,
Meta's developer center gives you both; keep them out of the repository.

## Pair it

1. On the Mac: **Settings › Server › Glasses › Pair glasses**. A code shows for ten minutes.
2. On the iPhone: open Glasscode and scan the code.
3. The title bar's glasses icon shows a green dot while the glasses are connected.

## Tests

- Swift (`Tests/`): the link protocol and its signed texts, pairing links, the lens's moves,
  turn-taking and sentences, and the toolkit's simulated glasses.
- Rust: `script/glasses-e2e/build.sh` builds the app's own link code for macOS, and
  `GLASSCODE_DOOR_E2E=<binary> cargo test -p glasscode door_end_to_end` runs it against the Mac
  app's device door.
