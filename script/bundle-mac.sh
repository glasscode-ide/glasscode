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

# Glasscode's headless voice helper (Swift + ElevenLabs/LiveKit) — spawned when the moon goes
# live. Bundle the binary into Resources and its two LiveKit frameworks into Frameworks,
# then point the helper's rpath at them.
echo "building glasscode-voice helper…"
( cd voice/helper && swift build -c release )
HELPER_BIN="voice/helper/.build/release/glasscode-voice"
HELPER_PRODUCTS="voice/helper/.build/out/Products/Release"
if [[ -f "$HELPER_BIN" ]]; then
  mkdir -p "$APP/Contents/Frameworks"
  for fw in LiveKitWebRTC RustLiveKitFFI; do
    if [[ -d "$HELPER_PRODUCTS/$fw.framework" ]]; then
      cp -R "$HELPER_PRODUCTS/$fw.framework" "$APP/Contents/Frameworks/"
    else
      echo "warning: $fw.framework missing in $HELPER_PRODUCTS; Glasscode's voice won't run" >&2
    fi
  done
  cp "$HELPER_BIN" "$APP/Contents/Resources/glasscode-voice"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$APP/Contents/Resources/glasscode-voice" 2>/dev/null || true
else
  echo "warning: glasscode-voice helper not built ($HELPER_BIN); Glasscode's voice won't run" >&2
fi
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
# Nested code (the LiveKit frameworks and the voice helper) must be signed before the app.
# The helper gets the same entitlements so its microphone use is covered.
SIGN_ID="${SIGN_IDENTITY:--}" # '-' = ad-hoc
RUNTIME_FLAGS=()
[[ -n "${SIGN_IDENTITY:-}" ]] && RUNTIME_FLAGS=(--options runtime --timestamp)
# Guard empty-array expansion for macOS bash 3.2 under `set -u`.
if [[ -d "$APP/Contents/Frameworks" ]]; then
  for fw in "$APP"/Contents/Frameworks/*.framework; do
    [[ -e "$fw" ]] && codesign --force ${RUNTIME_FLAGS[@]+"${RUNTIME_FLAGS[@]}"} --sign "$SIGN_ID" "$fw"
  done
fi
if [[ -f "$APP/Contents/Resources/glasscode-voice" ]]; then
  codesign --force ${RUNTIME_FLAGS[@]+"${RUNTIME_FLAGS[@]}"} --entitlements "$ENTITLEMENTS" --sign "$SIGN_ID" "$APP/Contents/Resources/glasscode-voice"
fi
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
