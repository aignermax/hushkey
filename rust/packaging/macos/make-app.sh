#!/bin/sh
# Build hushkey.app around a built hushkey program, ad-hoc signed.
#
#   make-app.sh <path/to/hushkey> <out-dir>     -> <out-dir>/hushkey.app
#
# The icon comes from assets/logo.png, the version from rust/Cargo.toml.
# macOS only (sips, iconutil, codesign).
set -eu

if [ $# -ne 2 ]; then
  echo "usage: $0 <hushkey binary> <out dir>" >&2
  exit 2
fi
bin=$1
out=$2
here=$(cd "$(dirname "$0")" && pwd)
logo="$here/../../../assets/logo.png"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/../../Cargo.toml" | head -n 1)

app="$out/hushkey.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/hushkey"
chmod 755 "$app/Contents/MacOS/hushkey"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
iconset="$work/hushkey.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$logo" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$logo" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/hushkey.icns"

# Ad hoc: no Developer ID. Gatekeeper still asks on the first open of a
# downloaded copy, and macOS ties granted permissions to this signature.
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
echo "$app"
