#!/usr/bin/env bash
# Remove the whisper-ptt service/agent and (optionally) the venv.
# On Linux the udev rule and 'input' group membership are left alone — other
# tools may rely on them. --purge-system removes those too.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS="$(uname -s)"

if [ "$OS" = "Darwin" ]; then
  launchctl bootout "gui/$(id -u)/com.whisper-ptt" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/com.whisper-ptt.plist"
  echo "launchd agent removed"
else
  UNIT_DIR="$HOME/.config/systemd/user"
  for unit in whisper-ptt.service ydotoold.service; do
    systemctl --user disable --now "$unit" 2>/dev/null || true
    rm -f "$UNIT_DIR/$unit"
  done
  systemctl --user daemon-reload
  echo "services removed"
fi

for arg in "$@"; do
  case "$arg" in
    --purge)
      rm -rf "$DIR/.venv"
      echo "venv removed"
      ;;
    --purge-system)
      if [ "$OS" = "Darwin" ]; then
        echo "--purge-system is Linux-only (no udev rule or 'input' group here)"
        continue
      fi
      SUDO=""
      if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null; then SUDO="sudo"; fi
      $SUDO rm -f /etc/udev/rules.d/99-whisper-ptt-uinput.rules
      $SUDO udevadm control --reload-rules
      $SUDO gpasswd -d "$USER" input 2>/dev/null || true
      echo "udev rule and 'input' group membership removed (log out to apply)"
      ;;
  esac
done
