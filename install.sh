#!/usr/bin/env bash
# whisper-ptt installer (Linux + macOS): venv + dependencies + autostart service.
# Idempotent — safe to re-run (e.g. after git pull). On Windows use install.ps1.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$DIR/.venv"
OS="$(uname -s)"

echo "==> checking prerequisites"
command -v python3 >/dev/null || { echo "ERROR: python3 missing"; exit 1; }
if [ "$OS" = "Linux" ]; then
  command -v pw-record >/dev/null || echo "WARNING: pw-record missing (install pipewire) — recording falls back to sounddevice (needs libportaudio2)"
  if [ "${XDG_SESSION_TYPE:-x11}" != "x11" ]; then
    echo "WARNING: session is '${XDG_SESSION_TYPE}' — dictation needs X11 (Wayland unsupported)"
  fi
elif [ "$OS" = "Darwin" ]; then
  echo "note: on macOS, grant your terminal Microphone + Accessibility/Input Monitoring when prompted"
else
  echo "ERROR: unsupported OS '$OS' — on Windows use install.ps1"
  exit 1
fi

echo "==> creating venv at $VENV"
python3 -m venv "$VENV" 2>/dev/null || {
  echo "ERROR: python3-venv missing. Install it, e.g.: sudo apt install python3-venv"
  exit 1
}
"$VENV/bin/pip" -q install --upgrade pip

echo "==> installing python dependencies"
"$VENV/bin/pip" -q install -r "$DIR/requirements.txt"
if command -v nvidia-smi >/dev/null; then
  echo "==> NVIDIA GPU detected — installing CUDA libraries"
  "$VENV/bin/pip" -q install -r "$DIR/requirements-gpu.txt"
else
  echo "==> no NVIDIA GPU — CPU mode (works fine, just slower)"
fi

if [ "$OS" = "Darwin" ]; then
  echo "==> installing launchd agent"
  PLIST="$HOME/Library/LaunchAgents/com.whisper-ptt.plist"
  mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/whisper-ptt"
  sed -e "s|@DIR@|$DIR|g" -e "s|@HOME@|$HOME|g" \
    "$DIR/macos/com.whisper-ptt.plist.in" > "$PLIST"
  launchctl bootout "gui/$(id -u)/com.whisper-ptt" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"

  echo
  echo "Done. Hold Right Ctrl in any window, speak, release — text gets typed."
  echo "First dictation downloads the whisper model (~0.5-1.5 GB), then it's offline."
  echo "Logs:    ~/Library/Logs/whisper-ptt/ (dictate.log + launchd.log)"
  echo "Restart: launchctl kickstart -k gui/\$(id -u)/com.whisper-ptt"
else
  echo "==> installing systemd user service"
  mkdir -p "$HOME/.config/systemd/user"
  sed "s|@DIR@|$DIR|g" "$DIR/systemd/whisper-ptt.service.in" \
    > "$HOME/.config/systemd/user/whisper-ptt.service"
  systemctl --user daemon-reload
  systemctl --user enable --now whisper-ptt.service

  echo
  echo "Done. Hold Right Ctrl in any window, speak, release — text gets typed."
  echo "First dictation downloads the whisper model (~0.5-1.5 GB), then it's offline."
  echo "Logs: journalctl --user -u whisper-ptt.service -f"
fi
