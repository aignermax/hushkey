#!/usr/bin/env python3
"""whisper-ptt: push-to-talk dictation, fully local — Linux, Windows, macOS.

Hold the push-to-talk key (default: Right Ctrl), speak, release — the
transcript lands in the focused window (terminal, editor, browser, ...).
Nothing is submitted automatically; review and hit Enter yourself.

- Recording: pw-record (PipeWire) on Linux where available, otherwise
  sounddevice/PortAudio (Windows WASAPI, macOS Core Audio) — see recorder.py.
- Transcription: faster-whisper, local — GPU (CUDA) when available, else CPU.
  After the one-time model download nothing leaves the machine.
- Key grab and text insertion are backend-specific:
    pynput   X11, Windows and macOS: pynput both reads the key and types the
             text out character by character. No group membership, no root
             daemon. On macOS grant the terminal Accessibility + Input
             Monitoring.
    wayland  Wayland gives clients neither a global key grab nor input
             injection, so evdev reads the key from /dev/input (needs the
             'input' group) and the text goes in via the clipboard plus a
             single synthetic Ctrl+V through ydotool. Typing it out
             keycode-by-keycode is not an option there: ydotool assumes a US
             layout, so on a 'de' keymap 'z' would come out as 'y' and umlauts
             not at all. The clipboard carries UTF-8, so only one
             layout-stable chord has to be synthesized.
- Log stores metadata only (duration, char count), never dictated text.

Config via environment:
  PTT_KEY          key name (default: ctrl_r; e.g. f9, caps_lock, or a raw
                   evdev name like KEY_RIGHTCTRL)
  WHISPER_MODEL    model size (default: medium on GPU, small on CPU)
  WHISPER_LANG     language code (default: de; empty string = auto-detect)
  PTT_BACKEND      force 'pynput' (alias: 'x11') or 'wayland'
                   (default: from XDG_SESSION_TYPE)
  PTT_TYPE_DELAY   pynput backend only: seconds between typed characters
                   (default: 0.01); heavy editors drop/reorder fast synthetic
                   keystrokes — raise it (e.g. 0.03) if dictation arrives
                   garbled
  PTT_TYPE_DELAY_TERMINAL  Windows + Linux/X11: delay used instead when the
                   focused window is a terminal (default: 0 = full speed;
                   consoles keep up, only heavy editors need the pacing;
                   Wayland pastes instead of typing, so nothing to pace)
  PTT_PASTE_KEY    Wayland paste chord (default: ctrl+v; terminals usually
                   need ctrl+shift+v)
  PTT_KEEP_CLIPBOARD  1 = leave the transcript in the clipboard instead of
                   restoring the previous contents
  PTT_CLIPBOARD_SETTLE  Wayland: seconds to wait before restoring the previous
                   clipboard (default: 0.4); raise it if a slow app ends up
                   pasting the restored value instead of the transcript
  PTT_CMD_TIMEOUT  seconds a helper (wl-paste, ydotool) may take before it is
                   given up on (default: 30; 0 = wait indefinitely)

Run:  .venv/bin/python dictate.py        (usually via service/scheduled task)
Stop: Ctrl-C, or stop the service (systemctl / Task Scheduler / launchctl)
"""
from __future__ import annotations

import glob
import json
import math
import os
import shutil
import subprocess
import sys
import threading
import time
from datetime import datetime

from recorder import pick_recorder


def _state_dir():
    if sys.platform == "darwin":
        return os.path.expanduser("~/Library/Logs/whisper-ptt")
    if sys.platform == "win32":
        return os.path.join(os.environ.get(
            "LOCALAPPDATA", os.path.expanduser("~/AppData/Local")),
            "whisper-ptt")
    return os.path.join(os.environ.get("XDG_STATE_HOME",
                                       os.path.expanduser("~/.local/state")),
                        "whisper-ptt")


STATE_DIR = _state_dir()

VERSION = "0.4.1"

# The tray icon (tray.py) reads this file; written on every state transition.
STATE_PATH = os.path.join(STATE_DIR, "state.json")

# The tray's persistent choices (currently just the whisper model).
CONFIG_PATH = os.path.join(STATE_DIR, "config.json")

# Set by load_model(); published in the state file so the tray can show it.
CURRENT_MODEL = None


