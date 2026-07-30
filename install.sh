#!/usr/bin/env bash
# whisper-ptt installer: venv + dependencies + systemd user service.
# On Wayland it additionally sets up ydotoold (see README "How it works").
# Idempotent — safe to re-run (e.g. after git pull).
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV="$DIR/.venv"
UNIT_DIR="$HOME/.config/systemd/user"
SESSION="${XDG_SESSION_TYPE:-x11}"
NEEDS_LOGOUT=0

SUDO=""
if [ "$(id -u)" -ne 0 ] && command -v sudo >/dev/null; then
  SUDO="sudo"
fi

install_pkg() {
  echo "==> installing $* (may ask for your password)"
  if command -v apt-get >/dev/null; then
    $SUDO apt-get update -qq && $SUDO apt-get install -y "$@"
  elif command -v dnf >/dev/null; then
    $SUDO dnf install -y "$@"
  elif command -v pacman >/dev/null; then
    $SUDO pacman -S --noconfirm "$@"
  else
    echo "ERROR: install $* manually (no apt/dnf/pacman found)" >&2
    exit 1
  fi
}

echo "==> checking prerequisites"
command -v python3 >/dev/null || { echo "ERROR: python3 missing"; exit 1; }
command -v pw-record >/dev/null || echo "WARNING: pw-record missing (install pipewire) — recording will fail"
echo "    session type: $SESSION"

echo "==> creating venv at $VENV"
if [ ! -x "$VENV/bin/python" ]; then
  # Debian/Ubuntu split ensurepip into a versioned package, so name the exact one.
  pyver="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
  python3 -m venv "$VENV" 2>/dev/null || {
    echo "    ensurepip unavailable — installing python${pyver}-venv"
    install_pkg "python${pyver}-venv"
    python3 -m venv "$VENV"
  }
fi
"$VENV/bin/pip" -q install --upgrade pip

echo "==> installing python dependencies"
# evdev is a C extension and has no wheels, so a machine without a compiler
# fails here with a wall of gcc output. Retry once with build deps rather than
# installing ~200 MB of toolchain up front on machines that don't need it.
if ! "$VENV/bin/pip" -q install -r "$DIR/requirements.txt"; then
  echo "    dependency build failed — installing compiler and Python headers"
  pyver="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
  if command -v apt-get >/dev/null; then
    install_pkg build-essential "python${pyver}-dev"
  elif command -v dnf >/dev/null; then
    install_pkg gcc "python3-devel"
  else
    install_pkg base-devel
  fi
  "$VENV/bin/pip" -q install -r "$DIR/requirements.txt"
fi
if command -v nvidia-smi >/dev/null; then
  echo "==> NVIDIA GPU detected — installing CUDA libraries"
  "$VENV/bin/pip" -q install -r "$DIR/requirements-gpu.txt"
else
  echo "==> no NVIDIA GPU — CPU mode (works fine, just slower)"
fi

if [ "$SESSION" = "wayland" ]; then
  echo "==> Wayland detected — setting up evdev key grab and ydotool typing"

  command -v ydotool  >/dev/null || install_pkg ydotool
  command -v wl-copy  >/dev/null || install_pkg wl-clipboard

  # /dev/uinput is root-only by default; hand it to the 'input' group so
  # ydotoold can run as a user service instead of as root.
  rule=/etc/udev/rules.d/99-whisper-ptt-uinput.rules
  if ! cmp -s "$DIR/udev/99-whisper-ptt-uinput.rules" "$rule"; then
    echo "    installing udev rule $rule"
    $SUDO install -m 0644 "$DIR/udev/99-whisper-ptt-uinput.rules" "$rule"
    $SUDO udevadm control --reload-rules
    $SUDO udevadm trigger --subsystem-match=misc --sysname-match=uinput || true
  fi

  # uinput is a module; load it now and on every boot.
  if [ ! -e /dev/uinput ]; then
    echo "    loading uinput module"
    $SUDO modprobe uinput
  fi
  if [ ! -f /etc/modules-load.d/uinput.conf ]; then
    printf 'uinput\n' | $SUDO tee /etc/modules-load.d/uinput.conf >/dev/null
  fi

  # Verify the rule actually applied. udevadm trigger does not always re-label
  # an already-existing node, and a silent failure here would only surface much
  # later as ydotool refusing to type.
  if [ -e /dev/uinput ]; then
    uinput_group="$(stat -c %G /dev/uinput)"
    if [ "$uinput_group" != "input" ]; then
      echo "    WARNING: /dev/uinput belongs to group '$uinput_group', not 'input'."
      echo "             Typing will fail until the rule applies — reboot, or run:"
      echo "               sudo udevadm control --reload-rules && sudo udevadm trigger"
    fi
  fi

  # Reading the PTT key from /dev/input needs the 'input' group. This also
  # grants the ability to observe all other keystrokes — see README.
  if ! id -nG "$USER" | tr ' ' '\n' | grep -qx input; then
    echo "    adding $USER to the 'input' group"
    $SUDO usermod -aG input "$USER"
    NEEDS_LOGOUT=1
  fi

  mkdir -p "$UNIT_DIR"
  sed "s|@DIR@|$DIR|g" "$DIR/systemd/ydotoold.service.in" \
    > "$UNIT_DIR/ydotoold.service"
fi

echo "==> installing systemd user service"
mkdir -p "$UNIT_DIR"
sed "s|@DIR@|$DIR|g" "$DIR/systemd/whisper-ptt.service.in" \
  > "$UNIT_DIR/whisper-ptt.service"
systemctl --user daemon-reload

if [ "$SESSION" = "wayland" ]; then
  systemctl --user enable ydotoold.service
  if [ "$NEEDS_LOGOUT" -eq 0 ]; then
    systemctl --user restart ydotoold.service
  fi
fi

if [ "$NEEDS_LOGOUT" -eq 1 ]; then
  systemctl --user enable whisper-ptt.service
  echo
  echo "Almost done — you were just added to the 'input' group."
  echo "Log out and back in (a reboot also works), then dictation starts automatically."
  echo "Verify afterwards with: systemctl --user status whisper-ptt.service"
  exit 0
fi

systemctl --user enable --now whisper-ptt.service

echo
echo "Done. Hold Right Ctrl in any window, speak, release — text gets inserted."
echo "First dictation downloads the whisper model (~0.5-1.5 GB), then it's offline."
echo "Logs: journalctl --user -u whisper-ptt.service -f"
if [ "$SESSION" = "wayland" ]; then
  echo "Wayland note: terminals paste with Ctrl+Shift+V — for those set"
  echo "  PTT_PASTE_KEY=ctrl+shift+v (see README 'Configuration')."
fi
