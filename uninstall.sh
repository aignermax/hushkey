#!/usr/bin/env bash
# Remove the whisper-ptt service and (optionally) the venv.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

systemctl --user disable --now whisper-ptt.service 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/whisper-ptt.service"
systemctl --user daemon-reload
echo "service removed"
if [ "${1:-}" = "--purge" ]; then
  rm -rf "$DIR/.venv"
  echo "venv removed"
fi
