#!/bin/sh
# Builds the stand-in phone of the device door's end-to-end test from the iPhone app's own link
# code, for macOS. Then: GLASSCODE_DOOR_E2E=<printed path> cargo test -p glasscode door_end_to_end
set -e
here="$(cd "$(dirname "$0")" && pwd)"
link="$here/../../glasses/GlasscodeGlasses/Sources/Link"
out="${1:-$here/.build/door-e2e}"
mkdir -p "$(dirname "$out")"
swiftc -O -o "$out" "$here/main.swift" "$link/DeviceKey.swift" "$link/LinkProtocol.swift" "$link/LineConnection.swift" "$link/Pairing.swift" "$link/DoorLink.swift"
echo "$out"
