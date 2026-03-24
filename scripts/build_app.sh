#!/bin/bash
# Creates ~/Applications/News Lab.app that double-clicks to open in Terminal

set -e

APP_NAME="News Lab"
BINARY_NAME="news_lab"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BINARY="$PROJECT_DIR/target/release/$BINARY_NAME"
ICNS="$PROJECT_DIR/resources/icon.icns"
APP_PATH="$HOME/Applications/$APP_NAME.app"
CONTENTS="$APP_PATH/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"

if [ ! -f "$BINARY" ]; then
    echo "❌  Binary not found: $BINARY"
    echo "   Run: cargo build --release"
    exit 1
fi

# Generate icon if missing
if [ ! -f "$ICNS" ]; then
    echo "🎨  Generating icon..."
    python3 "$SCRIPT_DIR/make_icon.py"
fi

echo "📦  Building $APP_NAME.app ..."

rm -rf "$APP_PATH"
mkdir -p "$MACOS" "$RESOURCES_DIR"

cp "$BINARY" "$MACOS/$BINARY_NAME"
chmod +x "$MACOS/$BINARY_NAME"

# Copy icon
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
PIDFILE="/tmp/com.news_lab.pid"

# Kill only our own stale instance — confirm the PID still belongs to news_lab
# before sending the signal, guarding against PID reuse by unrelated processes.
if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    if ps -p "$OLD_PID" -o comm= 2>/dev/null | grep -qx "news_lab"; then
        kill "$OLD_PID" 2>/dev/null || true
        sleep 0.5
    fi
    rm -f "$PIDFILE"
fi

# The Terminal shell writes its own PID before exec'ing into news_lab.
# exec replaces the shell with news_lab at the same PID, so the saved
# PID correctly identifies the news_lab process we own.
osascript -e "tell application \"Terminal\" to do script \"set -a; [ -f '$ENV_FILE' ] && source '$ENV_FILE'; set +a; echo \$\$ > '$PIDFILE'; exec '$BIN'\""

# Wait up to 10 s for our launched instance to write its PID
OUR_PID=""
for _i in $(seq 10); do
    sleep 1
    [ -f "$PIDFILE" ] && { OUR_PID=$(cat "$PIDFILE"); break; }
done

# Keep launcher alive until our specific news_lab instance exits,
# not any unrelated news_lab process that may be running elsewhere.
if [ -n "$OUR_PID" ]; then
    while ps -p "$OUR_PID" -o comm= 2>/dev/null | grep -qx "news_lab"; do
        sleep 1
    done
fi
rm -f "$PIDFILE"
LAUNCHER
chmod +x "$MACOS/launcher"

cat > "$CONTENTS/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>    <string>launcher</string>
  <key>CFBundleIdentifier</key>   <string>com.news_lab</string>
  <key>CFBundleName</key>         <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>  <string>$APP_NAME</string>
  <key>CFBundleVersion</key>      <string>0.1.7</string>
  <key>CFBundleShortVersionString</key> <string>0.1.7</string>
  <key>CFBundlePackageType</key>  <string>APPL</string>
  <key>CFBundleIconFile</key>     <string>icon</string>
</dict>
</plist>
PLIST

echo "🔏  Ad-hoc signing..."
codesign --force --deep --sign - "$APP_PATH"

echo "✅  Created: $APP_PATH"
echo "   → Drag to Dock or double-click in ~/Applications/"
echo "   → 首次使用：右鍵 → 打開（Gatekeeper ad-hoc 簽名）"
