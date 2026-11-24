#!/bin/sh
# Packs Drift into dist/Drift.app and an installer disk image, dist/Drift.dmg.
# The audio helper is copied as it is, so macOS keeps the capture permission it already has.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dist="$root/dist"
app="$dist/Drift.app"
helper="$root/target/live/Drift Audio.app"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

[ -d "$helper" ] || "$root/scripts/bundle-audio.sh" >/dev/null
cargo build --quiet --release --bin drift --manifest-path "$root/Cargo.toml"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$app/Contents/Helpers"
cp "$root/target/release/drift" "$app/Contents/MacOS/drift"
ditto "$helper" "$app/Contents/Helpers/Drift Audio.app"

# The icon in every size macOS asks for.
work=$(mktemp -d)
mkdir "$work/Drift.iconset"
# The square artwork: macOS cuts its own icon shape, and frames anything that is not square.
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/assets/icon-square.png" --out "$work/Drift.iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$root/assets/icon-square.png" --out "$work/Drift.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/Drift.iconset" -o "$app/Contents/Resources/Drift.icns"

# The identifier stays "drift", the name dev builds run under, so the signed-in session carries over.
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>drift</string>
    <key>CFBundleName</key><string>Drift</string>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>Drift</string>
    <key>CFBundleExecutable</key><string>drift</string>
    <key>CFBundleIconFile</key><string>Drift</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>14.2</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

xattr -cr "$app"
codesign --force --sign - --identifier drift "$app"

# A disk image with a shortcut to Applications, for the usual drag-to-install.
mkdir "$work/stage"
ditto "$app" "$work/stage/Drift.app"
ln -s /Applications "$work/stage/Applications"
rm -f "$dist/Drift.dmg"
hdiutil create -quiet -volname Drift -srcfolder "$work/stage" -ov -format UDZO "$dist/Drift.dmg"
rm -rf "$work"

echo "$app"
echo "$dist/Drift.dmg"
