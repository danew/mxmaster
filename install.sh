#!/bin/bash
# Builds MXMaster.app, installs it to ~/Applications and loads its LaunchAgent.
# Re-run to update. No root required.
set -euo pipefail

LABEL=io.github.danew.mxmaster
APP_NAME=MXMaster.app
APP_DIR="$HOME/Applications"
APP="$APP_DIR/$APP_NAME"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/mxmasterd.log"
IDENTITY="${SIGN_IDENTITY:-MXMaster Local Signing}"
DOMAIN="gui/$(id -u)"

cd "$(dirname "$0")"

if ! command -v cargo >/dev/null; then
    if command -v rustup >/dev/null; then
        PATH="$(dirname "$(rustup which cargo)"):$PATH"
    else
        echo "cargo not found; install Rust (https://rustup.rs)" >&2
        exit 1
    fi
fi

echo "==> Building"
cargo build --release --bin mxmasterd
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

echo "==> Assembling $APP_NAME $VERSION"
STAGE="target/$APP_NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS"
cp target/release/mxmasterd "$STAGE/Contents/MacOS/mxmasterd"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$STAGE/Contents/Info.plist"
plutil -lint -s "$STAGE/Contents/Info.plist"

# TCC keys permission grants on the code signature. A stable signing identity
# keeps Input Monitoring / Accessibility across rebuilds; an ad-hoc signature
# changes with every build and the grants have to be given again.
if security find-identity -p codesigning 2>/dev/null | grep -q "\"$IDENTITY\""; then
    echo "==> Signing with '$IDENTITY'"
    codesign --force --sign "$IDENTITY" --identifier "$LABEL" "$STAGE"
    SIGNED_STABLE=1
else
    echo "==> Signing ad-hoc (no '$IDENTITY' identity; see scripts/make-signing-identity.sh)"
    codesign --force --sign - --identifier "$LABEL" "$STAGE"
    SIGNED_STABLE=0
fi
codesign --verify --strict "$STAGE"

echo "==> Installing to $APP"
# Stopping the agent lets the running daemon undivert the gesture button.
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
mkdir -p "$APP_DIR" "$(dirname "$AGENT")" "$(dirname "$LOG")"
rm -rf "$APP"
ditto "$STAGE" "$APP"

sed -e "s|@APP@|$APP|g" -e "s|@LOG@|$LOG|g" packaging/$LABEL.plist > "$AGENT"
plutil -lint -s "$AGENT"

echo "==> Loading LaunchAgent $LABEL"
launchctl bootstrap "$DOMAIN" "$AGENT"

for p in /Library/LaunchAgents/com.logi.optionsplus.plist "$HOME/Library/LaunchAgents/com.logi.optionsplus.plist"; do
    if [ -e "$p" ]; then
        echo
        echo "WARNING: Logi Options+ is installed ($p)."
        echo "         It reconfigures the mouse at every login and conflicts with mxmasterd."
        echo "         Uninstall it, or stop it with: launchctl bootout $DOMAIN/com.logi.cp-dev-mgr"
    fi
done

cat <<EOF

Installed. Grant both permissions to "MXMaster" in
System Settings > Privacy & Security:
  - Input Monitoring  (talk to the mouse)
  - Accessibility     (switch Spaces)
macOS should prompt for both; if MXMaster is not listed, add $APP with "+".
After granting, restart the daemon:
  launchctl kickstart -k $DOMAIN/$LABEL

Log: $LOG
EOF
if [ "$SIGNED_STABLE" = 0 ]; then
    echo "Note: ad-hoc signed, so the permissions must be granted again after every reinstall."
fi
