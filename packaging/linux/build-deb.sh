#!/usr/bin/env bash
# Build hushkey_<version>_all.deb — needs only dpkg-deb (Debian/Ubuntu).
# Usage: packaging/linux/build-deb.sh 0.3.1 [output-dir]
set -euo pipefail

VER="${1:?usage: build-deb.sh <version> [output-dir]}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKG="hushkey_${VER}_all"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
DEST="$STAGE/$PKG"
mkdir -p "$DEST/opt/hushkey" "$DEST/DEBIAN" "$OUT"

# --- payload: exactly what install.sh needs at runtime ----------------------
for f in dictate.py tray.py recorder.py transcribe.py install.sh uninstall.sh \
         requirements.txt requirements-gpu.txt README.md; do
  cp "$ROOT/$f" "$DEST/opt/hushkey/"
done
mkdir -p "$DEST/opt/hushkey/assets" "$DEST/opt/hushkey/systemd" "$DEST/opt/hushkey/udev"
cp "$ROOT/assets/logo.png" "$DEST/opt/hushkey/assets/"
cp "$ROOT/systemd/"*.service.in "$DEST/opt/hushkey/systemd/"
cp "$ROOT/udev/"*.rules "$DEST/opt/hushkey/udev/"

# --- metadata ---------------------------------------------------------------
sed "s|@VERSION@|$VER|" "$ROOT/packaging/linux/control.in" > "$DEST/DEBIAN/control"
cp "$ROOT/packaging/linux/postinst" "$DEST/DEBIAN/postinst"
cp "$ROOT/packaging/linux/prerm" "$DEST/DEBIAN/prerm"
chmod 0755 "$DEST/DEBIAN/postinst" "$DEST/DEBIAN/prerm"

dpkg-deb --build --root-owner-group "$DEST" "$OUT/$PKG.deb"
echo "built $OUT/$PKG.deb"