def configured_model():
    """Model choice: WHISPER_MODEL env wins (documented), then the tray's
    config file, else None (the caller falls back to the device default)."""
    env = os.environ.get("WHISPER_MODEL")
    if env:
        return env
    try:
        with open(CONFIG_PATH, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return None
    name = data.get("model") if isinstance(data, dict) else None
    return name if isinstance(name, str) and name else None


def write_state(state):
    """Publish the daemon state ('idle'/'recording'/'transcribing') for tray.py.

    Best effort and atomic (temp file + replace): the tray tolerates a missing
    file, but a half-written one would be noise. Never let state reporting
    break dictation itself.
    """
    try:
        os.makedirs(STATE_DIR, exist_ok=True)
        tmp = STATE_PATH + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump({"state": state, "pid": os.getpid(),
                       "version": VERSION, "model": CURRENT_MODEL,
                       "ts": time.time()}, fh)
        os.replace(tmp, STATE_PATH)
    except OSError:
        pass
LOG_PATH = os.path.join(STATE_DIR, "dictate.log")
PTT_KEY = os.environ.get("PTT_KEY", "ctrl_r")


def _env_float(name, default):
    try:
        value = float(os.environ.get(name, ""))
    except ValueError:
        return default
    return value if math.isfinite(value) and value >= 0 else default


TYPE_DELAY = _env_float("PTT_TYPE_DELAY", 0.01)
TERMINAL_TYPE_DELAY = _env_float("PTT_TYPE_DELAY_TERMINAL", 0.0)
MIN_SECONDS = 0.5  # shorter recordings count as accidental taps
# Let the focused app fetch the selection before the clipboard is handed back.
# Configurable because the right value depends on how fast that app reacts.
CLIPBOARD_SETTLE = _env_float("PTT_CLIPBOARD_SETTLE", 0.4)
# How long to let wl-copy prove it survived long enough to own the clipboard.
CLIPBOARD_HANDOFF = _env_float("PTT_CLIPBOARD_HANDOFF", 0.2)
# Upper bound for helpers we genuinely have to wait for (wl-paste, ydotool).
# Not wl-copy — see _clipboard_write. Unbounded waits would wedge the daemon for
# the rest of the session, but the old 5 s was tight enough to drop real
# dictations, so this is deliberately generous. 0 removes the limit entirely.
CMD_TIMEOUT = _env_float("PTT_CMD_TIMEOUT", 30) or None

try:
    from pynput.keyboard import Key
    # control chars type as their keys, same as pynput's Controller.type()
    _CONTROL_KEYS = {"\n": Key.enter, "\r": Key.enter, "\t": Key.tab}
except ImportError:  # headless Linux: pynput needs X11; run() fails there anyway
    _CONTROL_KEYS = {}


def log(msg):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG_PATH, "a", encoding="utf-8") as fh:
        fh.write(f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}\n")


