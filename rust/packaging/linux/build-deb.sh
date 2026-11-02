#!/usr/bin/env bash
# Build hushkey_<version>_<arch>.deb from an already built binary.
#
# Usage: rust/packaging/linux/build-deb.sh <version> <binary> [output-dir]
#
# The package carries the program, its menu entry, its icon and the udev
# rule for /dev/uinput — no interpreter, no virtual environment, no
# downloads. Library dependencies are read out of the binary with
# dpkg-shlibdeps rather than written down by hand, so a new linked library
# cannot be forgotten.
set -euo pipefail

VER="${1:?usage: build-deb.sh <version> <binary> [output-dir]}"
BIN="${2:?usage: build-deb.sh <version> <binary> [output-dir]}"
OUT="${3:-dist}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

[ -f "$BIN" ] || { echo "ERROR: no such binary: $BIN" >&2; exit 1; }

# dpkg's name for this machine's architecture (amd64, arm64).
ARCH="$(dpkg --print-architecture)"

# A semver pre-release ("0.11.0-rc.1") means the opposite to dpkg, which
# reads everything after the last hyphen as a Debian revision — and a
# revision sorts *after* the plain version. Someone who installed the
# release candidate would then never be offered the release. A tilde sorts
# before anything, which is what "rc" is supposed to mean:
#   0.11.0~rc.1 < 0.11.0, and both > 0.10.10
DEB_VER="${VER/-/\~}"
if [ "$DEB_VER" != "$VER" ]; then
  echo "==> version $VER is packaged as $DEB_VER (a pre-release sorts first)"
fi
PKG="hushkey_${DEB_VER}_${ARCH}"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
DEST="$STAGE/$PKG"
mkdir -p "$DEST/DEBIAN" "$DEST/usr/bin" "$DEST/usr/share/applications" \
         "$DEST/usr/share/icons/hicolor/512x512/apps" \
         "$DEST/usr/lib/udev/rules.d" "$OUT"

install -m 0755 "$BIN" "$DEST/usr/bin/hushkey"
install -m 0644 "$ROOT/assets/logo.png" \
    "$DEST/usr/share/icons/hicolor/512x512/apps/hushkey.png"

# The same entry the program writes for a per-user install, but owned by
# dpkg: every removal path takes it along, and `packaged()` keeps the
# program from writing a second one into the user's home.
cat > "$DEST/usr/share/applications/hushkey.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=hushkey
GenericName=Push-to-talk dictation
GenericName[de]=Push-to-Talk-Diktat
Comment=Local, offline dictation: hold a key, speak, release
Comment[de]=Lokales Offline-Diktat: Taste halten, sprechen, loslassen
Exec=/usr/bin/hushkey
Icon=hushkey
Terminal=false
StartupNotify=false
Categories=Utility;Accessibility;
Keywords=dictation;speech;voice;whisper;transcribe;push-to-talk;Diktat;Sprache;Spracheingabe;
EOF
chmod 0644 "$DEST/usr/share/applications/hushkey.desktop"

# Reading the key from /dev/input and typing through /dev/uinput both need
# the 'input' group; uinput is a module, so static_node is required for the
# rule to apply before anything has opened it.
cat > "$DEST/usr/lib/udev/rules.d/99-hushkey-uinput.rules" <<'EOF'
# Let hushkey create a virtual keyboard without running as root.
#
# This is a real privilege grant: a process that can write /dev/uinput can
# synthesize arbitrary input, and 'input' group membership can also read
# every keystroke via /dev/input/event*. Only on a machine you trust.
KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"
EOF
chmod 0644 "$DEST/usr/lib/udev/rules.d/99-hushkey-uinput.rules"

# --- metadata -------------------------------------------------------------
# Programs hushkey calls when it finds them; all are checked at runtime, so
# none is a hard dependency. wl-clipboard is what a Wayland session pastes
# through, xdotool what types on X11.
cat > "$DEST/DEBIAN/control" <<EOF
Package: hushkey
Version: $DEB_VER
Section: utils
Priority: optional
Architecture: $ARCH
Maintainer: aignermax <aignermax@users.noreply.github.com>
Depends: \${shlibs:Depends}, \${misc:Depends}
Recommends: wl-clipboard, xdotool
Homepage: https://hushkey.eu
Description: local, offline push-to-talk dictation
 Hold a key, speak, release — the transcript goes into whatever window has
 focus. Powered by whisper.cpp; nothing leaves your machine after the
 one-time model download. X11 and Wayland are both supported.
 .
 Dictating needs membership in the 'input' group, which this package grants
 the installing user. That takes one logout to apply.
EOF

for script in postinst prerm postrm; do
  install -m 0755 "$ROOT/rust/packaging/linux/$script" "$DEST/DEBIAN/$script"
done

# Library dependencies straight from the binary. dpkg-shlibdeps wants a
# debian/control to exist, so give it a throwaway one.
mkdir -p "$STAGE/debian"
printf 'Source: hushkey\n\nPackage: hushkey\nArchitecture: %s\n' "$ARCH" \
    > "$STAGE/debian/control"
( cd "$STAGE" && dpkg-shlibdeps -O --ignore-missing-info "$DEST/usr/bin/hushkey" ) \
    > "$STAGE/deps" 2>"$STAGE/deps.err" || {
      echo "ERROR: dpkg-shlibdeps failed" >&2; cat "$STAGE/deps.err" >&2; exit 1; }
# Output is one line: "shlibs:Depends=libc6 (>= 2.35), libgtk-3-0 (>= 3.24), ..."
SHLIBS="$(sed -n 's/^shlibs:Depends=//p' "$STAGE/deps")"
[ -n "$SHLIBS" ] || { echo "ERROR: dpkg-shlibdeps found no libraries" >&2; exit 1; }
sed -i "s|\${shlibs:Depends}, \${misc:Depends}|$SHLIBS|" "$DEST/DEBIAN/control"
echo "==> depends on: $SHLIBS"

dpkg-deb --build --root-owner-group "$DEST" "$OUT/$PKG.deb"
echo "built $OUT/$PKG.deb"
