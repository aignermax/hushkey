#!/usr/bin/env bash
# A dpkg-owned desktop entry also completes deferred, per-user setup.
set -euo pipefail
PAYLOAD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$HOME/.local/share/whisper-ptt"
if ! cmp -s "$PAYLOAD/.release-version" "$DEST/.setup-version"; then
  LOG="$HOME/.local/state/whisper-ptt/install.log"
  mkdir -p "$(dirname "$LOG")"
  if ! pkexec /bin/sh "$PAYLOAD/package-setup" >"$LOG" 2>&1; then
    if command -v zenity >/dev/null; then
      zenity --error --text="hushkey setup failed. See $LOG for details."
    elif command -v notify-send >/dev/null; then
      notify-send hushkey "Setup failed. See $LOG for details."
    fi
    exit 1
  fi
fi
exec systemctl --user restart whisper-ptt.service