def notify(title, body=""):
    """Best-effort desktop notification; never raises."""
    try:
        if shutil.which("notify-send"):
            subprocess.Popen(["notify-send", "-t", "2000", title, body],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elif sys.platform == "darwin" and shutil.which("osascript"):
            script = (f"display notification {json.dumps(body, ensure_ascii=False)}"
                      f" with title {json.dumps(title, ensure_ascii=False)}")
            subprocess.Popen(["osascript", "-e", script],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        elif sys.platform == "win32":
            _notify_windows(title, body)
        else:
            print(f"{title} {body}".strip(), file=sys.stderr)
    except Exception:
        pass


def _notify_windows(title, body):
    """Toast via a PowerShell NotifyIcon balloon — no extra dependencies."""
    script = (
        "param($t, $b) "
        "Add-Type -AssemblyName System.Windows.Forms; "
        "Add-Type -AssemblyName System.Drawing; "
        "$n = New-Object System.Windows.Forms.NotifyIcon; "
        "$n.Icon = [System.Drawing.SystemIcons]::Information; "
        "$n.Visible = $true; "
        "$n.ShowBalloonTip(2000, $t, $b, [System.Windows.Forms.ToolTipIcon]::Info); "
        "Start-Sleep -Milliseconds 1500; "
        "$n.Dispose()")
    subprocess.Popen(
        ["powershell", "-NoProfile", "-NonInteractive", "-Command", script,
         title[:60], body[:250]],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))


def backend_name():
    """'wayland' or 'pynput' — which key-grab/insertion pair to use.

    'pynput' covers X11, Windows and macOS; only Linux/Wayland needs the
    evdev+ydotool pair. 'x11' stays accepted as an alias because earlier
    versions documented PTT_BACKEND=x11.
    """
    forced = os.environ.get("PTT_BACKEND")
    if forced:
        return "pynput" if forced == "x11" else forced
    return "wayland" if os.environ.get("XDG_SESSION_TYPE") == "wayland" else "pynput"


def preload_cuda_libs():
    """Preload pip-installed CUDA libs so ctranslate2 finds them by name."""
    try:
        import ctypes
        import nvidia.cublas  # type: ignore
        import nvidia.cudnn  # type: ignore
        libs = []
        for pkg in (nvidia.cublas, nvidia.cudnn):
            pkg_dir = pkg.__path__[0] if pkg.__file__ is None \
                else os.path.dirname(pkg.__file__)
            if sys.platform == "win32":
                libs += sorted(glob.glob(os.path.join(pkg_dir, "bin", "*.dll")))
            else:
                libs += sorted(glob.glob(os.path.join(pkg_dir, "lib", "*.so*")))
        for lib in libs:
            if hasattr(os, "add_dll_directory"):  # Windows Python 3.8+
                os.add_dll_directory(os.path.dirname(lib))
            try:
                ctypes.CDLL(lib, mode=getattr(ctypes, "RTLD_GLOBAL", 0))
            except OSError:
                pass
    except ImportError:
        pass


def pick_device():
    """Return (device, compute_type, default_model)."""
    preload_cuda_libs()
    try:
        import ctranslate2  # type: ignore
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", "float16", "medium"
    except Exception:
        pass
    return "cpu", "int8", "small"


# --------------------------------------------------------------------------
# Key names
# --------------------------------------------------------------------------

# pynput-style names (the documented PTT_KEY values) -> evdev names.
_EVDEV_ALIASES = {
    "ctrl_r": "KEY_RIGHTCTRL", "ctrl_l": "KEY_LEFTCTRL",
    "alt_r": "KEY_RIGHTALT", "alt_gr": "KEY_RIGHTALT", "alt_l": "KEY_LEFTALT",
    "shift_r": "KEY_RIGHTSHIFT", "shift_l": "KEY_LEFTSHIFT",
    "super_r": "KEY_RIGHTMETA", "super_l": "KEY_LEFTMETA",
    "caps_lock": "KEY_CAPSLOCK", "menu": "KEY_COMPOSE",
    "scroll_lock": "KEY_SCROLLLOCK", "pause": "KEY_PAUSE",
    "insert": "KEY_INSERT", "space": "KEY_SPACE",
}
_EVDEV_ALIASES.update({f"f{n}": f"KEY_F{n}" for n in range(1, 25)})


def evdev_keycode(name):
    """Resolve a PTT_KEY name to an evdev keycode, or None."""
    from evdev import ecodes
    candidate = name if name.startswith("KEY_") \
        else _EVDEV_ALIASES.get(name, "KEY_" + name.upper())
    return getattr(ecodes, candidate, None)


# --------------------------------------------------------------------------
# Text insertion backends
# --------------------------------------------------------------------------

# Window classes / process names that mean "the focused window is a terminal".
# Consoles keep up with full-speed synthetic keystrokes, so typing there can
# skip the pacing delay that heavy editors need.
_TERMINAL_WINDOW_CLASSES = {
    "ConsoleWindowClass",             # conhost: cmd, PowerShell, ...
    "CASCADIA_HOSTING_WINDOW_CLASS",  # Windows Terminal
}
_TERMINAL_PROCESSES = {
    "windowsterminal.exe", "wezterm-gui.exe", "alacritty.exe", "mintty.exe",
    "conemu.exe", "conemu64.exe", "tabby.exe", "hyper.exe",
}
# X11 WM_CLASS values (res_name or res_class, compared lowercased).
_TERMINAL_WM_CLASSES = {
    "alacritty", "contour", "cool-retro-term", "deepin-terminal", "foot",
    "ghostty", "gnome-terminal", "gnome-terminal-server", "guake", "hyper",
    "kitty", "konsole", "lxterminal", "mate-terminal", "pantheon-terminal",
    "qterminal", "st", "tabby", "terminator", "terminology", "tilix",
    "tilda", "urxvt", "uxterm", "wezterm", "xterm", "yakuake",
}


def foreground_window_is_terminal():
    """True if the focused window is a terminal.

    Windows: class/process name of the foreground window. Linux/X11: WM_CLASS
    of the input-focus window. Wayland hides which window has focus — and gets
    its text via clipboard paste anyway — so it stays False there.
    """
    if sys.platform == "win32":
        return _windows_foreground_is_terminal()
    if (sys.platform.startswith("linux") and os.environ.get("DISPLAY")
            and os.environ.get("XDG_SESSION_TYPE") != "wayland"):
        return _x11_focus_is_terminal()
    return False


def _windows_foreground_is_terminal():
    try:
        import ctypes
        from ctypes import wintypes
        user32 = ctypes.windll.user32
        kernel32 = ctypes.windll.kernel32
        user32.GetForegroundWindow.restype = wintypes.HWND
        hwnd = user32.GetForegroundWindow()
        if not hwnd:
            return False
        class_buf = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, class_buf, len(class_buf))
        if class_buf.value in _TERMINAL_WINDOW_CLASSES:
            return True
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        handle = kernel32.OpenProcess(0x1000, False, pid.value)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return False
        try:
            path_buf = ctypes.create_unicode_buffer(512)
            size = wintypes.DWORD(len(path_buf))
            if kernel32.QueryFullProcessImageNameW(handle, 0, path_buf, ctypes.byref(size)):
                return os.path.basename(path_buf.value).lower() in _TERMINAL_PROCESSES
        finally:
            kernel32.CloseHandle(handle)
    except Exception:
        return False
    return False


