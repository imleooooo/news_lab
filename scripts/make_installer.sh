#!/bin/bash
# Builds News Lab-<version>.pkg — double-click to install News Lab.app to /Applications

set -e

APP_NAME="News Lab"
BINARY_NAME="news_lab"
VERSION="0.1.7"
IDENTIFIER="com.news_lab"
PKG_NAME="${APP_NAME}-${VERSION}.pkg"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BINARY="$PROJECT_DIR/target/release/$BINARY_NAME"
ICNS="$PROJECT_DIR/resources/icon.icns"

# ── 1. Ensure release binary exists ───────────────────────────────────────────
if [ ! -f "$BINARY" ]; then
    echo "🔨  Binary not found — building release..."
    cd "$PROJECT_DIR"
    cargo build --release
fi

# ── 2. Generate icon if missing ────────────────────────────────────────────────
if [ ! -f "$ICNS" ]; then
    echo "🎨  Generating icon..."
    python3 "$SCRIPT_DIR/make_icon.py"
fi

# ── 3. Stage app bundle in temp dir ───────────────────────────────────────────
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

APP_PATH="$STAGING/$APP_NAME.app"
CONTENTS="$APP_PATH/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"
mkdir -p "$MACOS" "$RESOURCES_DIR"

cp "$BINARY" "$MACOS/$BINARY_NAME"
chmod +x "$MACOS/$BINARY_NAME"

cp "$ICNS" "$RESOURCES_DIR/icon.icns"

# Copy .env (search news-rs/ then news-app/)
ENV_SRC=""
if [ -f "$PROJECT_DIR/.env" ]; then
    ENV_SRC="$PROJECT_DIR/.env"
elif [ -f "$(dirname "$PROJECT_DIR")/.env" ]; then
    ENV_SRC="$(dirname "$PROJECT_DIR")/.env"
fi
if [ -n "$ENV_SRC" ]; then
    cp "$ENV_SRC" "$RESOURCES_DIR/.env"
    echo "🔑  Bundled .env from $ENV_SRC"
else
    echo "⚠️  No .env found — OPENAI_API_KEY must be set manually"
fi

# Launcher: sources bundled .env, opens Terminal, stays alive for Dock icon
cat > "$MACOS/launcher" << 'LAUNCHER'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/news_lab"
ENV_FILE="$DIR/../Resources/.env"
osascript -e "tell application \"Terminal\" to do script \"set -a; [ -f '$ENV_FILE' ] && source '$ENV_FILE'; set +a; '$BIN'; exit 0\""
# Keep launcher alive so Dock icon stays visible until news_lab exits
sleep 2
while pgrep -xq "news_lab"; do
    sleep 1
done
LAUNCHER
chmod +x "$MACOS/launcher"

# Info.plist
cat > "$CONTENTS/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>         <string>launcher</string>
  <key>CFBundleIdentifier</key>         <string>$IDENTIFIER</string>
  <key>CFBundleName</key>               <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>        <string>$APP_NAME</string>
  <key>CFBundleVersion</key>            <string>$VERSION</string>
  <key>CFBundleShortVersionString</key> <string>$VERSION</string>
  <key>CFBundlePackageType</key>        <string>APPL</string>
  <key>CFBundleIconFile</key>           <string>icon</string>
</dict>
</plist>
PLIST

# ── 4. Ad-hoc sign the app bundle ─────────────────────────────────────────────
echo "🔏  Ad-hoc signing..."
codesign --force --deep --sign - "$APP_PATH"

# ── 5. Build PKG ───────────────────────────────────────────────────────────────
echo "📦  Creating installer package..."
pkgbuild \
    --root "$STAGING" \
    --identifier "$IDENTIFIER" \
    --version "$VERSION" \
    --install-location "/Applications" \
    "$PROJECT_DIR/$PKG_NAME"

echo ""
echo "✅  Created: $PROJECT_DIR/$PKG_NAME"
echo ""
echo "   To install: double-click $PKG_NAME"
echo "   Installs News Lab.app → /Applications/News Lab.app"
echo ""
echo "   ⚠️  First launch: right-click → Open (Gatekeeper, ad-hoc signed)"
