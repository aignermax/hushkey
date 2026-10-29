#!/usr/bin/env bash
# hushkey installer for Linux and macOS: downloads the program from the latest
# GitHub release, checks it against SHA256SUMS, and lets it install itself
# (menu entry, start at login). An existing Python hushkey is taken over: its
# settings and history move along, and it is switched off.
#
# Usage:  bash install.sh [--pre] [--exe <file>]
#   or:   curl -fsSL https://raw.githubusercontent.com/aignermax/hushkey/master/install.sh | bash
#
# This is also the last step of the Python hushkey's own updater, which runs
# master's install.sh after an update.
set -euo pipefail

REPO="aignermax/hushkey"
PRE=0
EXE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pre) PRE=1 ;;
    --exe) EXE="$2"; shift ;;
    --no-autostart) ;;  # the Python installer's option; autostart is a tray setting now
    *) echo "ERROR: unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)          ASSET=hushkey-linux-x86_64 ;;
  Linux-aarch64)         ASSET=hushkey-linux-aarch64 ;;
  Darwin-arm64)          ASSET=hushkey-macos-arm64.zip ;;
  Darwin-x86_64)         ASSET=hushkey-macos-x86_64.zip ;;
  *) echo "ERROR: no hushkey build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -z "$EXE" ]; then
  if [ "$PRE" = 1 ]; then
    API="https://api.github.com/repos/$REPO/releases?per_page=30"
  else
    API="https://api.github.com/repos/$REPO/releases/latest"
  fi
  # The newest listed release with this system's asset (no JSON tool needed:
  # the download URLs name both the tag and the asset).
  URL="$(curl -fsSL -H "User-Agent: hushkey-installer" "$API" \
    | grep -o "https://github.com/$REPO/releases/download/[^\"]*/$ASSET\"" \
    | head -n1 | tr -d '"')" || true
  if [ -z "$URL" ]; then
    echo "ERROR: no release of $REPO carries $ASSET" >&2
    exit 1
  fi
  echo "==> downloading $URL"
  curl -fsSL -o "$TMP/$ASSET" "$URL"
  curl -fsSL -o "$TMP/SHA256SUMS" "${URL%/*}/SHA256SUMS"
  EXPECTED="$(awk -v a="$ASSET" '{n=$2; sub(/^\*/, "", n)} n == a {print tolower($1); exit}' "$TMP/SHA256SUMS")"
  ACTUAL="$(sha256 "$TMP/$ASSET")"
  if [ -z "$EXPECTED" ] || [ "$ACTUAL" != "$EXPECTED" ]; then
    echo "ERROR: $ASSET is damaged (hash $ACTUAL, expected ${EXPECTED:-none listed})" >&2
    exit 1
  fi
  EXE="$TMP/$ASSET"
fi

case "$ASSET" in
  *.zip)
    # macOS: the asset is hushkey.app, zipped.
    if [ "${EXE%.zip}" != "$EXE" ]; then
      ditto -x -k "$EXE" "$TMP/app"
      EXE="$TMP/app/hushkey.app/Contents/MacOS/hushkey"
    fi
    xattr -dr com.apple.quarantine "$(dirname "$(dirname "$(dirname "$EXE")")")" 2>/dev/null || true
    ;;
  *)
    chmod +x "$EXE"
    # Linux: the key is read from /dev/input and text is inserted through a
    # virtual keyboard (/dev/uinput); both need the 'input' group. The Python
    # hushkey set this up already; a new machine needs it once (sudo).
    RULE=/etc/udev/rules.d/99-whisper-ptt-uinput.rules
    if [ ! -f "$RULE" ] || ! id -nG | tr ' ' '\n' | grep -qx input; then
      echo "==> one-time setup: 'input' group and /dev/uinput access (asks for your password)"
      # The password comes from the terminal, also under `curl | bash`
      # (stdin is the script then); not at all without one (the Python
      # updater runs this unattended).
      if (: </dev/tty) 2>/dev/null && sudo -v </dev/tty; then
        printf '%s\n' 'KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
          | sudo tee "$RULE" >/dev/null
        sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uinput || true
        sudo usermod -aG input "$USER"
        echo "    NOTE: log out and back in once, so the 'input' group applies."
      else
        echo "    WARNING: skipped; hushkey will say what is missing when it starts." >&2
      fi
    fi
    ;;
esac

echo "==> installing"
"$EXE" --install --from-python
echo
echo "Done: hushkey runs (tray icon) and starts at login."
echo "Hold your key (Caps Lock unless you picked another), speak, release."
echo "Settings are in the tray menu. Remove with: ~/.local/bin/hushkey --uninstall (Linux),"
echo "  ~/Applications/hushkey.app/Contents/MacOS/hushkey --uninstall (macOS)"
