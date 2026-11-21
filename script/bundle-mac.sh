#!/usr/bin/env bash
# Build Glasscode.app (and a .dmg) for macOS.
#
#   script/bundle-mac.sh                 # ad-hoc signed, for local use
#   SIGN_IDENTITY="Developer ID Application: Your Co (TEAMID)" \
#   NOTARY_PROFILE=glasscode script/bundle-mac.sh   # signed + notarized for the team
#
# BUNDLE_ID defaults to dev.glasscode; set it to your company's reverse domain.
set -euo pipefail
cd "$(dirname "$0")/.."

BUNDLE_ID="${BUNDLE_ID:-dev.glasscode}"
# The version of the workspace ([workspace.package] in Cargo.toml).
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
# A release build's number (GLASSCODE_BUILD), else the commit count.
BUILD="${GLASSCODE_BUILD:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"

cargo build --release -p glasscode

APP="target/release/Glasscode.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/glasscode "$APP/Contents/MacOS/Glasscode"
cp crates/glasscode/resources/Glasscode.icns "$APP/Contents/Resources/Glasscode.icns"

# The manager's voice helper (Swift + LiveKit): its binary into Resources, its two LiveKit
# frameworks into Frameworks, and the helper's rpath pointed at them.
echo "building manager-voice helper…"
( cd voice/helper && swift build -c release --product manager-voice )
MANAGER_VOICE_BIN="voice/helper/.build/release/manager-voice"
HELPER_PRODUCTS="voice/helper/.build/out/Products/Release"
if [[ -f "$MANAGER_VOICE_BIN" ]]; then
  mkdir -p "$APP/Contents/Frameworks"
  for fw in LiveKitWebRTC RustLiveKitFFI; do
    if [[ -d "$HELPER_PRODUCTS/$fw.framework" ]]; then
      cp -R "$HELPER_PRODUCTS/$fw.framework" "$APP/Contents/Frameworks/"
    else
      echo "warning: $fw.framework missing in $HELPER_PRODUCTS; the manager's voice won't run" >&2
    fi
  done
  cp "$MANAGER_VOICE_BIN" "$APP/Contents/Resources/manager-voice"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP/Contents/Resources/manager-voice" 2>/dev/null || true
else
  echo "warning: manager-voice helper not built ($MANAGER_VOICE_BIN); the manager's voice won't run" >&2
fi
# The voice on this Mac (Settings › Manager › Voice › On this Mac): LiveKit's server and the
# voice worker's Python files. livekit-server: $LIVEKIT_SERVER_BIN, else one on the PATH, else
# Homebrew's build of the version the worker is written against (fetched, not installed).
LIVEKIT_VERSION="1.13.11"
LIVEKIT_BIN="${LIVEKIT_SERVER_BIN:-$(command -v livekit-server || true)}"
if [[ -z "$LIVEKIT_BIN" ]] && command -v brew >/dev/null 2>&1; then
  if HOMEBREW_NO_AUTO_UPDATE=1 brew fetch -q livekit >/dev/null 2>&1; then
    LIVEKIT_STAGE="target/livekit"
    rm -rf "$LIVEKIT_STAGE" && mkdir -p "$LIVEKIT_STAGE"
    tar -xzf "$(brew --cache livekit)" -C "$LIVEKIT_STAGE"
    LIVEKIT_BIN="$(find "$LIVEKIT_STAGE" -type f -name livekit-server | head -1)"
  fi
fi
if [[ -n "$LIVEKIT_BIN" && -x "$LIVEKIT_BIN" ]]; then
  cp "$LIVEKIT_BIN" "$APP/Contents/Resources/livekit-server"
  chmod 755 "$APP/Contents/Resources/livekit-server"
  LIVEKIT_LICENSE="$(dirname "$LIVEKIT_BIN")/../LICENSE"
  mkdir -p "$APP/Contents/Resources/licenses"
  if [[ -f "$LIVEKIT_LICENSE" ]]; then cp "$LIVEKIT_LICENSE" "$APP/Contents/Resources/licenses/LIVEKIT-LICENSE.txt"; fi
  echo "livekit-server: $("$LIVEKIT_BIN" --version 2>/dev/null) (written against $LIVEKIT_VERSION)"
else
  echo "warning: no livekit-server (brew install livekit, or LIVEKIT_SERVER_BIN=…); the voice on this Mac won't run" >&2
