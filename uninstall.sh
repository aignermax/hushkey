#!/usr/bin/env bash
# Remove hushkey: autostart, menu entry and program. Settings and log stay.
set -euo pipefail
for exe in "$HOME/.local/bin/hushkey" "$HOME/Applications/hushkey.app/Contents/MacOS/hushkey"; do
  if [ -x "$exe" ]; then
    "$exe" --uninstall
    [ -d "$HOME/Applications/hushkey.app" ] && [ "${exe#"$HOME/Applications"}" != "$exe" ] && rm -rf "$HOME/Applications/hushkey.app"
    echo "hushkey removed"
    exit 0
  fi
done
echo "hushkey is not installed"
