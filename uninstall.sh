#!/bin/bash
# Stops and removes mxmasterd. Pass --reset-permissions to also clear its
# Input Monitoring / Accessibility entries (only for this app's bundle ID).
set -euo pipefail

LABEL=io.github.danew.mxmaster
APP="$HOME/Applications/MXMaster.app"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"

# The daemon undiverts the gesture button when stopped; the mouse is stock
# afterwards (diversion is volatile anyway).
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$AGENT"
rm -rf "$APP"
echo "Removed $APP and $AGENT (log kept at ~/Library/Logs/mxmasterd.log)."

if [ "${1:-}" = "--reset-permissions" ]; then
    tccutil reset ListenEvent "$LABEL" || true
    tccutil reset Accessibility "$LABEL" || true
    tccutil reset PostEvent "$LABEL" || true
    echo "Reset permissions for $LABEL."
fi