def _x11_focus_is_terminal():
    """WM_CLASS heuristic via python-xlib (already pulled in by pynput)."""
    try:
        from Xlib import display as xdisplay
    except ImportError:
        return False
    d = None
    try:
        d = xdisplay.Display()
        focus = d.get_input_focus().focus
        if not hasattr(focus, "get_wm_class"):
            return False
        wm_class = focus.get_wm_class() or ()
        return any(c.lower() in _TERMINAL_WM_CLASSES for c in wm_class if c)
    except Exception:
        return False
    finally:
        if d is not None:
            d.close()


class PynputInjector:
    """Type the text out via pynput (XTEST on X11, native APIs elsewhere).

    Layout-independent, no extra setup. Characters go one at a time with a
    small delay: heavy apps (Electron editors, browsers) drop or reorder
    keystrokes fired at full speed.
    """

    def __init__(self):
        self.controller = None

    def check(self):
        return None

    def insert(self, text):
        if self.controller is None:
            from pynput.keyboard import Controller
            self.controller = Controller()
        delay = TYPE_DELAY
        if delay and TERMINAL_TYPE_DELAY < delay and foreground_window_is_terminal():
            delay = TERMINAL_TYPE_DELAY
        for ch in text:
            key = _CONTROL_KEYS.get(ch, ch)
            self.controller.press(key)
            self.controller.release(key)
            if delay:
                time.sleep(delay)