fi
mkdir -p "$APP/Contents/Resources/voice-worker"
cp voice/manager/worker/*.py voice/manager/worker/requirements.txt "$APP/Contents/Resources/voice-worker/"
rm -f "$APP/Contents/Resources/voice-worker/test_"*.py
# Linux builds of the server daemon, installed on a developer's server from the Server dialog.
# Built with Docker by script/build-glasscoded.sh; skipped (with a warning) when missing.
if ls target/glasscoded/glasscoded-linux-* >/dev/null 2>&1; then
  mkdir -p "$APP/Contents/Resources/glasscoded"
  cp target/glasscoded/glasscoded-linux-* "$APP/Contents/Resources/glasscoded/"
else
  echo "warning: no glasscoded Linux builds (run script/build-glasscoded.sh); the app cannot install the server daemon" >&2
fi
# The licence texts go with the app (Apache-2.0, section 4; the fonts' OFL). The licences of the
# Rust packages are THIRD-PARTY-NOTICES.html, when cargo-about is installed.
mkdir -p "$APP/Contents/Resources/licenses/fonts"
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
cp NOTICE "$APP/Contents/Resources/NOTICE.txt"
cp crates/glasscode/assets/fonts/*.txt "$APP/Contents/Resources/licenses/fonts/"
cp vendor/LICENSE-APACHE "$APP/Contents/Resources/licenses/GPUI-LICENSE-APACHE.txt"
if cargo about --version >/dev/null 2>&1; then
  # What the app has: the Mac app, and the server daemon for Linux.
  cargo about generate --locked --fail --workspace --target aarch64-apple-darwin --target x86_64-apple-darwin --target x86_64-unknown-linux-gnu --target aarch64-unknown-linux-gnu -o "$APP/Contents/Resources/THIRD-PARTY-NOTICES.html" about.hbs
else
  echo "warning: cargo-about is not installed; the app has no THIRD-PARTY-NOTICES.html (cargo install --locked cargo-about --features cli)" >&2
fi
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Glasscode</string>
  <key>CFBundleDisplayName</key><string>Glasscode</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>Glasscode</string>
  <key>CFBundleIconFile</key><string>Glasscode</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${BUILD}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <!-- macOS asks the app that started a program for privacy permissions. Without these texts
       it silently refuses, e.g. Claude Code's /voice records silence. -->
  <key>NSMicrophoneUsageDescription</key><string>A program in a Glasscode terminal, such as Claude Code's voice mode, wants to use the microphone.</string>
  <key>NSSpeechRecognitionUsageDescription</key><string>Glasscode, the voice copilot, listens on-device for “Hey Glasscode” to wake up.</string>
  <key>NSCameraUsageDescription</key><string>A program in a Glasscode terminal wants to use the camera.</string>
  <key>NSAppleEventsUsageDescription</key><string>A program in a Glasscode terminal wants to control another app.</string>
</dict>
</plist>
PLIST

ENTITLEMENTS="script/Glasscode.entitlements"
# Nested code (the LiveKit frameworks and the voice helpers) must be signed before the app.
# The helpers get the same entitlements so their microphone use is covered.
SIGN_ID="${SIGN_IDENTITY:--}" # '-' = ad-hoc
RUNTIME_FLAGS=()
[[ -n "${SIGN_IDENTITY:-}" ]] && RUNTIME_FLAGS=(--options runtime --timestamp)
# Guard empty-array expansion for macOS bash 3.2 under `set -u`.
if [[ -d "$APP/Contents/Frameworks" ]]; then
  for fw in "$APP"/Contents/Frameworks/*.framework; do
    [[ -e "$fw" ]] && codesign --force ${RUNTIME_FLAGS[@]+"${RUNTIME_FLAGS[@]}"} --sign "$SIGN_ID" "$fw"
  done
fi
if [[ -f "$APP/Contents/Resources/livekit-server" ]]; then
  codesign --force ${RUNTIME_FLAGS[@]+"${RUNTIME_FLAGS[@]}"} --sign "$SIGN_ID" "$APP/Contents/Resources/livekit-server"
fi
for helper in manager-voice; do
  if [[ -f "$APP/Contents/Resources/$helper" ]]; then
    codesign --force ${RUNTIME_FLAGS[@]+"${RUNTIME_FLAGS[@]}"} --entitlements "$ENTITLEMENTS" --sign "$SIGN_ID" "$APP/Contents/Resources/$helper"
  fi
done
if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" --sign "$SIGN_IDENTITY" "$APP"
else
  # Ad-hoc: macOS ties privacy permissions to this exact build, so it asks again after each
  # update. Developer ID builds keep them.
  codesign --force --entitlements "$ENTITLEMENTS" --sign - "$APP"
fi

DMG="target/release/Glasscode-${VERSION}.dmg"
rm -f "$DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "Glasscode" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
  if [[ -n "${NOTARY_PROFILE:-}" ]]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
  fi
fi

echo "Built $APP and $DMG"
