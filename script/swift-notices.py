#!/usr/bin/env python3
"""The licence texts of the Swift packages that a built program carries, as one text file.

The Mac app ships the voice helper (LiveKit, WebRTC) and LiveKit's server; the glasses app ships
LiveKit, WebRTC, Swift NIO and Meta's toolkit. Apache-2.0 asks that a copy of each licence and the
contents of each NOTICE go with the program; BSD and MIT ask for the licence text. This reads the
packages and their exact revisions from a Package.resolved, fetches each one's LICENSE and NOTICE
at that revision from GitHub, and writes them in order. The output is committed, so a build never
needs the network for it. Run it again whenever a Package.resolved changes:

  script/swift-notices.py mac      # voice/helper/THIRD-PARTY-NOTICES.txt
  script/swift-notices.py glasses  # glasses/GlasscodeGlasses/Sources/Licenses/ThirdPartyNotices.txt
"""
import json
import pathlib
import sys
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
GLASSES_RESOLVED = "glasses/GlasscodeGlasses/GlasscodeGlasses.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved"
TARGETS = {
    "mac": ("voice/helper/Package.resolved", "voice/helper/THIRD-PARTY-NOTICES.txt",
            "The voice helper (manager-voice) and LiveKit's server, as the Mac app carries them."),
    "glasses": (GLASSES_RESOLVED, "glasses/GlasscodeGlasses/Sources/Licenses/ThirdPartyNotices.txt",
                "The packages the Glasscode glasses app is built with."),
}
LICENCE_NAMES = ["LICENSE", "LICENSE.txt", "LICENSE.md", "COPYING"]
NOTICE_NAMES = ["NOTICE", "NOTICE.txt", "NOTICE.md"]
# The WebRTC build LiveKit packages: its own licence travels inside the binary, not in the
# package's repository (whose LICENSE, MIT, covers LiveKit's packaging).
WEBRTC = ("WebRTC (inside LiveKitWebRTC)", "https://webrtc.googlesource.com/src",
          "https://webrtc.googlesource.com/src/+/refs/heads/main/LICENSE?format=TEXT")
# LiveKit's server, which the Mac app bundles, at the version script/bundle-mac.sh names.
LIVEKIT_SERVER_VERSION = next(line.split('"')[1] for line in (ROOT / "script/bundle-mac.sh").read_text().splitlines()
                              if line.startswith("LIVEKIT_VERSION="))
LIVEKIT_SERVER = ("livekit-server", "https://github.com/livekit/livekit", f"v{LIVEKIT_SERVER_VERSION}")
# Not open source: cite its terms, do not reproduce them.
TERMS_ONLY = {
    "meta-wearables-dat-ios": "Meta Wearables Device Access Toolkit. Used under the Meta Wearables Developer "
                              "Terms (https://wearables.developer.meta.com/terms) and the Acceptable Use Policy "
                              "(https://wearables.developer.meta.com/acceptable-use-policy). Not covered by "
                              "Glasscode's licence.",
}


def get(url):
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "glasscode-notices"}), timeout=30) as r:
            return r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def first(owner_repo, ref, names):
    for name in names:
        text = get(f"https://raw.githubusercontent.com/{owner_repo}/{ref}/{name}")
        if text is not None:
            return name, text
    return None, None


def section(title, source, body):
    bar = "=" * 80
    return f"{bar}\n{title}\n{source}\n{bar}\n\n{body.strip()}\n\n"


def package(owner_repo, ref, label, url):
    out = []
    lic_name, lic = first(owner_repo, ref, LICENCE_NAMES)
    if lic is None:
        sys.exit(f"no licence file in {owner_repo} at {ref}")
    out.append(section(f"{label}: {lic_name}", url, lic))
    notice_name, notice = first(owner_repo, ref, NOTICE_NAMES)
    if notice is not None:
        out.append(section(f"{label}: {notice_name}", url, notice))
    return out


def main(which):
    resolved, output, about = TARGETS[which]
    pins = json.loads((ROOT / resolved).read_text())["pins"]
    parts = [f"THIRD-PARTY NOTICES\n\n{about}\nEach package's licence, and its NOTICE where it has one, at the exact\n"
             f"version built (from {resolved}).\nWritten by script/swift-notices.py.\n\n"]
    for pin in sorted(pins, key=lambda p: p["identity"]):
        url = pin["location"].removesuffix(".git")
        owner_repo = url.split("github.com/")[1]
        version = pin["state"].get("version") or pin["state"]["revision"][:12]
        label = f"{pin['identity']} {version}"
        if pin["identity"] in TERMS_ONLY:
            parts.append(section(label, url, TERMS_ONLY[pin["identity"]]))
            continue
        parts += package(owner_repo, pin["state"]["revision"], label, url)
        if pin["identity"] == "webrtc-xcframework":
            import base64
            text = base64.b64decode(get(WEBRTC[2])).decode()
            parts.append(section(f"{WEBRTC[0]}: LICENSE", WEBRTC[1], text))
    if which == "mac":
        name, url, tag = LIVEKIT_SERVER
        parts += package(url.split("github.com/")[1], tag, f"{name} {tag}", url)
    target = ROOT / output
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(parts))
    print(f"{output}: {len(parts) - 1} texts")


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in TARGETS:
        sys.exit(__doc__)
    main(sys.argv[1])