class WaylandInjector:
    """Put the text on the clipboard, then synthesize one paste chord.

    Clients cannot inject input under Wayland, so the keystroke goes through
    /dev/uinput via ydotool — below the compositor, which therefore treats it
    as a real keyboard. Only the chord is synthesized; the text itself travels
    as UTF-8 through the clipboard and is unaffected by the active keymap.
    """

    def __init__(self):
        self.chord = os.environ.get("PTT_PASTE_KEY", "ctrl+v")
        self.keep_clipboard = os.environ.get("PTT_KEEP_CLIPBOARD") == "1"
        self._chord_args = None
        self._copy_proc = None  # the wl-copy currently owning the clipboard

    @staticmethod
    def _socket_path():
        """The socket ydotool will talk to, resolved the same way ydotool does.

        Resolving it ourselves (rather than only honouring an explicitly set
        YDOTOOL_SOCKET) means a manual `python dictate.py` run validates the
        same socket the systemd unit uses, instead of failing later inside
        ydotool with a less obvious message.
        """
        explicit = os.environ.get("YDOTOOL_SOCKET")
        if explicit:
            return explicit
        runtime = os.environ.get("XDG_RUNTIME_DIR")
        return os.path.join(runtime, ".ydotool_socket") if runtime else None

    def check(self):
        """Return a human-readable problem description, or None if usable."""
        for tool in ("wl-copy", "wl-paste", "ydotool"):
            if not shutil.which(tool):
                return f"{tool} not found (install wl-clipboard and ydotool)"
        try:
            self._chord_args = self._build_chord(self.chord)
        except ValueError as exc:
            return str(exc)
        socket = self._socket_path()
        if socket is None:
            return ("neither YDOTOOL_SOCKET nor XDG_RUNTIME_DIR is set — "
                    "cannot locate the ydotoold socket")
        if not os.path.exists(socket):
            return (f"ydotoold socket {socket} missing — "
                    "systemctl --user start ydotoold.service")
        return None

    @staticmethod
    def _build_chord(chord):
        """'ctrl+shift+v' -> ydotool key arguments, press then release."""
        from evdev import ecodes
        names = {"ctrl": "KEY_LEFTCTRL", "control": "KEY_LEFTCTRL",
                 "shift": "KEY_LEFTSHIFT", "alt": "KEY_LEFTALT",
                 "super": "KEY_LEFTMETA", "meta": "KEY_LEFTMETA"}
        codes = []
        for part in chord.lower().split("+"):
            part = part.strip()
            if not part:
                continue
            evname = names.get(part) or (
                part.upper() if part.startswith("KEY_") else "KEY_" + part.upper())
            code = getattr(ecodes, evname, None)
            if code is None:
                raise ValueError(f"unknown key '{part}' in PTT_PASTE_KEY={chord}")
            codes.append(code)
        if not codes:
            raise ValueError("PTT_PASTE_KEY is empty")
        # ydotool 1.x takes KEYCODE:STATE pairs; numeric codes avoid depending
        # on its own (version-dependent) key-name parser.
        return ([f"{c}:1" for c in codes]
                + [f"{c}:0" for c in reversed(codes)])

    def _clipboard_read(self):
        """Previous clipboard contents, but only when they are plain text.

        Pinning the type matters: without it a clipboard holding an image would
        come back as raw bytes and get restored as text/plain, i.e. silently
        corrupted. Failing to read is the safe outcome — we then simply leave
        the transcript in place rather than writing something wrong back.
        """
        try:
            out = subprocess.run(
                ["wl-paste", "--no-newline", "--type", "text/plain"],
                capture_output=True, timeout=CMD_TIMEOUT)
            return out.stdout if out.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            # Includes the timeout case. wl-paste is not a selection owner, so
            # run() killing it here harms nothing.
            return None

    def _clipboard_write(self, data: bytes):
        """Hand the bytes to wl-copy and leave it running.

        wl-copy is not allowed to exit: under Wayland the client offering a
        selection *is* its owner for as long as the content stays on the
        clipboard. So waiting for it to finish (subprocess.run) blocks until
        some other client takes ownership — which may be never — and a
        run(timeout=...) makes it worse, because run() kills the process on the
        way out. Killing the owner leaves the compositor pointing at a dead
        client, and from then on every clipboard operation in the session hangs,
        not just ours. Start it, feed it, let go.

        --foreground keeps *this* process the owner instead of a fork of it, so
        poll() below actually tells us whether the handoff worked.

        Failure still has to propagate: insert() synthesizes a paste right
        after, so a silently-failed write would paste whatever was on the
        clipboard before — a copied password, say — while the log records a
        successful dictation.
        """
        self._release_previous_owner()
        try:
            proc = subprocess.Popen(
                ["wl-copy", "--foreground", "--type", "text/plain"],
                stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, start_new_session=True)
        except OSError as exc:
            raise RuntimeError(f"cannot start wl-copy: {exc}") from exc
        try:
            proc.stdin.write(data)
            proc.stdin.close()
        except OSError as exc:
            raise RuntimeError(f"wl-copy would not take the text: {exc}") from exc
        # An immediate exit means it never became the owner (no compositor, bad
        # arguments), and pasting now would insert the previous contents.
        time.sleep(CLIPBOARD_HANDOFF)
        if proc.poll() is not None:
            raise RuntimeError(
                f"wl-copy exited straight away (status {proc.returncode}) — "
                "the transcript is not on the clipboard")
        self._copy_proc = proc

    def _release_previous_owner(self):
        """Stop tracking the last wl-copy, without killing it.

        The new wl-copy takes ownership and the old one then exits by itself.
        Terminating it here is exactly the mistake described above.
        """
        proc, self._copy_proc = self._copy_proc, None
        if proc is not None:
            proc.poll()  # reap if it already exited; never signal it

    def insert(self, text):
        if self._chord_args is None:
            self._chord_args = self._build_chord(self.chord)
        env = dict(os.environ)
        socket = self._socket_path()
        if socket:
            env["YDOTOOL_SOCKET"] = socket  # keep client and daemon in sync
        previous = None if self.keep_clipboard else self._clipboard_read()
        self._clipboard_write(text.encode("utf-8"))
        subprocess.run(["ydotool", "key"] + self._chord_args,
                       timeout=CMD_TIMEOUT, env=env,
                       stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True)
        if previous is not None:
            # Best effort: the paste is asynchronous, so give the focused app
            # time to fetch the selection before handing the clipboard back.
            time.sleep(CLIPBOARD_SETTLE)
            try:
                self._clipboard_write(previous)
            except (OSError, subprocess.SubprocessError, RuntimeError) as exc:
                # Only the restore failed — the transcript is already inserted,
                # so log it and leave the clipboard holding the transcript.
                log(f"clipboard restore failed: {exc}")


