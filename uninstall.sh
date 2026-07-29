#!/usr/bin/env bash
# Remove the whisper-ptt service/agent and (optionally) the venv.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS="$(uname -s)"

if [ "$OS" = "Darwin" ]; then
  launchctl bootout "gui/$(id -u)/com.whisper-ptt" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.whisper-ptt.plist"
  echo "launchd agent removed"
else
  systemctl --user disable --now whisper-ptt.service 2>/dev/null || true
  rm -f "$HOME/.config/systemd/user/whisper-ptt.service"
  systemctl --user daemon-reload
  echo "service removed"
fi
if [ "${1:-}" = "--purge" ]; then
  rm -rf "$DIR/.venv"
  echo "venv removed"
fi
