#!/bin/zsh
# Builds "Kitchen Scale.app" into mac/dist. Run from anywhere: ./mac/bundle.sh
set -euo pipefail
cd "$(dirname "$0")"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

APP="dist/Kitchen Scale.app"
BIN=kitchen-scale
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

cargo build --release

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$BIN" "$APP/Contents/MacOS/$BIN"

# App icon (drawn by a small Swift script, then turned into .icns).
if [[ ! -f assets/AppIcon.icns ]]; then
  TMP=$(mktemp -d)
  swift assets/make_icon.swift "$TMP/icon.png"
  ICONSET="$TMP/AppIcon.iconset"; mkdir "$ICONSET"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$TMP/icon.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) "$TMP/icon.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o assets/AppIcon.icns
  rm -rf "$TMP"
fi
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kitchen Scale</string>
  <key>CFBundleDisplayName</key><string>Kitchen Scale</string>
  <key>CFBundleIdentifier</key><string>io.github.felixcriv.kitchenscale</string>
  <key>CFBundleExecutable</key><string>$BIN</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSBluetoothAlwaysUsageDescription</key>
  <string>Kitchen Scale connects to your Vitamix Bluetooth scale to show its weight.</string>
</dict>
</plist>
EOF

# Ad-hoc signature so macOS keeps the Bluetooth permission between launches.
codesign --force --deep --sign - "$APP"
echo "Built: $(pwd)/$APP"