# --------------------------------------------------------------------------
# Key listener backends
# --------------------------------------------------------------------------

class PynputListener:
    def __init__(self, key_name):
        self.key_name = key_name

    def check(self):
        # pynput raises on import when it cannot reach a display server, which
        # is exactly what happens when this backend is forced on a Wayland or
        # headless session. check() exists to explain that, not to traceback.
        try:
            from pynput import keyboard
        except Exception as exc:
            return (f"pynput unusable ({exc}) — on Wayland drop PTT_BACKEND=x11 "
                    "so the evdev/ydotool backend is used instead")
        if getattr(keyboard.Key, self.key_name, None) is None:
            return f"unknown PTT_KEY '{self.key_name}' (see pynput.keyboard.Key)"
        return None

    def listen(self, on_press, on_release):
        from pynput import keyboard
        key = getattr(keyboard.Key, self.key_name)

        with keyboard.Listener(
                on_press=lambda k: on_press() if k == key else None,
                on_release=lambda k: on_release() if k == key else None) as lst:
            lst.join()


class EvdevListener:
    """Read the PTT key straight from /dev/input.

    Wayland offers clients no global key grab, so we watch the kernel devices
    instead. This needs read access to /dev/input/event*, i.e. membership in
    the 'input' group — which also grants the ability to observe every other
    keystroke on the machine, so only grant it on a machine you trust.
    """

    def __init__(self, key_name):
        self.key_name = key_name
        self.code = None

    def check(self):
        try:
            import evdev  # noqa: F401
        except ImportError:
            return "python-evdev missing (pip install evdev)"
        self.code = evdev_keycode(self.key_name)
        if self.code is None:
            return f"unknown PTT_KEY '{self.key_name}' (try e.g. ctrl_r, f9)"
        devices = self._keyboards()
        if not devices:
            return ("no readable keyboard in /dev/input — add yourself to the "
                    "'input' group and log out once")
        for dev in devices:
            dev.close()
        return None

    @staticmethod
    def _keyboards():
        """Open every device that looks like a real keyboard."""
        import evdev
        from evdev import ecodes
        found = []
        for path in evdev.list_devices():
            try:
                dev = evdev.InputDevice(path)
            except OSError:
                continue  # not readable for us — expected for some nodes
            keys = dev.capabilities().get(ecodes.EV_KEY, ())
            # Letter keys separate keyboards from mice, lid switches and
            # power buttons, which also advertise EV_KEY.
            if ecodes.KEY_A in keys and ecodes.KEY_Z in keys:
                found.append(dev)
            else:
                dev.close()
        return found

    def listen(self, on_press, on_release):
        import selectors
        from evdev import ecodes

        devices = self._keyboards()
        selector = selectors.DefaultSelector()
        for dev in devices:
            selector.register(dev, selectors.EVENT_READ)
        log(f"evdev listening on: {', '.join(d.path for d in devices)}")
        try:
            while selector.get_map():
                for ready, _mask in selector.select():
                    dev = ready.fileobj
                    try:
                        events = list(dev.read())
                    except OSError as exc:
                        # Keyboard went away (USB unplugged, Bluetooth asleep).
                        # Drop just that device: with several keyboards attached
                        # the others keep working, and tearing the daemon down
                        # would mean reloading the whisper model afterwards.
                        log(f"evdev device {dev.path} lost ({exc}) — dropping it")
                        selector.unregister(dev)
                        # A key held when the device vanished never sends its
                        # release. stop_recording() is a no-op if nothing is
                        # recording, so this is safe either way.
                        on_release()
                        continue
                    for event in events:
                        if event.type != ecodes.EV_KEY or event.code != self.code:
                            continue
                        if event.value == 1:
                            on_press()
                        elif event.value == 0:
                            on_release()
                        # value 2 is auto-repeat while held — ignore it
            # Every keyboard disappeared. Exiting lets systemd restart us once
            # they are back, which also re-scans for newly plugged devices.
            sys.exit("no keyboard left to listen on")
        finally:
            selector.close()
            for dev in devices:
                try:
                    dev.close()
                except OSError:
                    pass


