#!/usr/bin/env bash
# whisper-ptt installer: venv + dependencies + systemd user service.
# Idempotent — safe to re-run (e.g. after git pull).
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$DIR/.venv"

echo "==> checking prerequisites"
command -v python3 >/dev/null || { echo "ERROR: python3 missing"; exit 1; }
command -v pw-record >/dev/null || echo "WARNING: pw-record missing (install pipewire) — recording will fail"
if [ "${XDG_SESSION_TYPE:-x11}" != "x11" ]; then
  echo "WARNING: session is '${XDG_SESSION_TYPE}' — dictation needs X11 (Wayland unsupported)"
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
