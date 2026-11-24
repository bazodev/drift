#!/bin/sh
# Builds "Drift Audio.app", the small helper that macOS asks audio-capture permission for.
# The bundle is re-signed only when the binary changes, so the permission survives rebuilds.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/live/Drift Audio.app"

cargo build --quiet --bin drift-audio --manifest-path "$root/Cargo.toml"
mkdir -p "$app/Contents/MacOS"

if cmp -s "$root/target/debug/drift-audio" "$app/Contents/MacOS/drift-audio"; then
    echo "$app"
    exit 0
fi

cp "$root/target/debug/drift-audio" "$app/Contents/MacOS/drift-audio"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.drift.audio</string>
    <key>CFBundleName</key><string>Drift Audio</string>
    <key>CFBundleExecutable</key><string>drift-audio</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSUIElement</key><true/>
    <key>NSAudioCaptureUsageDescription</key>
    <string>Drift listens only to its own sound, so its animations can move with the music. Nothing is recorded or sent anywhere.</string>
</dict>
</plist>
PLIST
codesign --force --sign - --identifier dev.drift.audio "$app" 2>/dev/null
echo "$app"