def make_backends(key_name):
    if backend_name() == "wayland":
        return EvdevListener(key_name), WaylandInjector()
    return PynputListener(key_name), PynputInjector()


# --------------------------------------------------------------------------

class DictationDaemon:
    def __init__(self):
        self.model = None
        self.recorder = pick_recorder()
        self.recording = None  # start_time while a recording is active
        self.busy_lock = threading.Lock()
        self.listener, self.injector = make_backends(PTT_KEY)

    def load_model(self):
        global CURRENT_MODEL
        device, compute, default_model = pick_device()
        name = configured_model() or default_model
        from faster_whisper import WhisperModel
        print(f"loading whisper '{name}' on {device} ({compute}) ...", file=sys.stderr)
        try:
            self.model = WhisperModel(name, device=device, compute_type=compute)
        except Exception as exc:
            if device == "cuda":
                print(f"CUDA failed ({exc}); using CPU", file=sys.stderr)
                self.model = WhisperModel(name, device="cpu", compute_type="int8")
            else:
                raise
        CURRENT_MODEL = name
        log(f"model loaded: {name}/{device}")

    def start_recording(self):
        if self.recording is not None:
            return  # ignore key auto-repeat
        if self.recorder is None:
            notify("dictation error",
                   "no recorder (install pipewire/pw-record or sounddevice)")
            log("ERROR: no recording backend available")
            return
        try:
            self.recorder.start()
        except Exception as exc:
            log(f"ERROR starting recording: {exc}")
            notify("dictation error", str(exc)[:80])
            return
        self.recording = time.time()
        write_state("recording")
        notify("● recording", f"(release {PTT_KEY} to transcribe)")

    def stop_recording(self):
        started, self.recording = self.recording, None
        if started is None:
            return
        duration = time.time() - started
        try:
            wav = self.recorder.stop()
        except Exception as exc:
            log(f"ERROR stopping recording: {exc}")
            notify("dictation error", str(exc)[:80])
            write_state("idle")
            return
        if duration < MIN_SECONDS or not wav or os.path.getsize(wav) < 1000:
            log(f"ignored short/empty recording ({duration:.2f}s)")
            if wav and os.path.exists(wav):
                os.remove(wav)
            write_state("idle")
            return
        threading.Thread(target=self._transcribe_and_insert,
                         args=(wav, duration), daemon=True).start()

    def _transcribe_and_insert(self, wav, duration):
        with self.busy_lock:
            write_state("transcribing")
            try:
                notify("… transcribing", "")
                lang = os.environ.get("WHISPER_LANG", "de") or None
                segments, _info = self.model.transcribe(wav, language=lang,
                                                        vad_filter=True, beam_size=5)
                text = " ".join(s.text.strip() for s in segments).strip()
                log(f"transcribed {duration:.1f}s audio -> {len(text)} chars")
                if not text:
                    notify("dictation", "nothing recognized")
                    return
                time.sleep(0.15)  # let the modifier release settle
                self.injector.insert(text + " ")  # space separates dictations
            except Exception as exc:
                log(f"ERROR: {exc}")
                notify("dictation error", str(exc)[:80])
            finally:
                try:
                    os.remove(wav)
                except OSError:
                    pass
                write_state("idle")

    def run(self):
        which = backend_name()
        for component in (self.listener, self.injector):
            problem = component.check()
            if problem:
                sys.exit(f"{which} backend unusable: {problem}")
        write_state("starting")  # tray + overlay show the boot/download phase
        self.load_model()
        notify("dictation ready", f"hold {PTT_KEY} and speak")
        print(f"push-to-talk ready on '{PTT_KEY}' ({which}) — hold to record",
              file=sys.stderr)
        log(f"ready on {PTT_KEY} ({which} backend)")
        write_state("idle")
        self.listener.listen(self.start_recording, self.stop_recording)


def main():
    try:
        DictationDaemon().run()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
