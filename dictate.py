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
             Monitoring. On X11, characters the server cannot map to a key
             (CJK on a keymap without a single unused keycode row — GNOME
             fills every row with XF86 keysyms) go in through the clipboard
             plus a synthetic paste chord instead; typing them is impossible.
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
                   evdev name like KEY_RIGHTCTRL). With caps_lock a short tap
                   keeps toggling capitals while a hold dictates; the caps
                   state is restored on release
  WHISPER_MODEL    model size (default: medium on GPU, small on CPU)
  WHISPER_LANG     language code (default: de; empty string = auto-detect)
  WHISPER_ZH_PROMPT  initial prompt for Chinese dictation (default: a
                   Simplified-Chinese sentence). Whisper's multilingual
                   models otherwise mix Traditional characters into Mandarin
                   output at random; the prompt pins the script. Applied
                   directly when zh is pinned; on auto-detect, dictations
                   recognized as Chinese are decoded a second time with it —
                   only those pay for the extra pass. '' disables it;
                   Traditional-Chinese users can set a Traditional prompt
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
  PTT_STREAMING    1 = experimental: while the key is held, completed speech
                   blocks are transcribed and inserted every few seconds
                   instead of one big paste on release (default: 0)
  PTT_STREAM_INTERVAL  seconds between streaming ticks (default: 3.0)
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

VERSION = "0.7.8"

# The tray icon (tray.py) reads this file; written on every state transition.
STATE_PATH = os.path.join(STATE_DIR, "state.json")

# The tray's persistent choices (model, push-to-talk key, …).
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


def configured_ptt_key():
    """Push-to-talk key: PTT_KEY env wins (documented), then the tray's
    config file, else the default 'ctrl_r'."""
    env = os.environ.get("PTT_KEY")
    if env:
        return env
    try:
        with open(CONFIG_PATH, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return "ctrl_r"
    name = data.get("ptt_key") if isinstance(data, dict) else None
    return name if isinstance(name, str) and name else "ctrl_r"


def configured_lang():
    """Dictation language: WHISPER_LANG env wins (documented), then the tray's
    config file, else 'de'. Returns None for auto-detect — spelled 'auto' in
    the config file, '' or 'auto' in the env var. Unknown codes fall back to
    'de' instead of killing every dictation with a tokenizer error.

    Read on every dictation, so a tray language switch needs no restart.
    """
    if "WHISPER_LANG" in os.environ:
        name = os.environ["WHISPER_LANG"]
    else:
        try:
            with open(CONFIG_PATH, encoding="utf-8") as fh:
                data = json.load(fh)
            name = data.get("lang") if isinstance(data, dict) else None
        except (OSError, ValueError):
            name = None
        if not isinstance(name, str) or not name:
            name = "de"
    if name in ("", "auto"):
        return None
    if name not in _WHISPER_LANGS:
        log(f"unknown language code '{name}' — falling back to 'de'")
        return "de"
    return name


# the language codes whisper's tokenizer knows (kept in sync with
# faster-whisper's tokenizer._LANGUAGE_CODES — 100 codes)
_WHISPER_LANGS = frozenset(
    "af am ar as az ba be bg bn bo br bs ca cs cy da de el en es et eu fa fi "
    "fo fr gl gu ha haw he hi hr ht hu hy id is it ja jw ka kk km kn ko la lb "
    "ln lo lt lv mg mi mk ml mn mr ms mt my ne nl nn no oc pa pl ps pt ro ru "
    "sa sd si sk sl sn so sq sr su sv sw ta te tg th tk tl tr tt uk ur uz vi "
    "yi yo yue zh".split())


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
                       "ptt_key": PTT_KEY,
                       "ts": time.time()}, fh)
        os.replace(tmp, STATE_PATH)
    except OSError:
        pass
LOG_PATH = os.path.join(STATE_DIR, "dictate.log")
PTT_KEY = configured_ptt_key()


def _env_float(name, default):
    try:
        value = float(os.environ.get(name, ""))
    except ValueError:
        return default
    return value if math.isfinite(value) and value >= 0 else default


TYPE_DELAY = _env_float("PTT_TYPE_DELAY", 0.01)
TERMINAL_TYPE_DELAY = _env_float("PTT_TYPE_DELAY_TERMINAL", 0.0)
# Mandarin script fix: whisper's multilingual models mix Traditional
# characters into Mandarin output at random; a Simplified initial_prompt
# pins the script (see DictationDaemon._transcribe). '' disables it;
# Traditional-Chinese users can set a Traditional prompt instead.
ZH_PROMPT = os.environ.get("WHISPER_ZH_PROMPT", "以下是简体中文的句子。")
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

# Streaming mode (experimental, opt-in): while the key is held, completed
# speech blocks are transcribed and inserted every STREAM_INTERVAL seconds
# instead of one big paste on release. The tail is still handled on release.
STREAMING = os.environ.get("PTT_STREAMING", "0") != "0"
# 0.5 s floor: a 0 interval would spin the worker in a back-to-back
# snapshot+transcribe loop for the whole hold.
STREAM_INTERVAL = max(0.5, _env_float("PTT_STREAM_INTERVAL", 3.0))
# Never transcribe right up to the snapshot edge: the recorder may still be
# flushing. And never commit a segment that reaches the slice end — speech
# touching the edge is likely cut mid-word and comes back, complete, on the
# next tick with more context.
STREAM_TAIL_GUARD = 0.3
STREAM_TRAILING_SILENCE = 0.8
STREAM_MIN_SLICE = 1.0  # seconds of new audio before a tick bothers

try:
    from pynput.keyboard import Key
    # control chars type as their keys, same as pynput's Controller.type()
    _CONTROL_KEYS = {"\n": Key.enter, "\r": Key.enter, "\t": Key.tab}
    # Modifier/special keys for the X11 clipboard fallback's paste chord and
    # for the caps lock restore tap. pynput's Key members differ per backend
    # (xorg only aliases ctrl_l -> ctrl, darwin has no insert key at all),
    # so resolve defensively.
    _CHORD_KEYS = {name: key for name, key in (
        ("ctrl", getattr(Key, "ctrl_l", None)),
        ("control", getattr(Key, "ctrl_l", None)),
        ("shift", getattr(Key, "shift_l", None)),
        ("alt", getattr(Key, "alt_l", None)),
        ("super", getattr(Key, "cmd_l", None)),
        ("insert", getattr(Key, "insert", None)),
        ("caps_lock", getattr(Key, "caps_lock", None)),
    ) if key is not None}
except ImportError:  # headless Linux: pynput needs X11; run() fails there anyway
    _CONTROL_KEYS = {}
    _CHORD_KEYS = {}


def log(msg):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG_PATH, "a", encoding="utf-8") as fh:
        fh.write(f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}\n")


def _audio_rms(wav):
    """RMS level of a 16-bit mono WAV (0.0–1.0); None when unreadable."""
    try:
        import wave
        import numpy as np
        with wave.open(wav, "rb") as fh:
            if fh.getsampwidth() != 2:
                return None
            frames = fh.readframes(fh.getnframes())
        if not frames:
            return None
        pcm = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
        return float(np.sqrt(np.mean(pcm ** 2)))
    except Exception:
        return None


def _decode_wav_16k(path):
    """Decode any WAV to a float32 16 kHz mono array — whisper's input shape.

    Lazy import: faster-whisper pulls in PyAV, and this is only needed once a
    model exists anyway (streaming mode and its tail pass slice numpy arrays).
    """
    from faster_whisper.audio import decode_audio
    return decode_audio(path, sampling_rate=16000)


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
    """WM_CLASS heuristic via python-xlib (already pulled in by pynput).

    The input focus usually sits on a *child* of the real client window —
    GTK/Qt hand focus to an internal widget window — and only the top-level
    client window carries WM_CLASS. So walk up the ancestors to the first
    window that has one and judge by that.
    """
    try:
        from Xlib import display as xdisplay
    except ImportError:
        return False
    d = None
    try:
        d = xdisplay.Display()
        win = d.get_input_focus().focus
        for _ in range(16):  # way past focus child -> client window -> root
            if not hasattr(win, "get_wm_class"):
                # X.NONE / PointerRoot come back as plain ints, not windows.
                # PointerRoot means focus-follows-mouse with no focused
                # window: nothing to judge by, so pacing stays on.
                return False
            wm_class = win.get_wm_class() or ()
            if wm_class:
                return any(c.lower() in _TERMINAL_WM_CLASSES
                           for c in wm_class if c)
            parent = win.query_tree().parent
            # The root window's parent comes back as int 0 (X.NONE), not a
            # window; python-xlib hands out a fresh Window object per query,
            # so guard the self-parenting case by id, not by identity.
            if not hasattr(parent, "get_wm_class") or parent.id == win.id:
                return False
            win = parent
        return False
    except Exception:
        return False
    finally:
        if d is not None:
            d.close()


def _x11_clipboard_read(timeout=2.0):
    """Current X11 CLIPBOARD text as bytes, or None.

    None covers: no owner, the owner not offering UTF8_STRING, or no answer
    within `timeout`. Reading means asking the owner for the selection, so it
    needs its own short-lived connection and window.
    """
    from Xlib import X, display as xdisplay
    d = xdisplay.Display()
    try:
        clipboard = d.get_atom("CLIPBOARD")
        if d.get_selection_owner(clipboard) == X.NONE:
            return None
        utf8 = d.get_atom("UTF8_STRING")
        prop_atom = d.get_atom("WHISPER_PTT_CLIPBOARD_READ")
        win = d.screen().root.create_window(
            0, 0, 1, 1, 0, X.CopyFromParent, X.InputOutput, X.CopyFromParent)
        win.convert_selection(clipboard, utf8, prop_atom, X.CurrentTime)
        d.flush()
        end = time.time() + timeout
        while time.time() < end:
            # Poll instead of select(): python-xlib reads the socket ahead
            # into its internal queue, where select() cannot see the data.
            if d.pending_events() == 0:
                time.sleep(0.05)
                continue
            ev = d.next_event()
            if ev.type != X.SelectionNotify:
                continue
            if ev.property == X.NONE:
                return None  # the owner does not offer UTF8_STRING
            prop = win.get_full_property(prop_atom, X.AnyPropertyType)
            win.delete_property(prop_atom)
            if prop is None or prop.format != 8:
                # e.g. an INCR size marker for a huge clipboard: unreadable
                # here, so restoring it is skipped rather than corrupted
                return None
            return bytes(prop.value)
        return None
    finally:
        d.close()


def _x11_clipboard_own(data, serve_seconds=None):
    """Own the X11 CLIPBOARD selection holding `data` (bytes).

    Owning a selection means answering SelectionRequest events for it, which
    runs on a daemon thread until another client takes the selection over
    (SelectionClear) or serve_seconds pass — the default None serves
    indefinitely, mirroring how wl-copy/xclip stay alive: closing the
    connection would destroy the owner window and the clipboard would revert
    to no-owner. The thread closes the connection when it ends; the caller
    must not touch it afterwards. Returns the serving thread.
    """
    from Xlib import X, Xatom, display as xdisplay
    from Xlib.protocol import event as xevent
    d = xdisplay.Display()
    clipboard = d.get_atom("CLIPBOARD")
    utf8 = d.get_atom("UTF8_STRING")
    targets = d.get_atom("TARGETS")
    win = d.screen().root.create_window(
        0, 0, 1, 1, 0, X.CopyFromParent, X.InputOutput, X.CopyFromParent)
    try:
        win.set_selection_owner(clipboard, X.CurrentTime)
        d.sync()
        if d.get_selection_owner(clipboard).id != win.id:
            raise RuntimeError("could not take the CLIPBOARD selection")
    except Exception:
        d.close()
        raise

    def serve():
        try:
            end = None if serve_seconds is None else time.time() + serve_seconds
            while end is None or time.time() < end:
                if d.pending_events() == 0:  # poll: see _x11_clipboard_read
                    time.sleep(0.1)
                    continue
                ev = d.next_event()
                if ev.type == X.SelectionClear:
                    break  # another client owns the selection now
                if ev.type != X.SelectionRequest:
                    continue
                try:
                    # obsolete clients signal "use the target as the property"
                    prop = ev.property if ev.property != X.NONE else ev.target
                    ok = True
                    if ev.target == targets:
                        ev.requestor.change_property(
                            prop, Xatom.ATOM, 32,
                            [targets, utf8, Xatom.STRING])
                    elif ev.target in (utf8, Xatom.STRING):
                        ev.requestor.change_property(prop, ev.target, 8, data)
                    else:
                        ok = False
                    ev.requestor.send_event(xevent.SelectionNotify(
                        time=ev.time, requestor=ev.requestor,
                        selection=ev.selection, target=ev.target,
                        property=prop if ok else X.NONE))
                    d.sync()
                except Exception as exc:
                    # e.g. the requestor died mid-request: lose this answer,
                    # not the whole selection
                    log(f"X11 clipboard request failed: {exc}")
        except Exception as exc:
            # A serving failure loses this paste, never the daemon.
            log(f"X11 clipboard serving stopped: {exc}")
        finally:
            d.close()

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    return thread


class PynputInjector:
    """Type the text out via pynput (XTEST on X11, native APIs elsewhere).

    Layout-independent, no extra setup. Characters go one at a time with a
    small delay: heavy apps (Electron editors, browsers) drop or reorder
    keystrokes fired at full speed.

    X11 only: a character the server cannot map to a key cannot be typed at
    all. pynput works around unmappable characters by borrowing an unused
    keycode row, but some keymaps have none (GNOME fills every row with XF86
    keysyms) — and then CJK text fails hard. Such text goes in through the
    clipboard plus a synthetic paste chord instead, like the Wayland backend.
    """

    def __init__(self):
        self.controller = None
        self._borrowable = None  # X11: does the keymap have an unused row?

    def check(self):
        return None

    def insert(self, text, chord=None):
        # The chord override matters only for the clipboard path (Wayland
        # always pastes; on X11 only text with untypeable characters does).
        # Typed text has no chord to swap; held-modifier caveats for typed
        # text stay as documented.
        if self.controller is None:
            from pynput.keyboard import Controller
            self.controller = Controller()
        if self._needs_clipboard(text) and self._paste_text(text, chord):
            return
        delay = TYPE_DELAY
        if delay and TERMINAL_TYPE_DELAY < delay and foreground_window_is_terminal():
            delay = TERMINAL_TYPE_DELAY
        for ch in text:
            key = _CONTROL_KEYS.get(ch, ch)
            try:
                self.controller.press(key)
                self.controller.release(key)
            except Exception as exc:
                # e.g. pynput's InvalidKeyException: one unmappable character
                # must not abort the rest of the dictation
                log(f"skipped untypeable character {ch!r}: {exc}")
            if delay:
                time.sleep(delay)

    def _needs_clipboard(self, text):
        """X11 with a character pynput cannot map → paste, don't type."""
        if not (sys.platform.startswith("linux") and os.environ.get("DISPLAY")
                and os.environ.get("XDG_SESSION_TYPE") != "wayland"):
            return False
        try:
            # keyboard_mapping/_borrows/_display are private pynput attrs;
            # if a future pynput renames them the AttributeError degrades to
            # the typed path, which is the safe side
            mapping = self.controller.keyboard_mapping
            borrows = self.controller._borrows
            for ch in text:
                if ch in _CONTROL_KEYS:
                    continue
                ordinal = ord(ch)
                keysym = ordinal if ordinal < 0x100 else ordinal | 0x01000000
                if keysym in mapping or keysym in borrows:
                    continue
                if self._can_borrow():
                    continue
                return True
        except Exception:
            return False  # introspection failed: keep the typed path
        return False

    def _can_borrow(self):
        """Whether pynput can still remap a keycode row for unmappable
        characters — a property of the keymap, probed once."""
        if self._borrowable is None:
            mapping = self.controller._display.get_keyboard_mapping(8, 255 - 8)
            self._borrowable = any(not any(row) for row in mapping)
        return self._borrowable

    def _chord_keys(self, chord):
        """'ctrl+shift+v' -> keys for controller.press, press order.

        Chord names resolve through the module-level _CHORD_KEYS (real
        pynput Keys where available); anything else — 'v', and modifier
        names the backend does not know — passes through as-is, which for
        single characters is exactly what controller.press expects.
        """
        keys = []
        for part in chord.lower().split("+"):
            part = part.strip()
            if not part:
                continue
            keys.append(_CHORD_KEYS.get(part, part))
        return keys

    def _paste_text(self, text, chord=None):
        """Clipboard fallback for untypeable characters: own CLIPBOARD, one
        synthetic paste chord (ctrl+shift+v in terminals, ctrl+v elsewhere;
        the streaming mid-hold override wins), then restore the previous
        contents. False when the clipboard could not be set up — the caller
        then types what it can."""
        try:
            previous = _x11_clipboard_read()
            # When a restore follows, the transcript only has to survive until
            # then; otherwise it stays pasteable (until the next copy), like
            # wl-copy leaves it on the Wayland backend.
            _x11_clipboard_own(
                text.encode("utf-8"),
                serve_seconds=CLIPBOARD_SETTLE + 2.0 if previous is not None
                else None)
        except Exception as exc:
            log(f"X11 clipboard paste unavailable: {exc}")
            return False
        if chord is None:
            chord = "ctrl+shift+v" if foreground_window_is_terminal() \
                else "ctrl+v"
        keys = self._chord_keys(chord)
        for key in keys:
            self.controller.press(key)
        for key in reversed(keys):
            self.controller.release(key)
        # The paste is asynchronous: give the focused app time to fetch the
        # selection before the clipboard is handed back.
        time.sleep(CLIPBOARD_SETTLE)
        if previous is not None:
            try:
                # served until another client takes the selection over —
                # a bounded serve would destroy the owner window on exit and
                # the "restored" clipboard would vanish shortly after
                _x11_clipboard_own(previous)
            except Exception as exc:
                # Only the restore failed — the transcript is already
                # inserted, so the clipboard simply keeps it.
                log(f"clipboard restore failed: {exc}")
        return True

    def leave_in_clipboard(self, text):
        """Put text on the clipboard without pasting and without restoring —
        the recovery copy for streamed blocks a target could not paste
        mid-hold (see _leave_recovery_transcript)."""
        _x11_clipboard_own(text.encode("utf-8"))

    def tap_caps_lock(self):
        """Toggle the caps lock state once.

        The daemon calls this after a dictation on the Caps Lock key: the
        hold also flipped the system caps state on press, and the flip-back
        restores it. A short tap never reaches here and keeps its normal
        caps meaning."""
        if self.controller is None:
            from pynput.keyboard import Controller
            self.controller = Controller()
        key = _CHORD_KEYS.get("caps_lock")
        if key is None:
            log("caps lock restore unavailable: pynput has no caps_lock key")
            return
        self.controller.press(key)
        time.sleep(0.05)  # too-brief taps can be debounced away (macOS)
        self.controller.release(key)


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
        self._chord_args = {}  # chord string -> ydotool argv
        self._chord_style = None  # ydotool key syntax, probed once
        self._copy_proc = None  # the wl-copy currently owning the clipboard

    @staticmethod
    def _socket_path():
        """The socket ydotool will talk to, resolved the same way ydotool does.

        Resolving it ourselves (rather than only honouring an explicitly set
        YDOTOOL_SOCKET) means a manual `python dictate.py` run validates the
        socket up front, instead of failing later inside ydotool with a less
        obvious message.

        Older ydotoold versions (e.g. the 0.1.x packages in Ubuntu 24.04)
        ignore --socket-path and always create /tmp/.ydotool_socket, so we
        fall back to that location when the runtime-dir socket is absent.
        """
        explicit = os.environ.get("YDOTOOL_SOCKET")
        if explicit:
            return explicit
        runtime = os.environ.get("XDG_RUNTIME_DIR")
        candidates = []
        if runtime:
            candidates.append(os.path.join(runtime, ".ydotool_socket"))
        candidates.append("/tmp/.ydotool_socket")
        # Prefer the runtime-dir default, but accept the legacy path when
        # ydotoold is an old version that ignores --socket-path. os.access:
        # on a multi-user machine /tmp/.ydotool_socket may belong to another
        # user's session — it exists, but we could never connect to it.
        for path in candidates:
            if os.access(path, os.W_OK):
                return path
        return candidates[0] if candidates else None

    def check(self):
        """Return a human-readable problem description, or None if usable."""
        for tool in ("wl-copy", "wl-paste", "ydotool"):
            if not shutil.which(tool):
                return f"{tool} not found (install wl-clipboard and ydotool)"
        try:
            # probe once here: insert() skips its lazy probe whenever the
            # style is already cached
            self._chord_style = self._ydotool_key_style()
            self._chord_args = {self.chord:
                                self._build_chord(self.chord, self._chord_style)}
        except ValueError as exc:
            return str(exc)
        socket = self._socket_path()
        if socket is None or not os.access(socket, os.W_OK):
            return (f"ydotoold socket {socket} missing or not writable — "
                    "systemctl --user start ydotoold.service")
        return None

    @staticmethod
    def _ydotool_key_style():
        """Detect whether the installed ydotool expects KEYCODE:STATE (1.x)
        or key-name sequences like 'ctrl+v' (0.1.x).

        Runs a probe subprocess on every call — in practice once per daemon,
        in check() at startup. Deliberately not cached with lru_cache:
        the tests probe with different monkeypatched runs, and a cache would
        leak the first result into the later ones.
        """
        try:
            proc = subprocess.run(["ydotool", "key", "--help"],
                                  capture_output=True, text=True,
                                  timeout=5)
            out = proc.stdout + proc.stderr
        except (OSError, subprocess.SubprocessError):
            out = ""
        # ydotool 0.1.x help says: "modifiers and keys, separated by plus (+)".
        # ydotool 1.x help describes KEYCODE:STATE pairs.
        if "plus (+)" in out and "KEYCODE" not in out:
            return "name"
        return "code"

    @staticmethod
    def _build_chord(chord, style=None):
        """'ctrl+shift+v' -> ydotool key arguments, press then release.

        style ('name' for ydotool 0.1.x, 'code' for 1.x) is probed when not
        given; callers that already probed pass it in to skip the subprocess.
        """
        from evdev import ecodes
        names = {"ctrl": "KEY_LEFTCTRL", "control": "KEY_LEFTCTRL",
                 "shift": "KEY_LEFTSHIFT", "alt": "KEY_LEFTALT",
                 "super": "KEY_LEFTMETA", "meta": "KEY_LEFTMETA"}
        # The spellings ydotool 0.1.x accepts for modifiers; anything its key
        # table does not know silently degrades to the token's first letter.
        display_names = {"KEY_LEFTCTRL": "ctrl", "KEY_LEFTSHIFT": "shift",
                         "KEY_LEFTALT": "alt", "KEY_LEFTMETA": "super"}
        codes = []
        display_parts = []
        for part in chord.lower().split("+"):
            part = part.strip()
            if not part:
                continue
            evname = names.get(part) or (
                # part is already lowercased, so KEY_ arrives as 'key_'
                part.upper() if part.startswith("key_") else "KEY_" + part.upper())
            code = getattr(ecodes, evname, None)
            if code is None:
                raise ValueError(f"unknown key '{part}' in PTT_PASTE_KEY={chord}")
            codes.append(code)
            display = display_names.get(evname)
            if display is None:
                display = (evname[4:].lower() if evname.startswith("KEY_")
                           else part)
            display_parts.append(display)
        if not codes:
            raise ValueError("PTT_PASTE_KEY is empty")
        # ydotool 0.1.x (Ubuntu 24.04 package) expects key-name sequences like
        # 'ctrl+v'. ydotool 1.x expects KEYCODE:STATE pairs.
        if (style or WaylandInjector._ydotool_key_style()) == "name":
            return ["+".join(display_parts)]
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

    def insert(self, text, chord=None):
        """Paste `text` via the clipboard plus one synthetic chord.

        chord overrides the configured paste chord for this insert. Used by
        streaming mode: with AltGr as the PTT key the held level-3 modifier
        remaps letter keysyms at the compositor (de layout: AltGr+V = „), so
        a letter chord never matches mid-hold — Insert is level-stable and
        Shift+Insert pastes in GTK and Qt apps (not in VTE terminals, which
        only take a letter chord — hence the recovery transcript).
        """
        chord = chord or self.chord
        args = self._chord_args.get(chord)
        if args is None:
            if self._chord_style is None:
                # probe once and keep it — building must not re-run a
                # `ydotool key --help` subprocess on every insert
                self._chord_style = self._ydotool_key_style()
            args = self._build_chord(chord, self._chord_style)
            self._chord_args[chord] = args
        env = dict(os.environ)
        socket = self._socket_path()
        if socket:
            env["YDOTOOL_SOCKET"] = socket  # keep client and daemon in sync
        previous = None if self.keep_clipboard else self._clipboard_read()
        self._clipboard_write(text.encode("utf-8"))
        subprocess.run(["ydotool", "key"] + args,
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

    def leave_in_clipboard(self, text):
        """Put text on the clipboard without pasting and without restoring —
        the recovery copy for blocks a target could not paste mid-hold."""
        self._clipboard_write(text.encode("utf-8"))

    def tap_caps_lock(self):
        """Toggle the caps lock state once through ydotool.

        The daemon calls this after a dictation on the Caps Lock key: the
        hold also flipped the system caps state on press, and the flip-back
        restores it. The synthetic press comes out of the ydotoold device,
        which the evdev listener ignores — it cannot look like a dictation.
        """
        args = self._chord_args.get("caps_lock")
        if args is None:
            if self._chord_style is None:
                self._chord_style = self._ydotool_key_style()
            # the evdev name: _build_chord's pynput spelling would produce
            # the nonexistent KEY_CAPS_LOCK
            args = self._build_chord("KEY_CAPSLOCK", self._chord_style)
            self._chord_args["caps_lock"] = args
        env = dict(os.environ)
        socket = self._socket_path()
        if socket:
            env["YDOTOOL_SOCKET"] = socket  # keep client and daemon in sync
        subprocess.run(["ydotool", "key"] + args,
                       timeout=CMD_TIMEOUT, env=env,
                       stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True)


def _streaming_paste_chord():
    """Paste chord for mid-hold streaming inserts, or None for the normal one.

    With AltGr as the PTT key the held level-3 modifier remaps letter keysyms
    at the compositor (de layout: AltGr+V = „), so a letter chord never
    matches while the key is held — and a synthetic release cannot clear a
    *physically* held modifier, whose state unions with ours. Insert is
    level-stable in every keymap, and Shift+Insert pastes in GTK and Qt apps
    (VTE terminals are the exception: they only paste via a letter chord).
    """
    if PTT_KEY == "alt_gr":
        return "shift+insert"
    return None


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
            if dev.name == "ydotoold virtual device":
                # Our own paste chords come out of this device — listening to
                # it would make the synthetic key events look like real PTT
                # key presses.
                dev.close()
                continue
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

class _StreamSession:
    """Per-recording streaming state (one key hold), PTT_STREAMING=1 only.

    committed_end: seconds of audio already transcribed and inserted by the
    streaming ticks; the release pass only transcribes the tail after it.
    texts: the inserted block texts, so the release pass can leave a complete
    transcript on the clipboard for targets that could not paste mid-hold.
    """
    def __init__(self):
        self.stop = threading.Event()
        self.thread = None
        self.committed_end = 0.0
        self.inserted = False
        self.texts = []


class DictationDaemon:
    def __init__(self):
        self.model = None
        self.recorder = pick_recorder()
        self.recording = None  # start_time while a recording is active
        self.busy_lock = threading.Lock()
        self._stream = None  # _StreamSession while a streaming dictation runs
        self._caps_restore_until = 0.0  # time backstop for the tap suppression
        self._caps_restore_pending = 0  # synthetic caps taps still in flight
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
        if (self._caps_restore_pending
                and time.time() < self._caps_restore_until):
            # exactly one press per restore: the synthetic caps tap. A real
            # re-press right after must still work — and must not be eaten.
            self._caps_restore_pending -= 1
            return
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
        if STREAMING and hasattr(self.recorder, "snapshot_wav"):
            session = _StreamSession()
            session.thread = threading.Thread(target=self._stream_loop,
                                              args=(session,), daemon=True)
            session.thread.start()  # before the assignment: a failed start
            self._stream = session  # must not leave a thread-less session

    def stop_recording(self):
        started, self.recording = self.recording, None
        if started is None:
            return
        duration = time.time() - started
        session, self._stream = self._stream, None
        if session is not None:
            # A tick may be mid-transcribe; the busy_lock inside keeps its
            # insert ordered before the tail pass below.
            session.stop.set()
            session.thread.join(timeout=30)
        if duration >= MIN_SECONDS and PTT_KEY == "caps_lock":
            # A hold long enough to dictate also toggled caps lock on the
            # press — flip it back, or every dictation leaves capitals on.
            # Duration alone decides: even a recording that fails below has
            # already flipped the state. A short tap never reaches here and
            # keeps its caps meaning. The synthetic tap looks like a fresh
            # PTT press to the pynput listener, so swallow exactly that one
            # (the evdev listener ignores the ydotoold device anyway).
            if backend_name() != "wayland":
                self._caps_restore_until = time.time() + 1.0
                self._caps_restore_pending = 1
            try:
                self.injector.tap_caps_lock()
            except Exception as exc:
                log(f"caps lock restore failed: {exc}")
        try:
            wav = self.recorder.stop()
        except Exception as exc:
            log(f"ERROR stopping recording: {exc}")
            notify("dictation error", str(exc)[:80])
            write_state("idle")
            return
        if duration < MIN_SECONDS or not wav or not os.path.isfile(wav) \
                or os.path.getsize(wav) < 1000:
            log(f"ignored short/empty recording ({duration:.2f}s)")
            if wav and os.path.exists(wav):
                os.remove(wav)
            write_state("idle")
            return
        threading.Thread(target=self._transcribe_and_insert,
                         args=(wav, duration, session), daemon=True).start()

    def _stream_loop(self, session):
        """Streaming mode: transcribe completed blocks while the key is held."""
        while not session.stop.wait(STREAM_INTERVAL):
            try:
                self._stream_tick(session)
            except Exception as exc:  # a tick must never kill the dictation
                log(f"streaming tick failed: {exc}")

    def _transcribe(self, source, lang):
        """model.transcribe with the Chinese script fix applied.

        Whisper's multilingual models mix Traditional characters into
        Mandarin output at random; a Simplified initial_prompt pins the
        script. A Chinese prompt would bias decoding for *every* language,
        though, so it goes into the single pass only when zh is pinned — on
        auto-detect it enters a second pass, and only when the first pass
        just detected Chinese. Other languages never pay for it.
        """
        prompt = ZH_PROMPT or None
        segments, info = self.model.transcribe(
            source, language=lang, vad_filter=True, beam_size=5,
            initial_prompt=prompt if lang == "zh" else None)
        if (lang is None and prompt
                and getattr(info, "language", None) == "zh"):
            log("auto-detected zh — re-decoding with the Simplified prompt")
            segments, _info = self.model.transcribe(
                source, language="zh", vad_filter=True, beam_size=5,
                initial_prompt=prompt)
        return segments

    def _stream_tick(self, session):
        wav = self.recorder.snapshot_wav()
        if not wav:
            return
        try:
            audio = _decode_wav_16k(wav)
        finally:
            try:
                os.remove(wav)
            except OSError:
                pass
        total = len(audio) / 16000.0
        end = total - STREAM_TAIL_GUARD
        if end - session.committed_end < STREAM_MIN_SLICE:
            return
        window = audio[int(session.committed_end * 16000):int(end * 16000)]
        # Runs outside busy_lock: the previous dictation's tail pass may be
        # transcribing concurrently — ctranslate2 is thread-safe per call.
        segments = self._transcribe(window, configured_lang())
        segs = [s for s in segments if s.text.strip()]
        if not segs:
            return
        window_dur = len(window) / 16000.0
        # Speech touching the slice edge is probably cut mid-word — leave that
        # segment for the next tick, which sees it complete with more context.
        if segs[-1].end >= window_dur - STREAM_TRAILING_SILENCE:
            segs = segs[:-1]
        if not segs:
            return
        if session.stop.is_set():
            # Released mid-tick: the tail pass re-covers this block, because
            # committed_end only advances after a successful insert.
            return
        text = " ".join(s.text.strip() for s in segs).strip()
        with self.busy_lock:
            self.injector.insert(text + " ", chord=_streaming_paste_chord())
        session.committed_end += segs[-1].end
        session.inserted = True
        session.texts.append(text)
        log(f"streamed {len(text)} chars "
            f"(audio committed up to {session.committed_end:.1f}s)")

    def _leave_recovery_transcript(self, stream, tail_text):
        """Park the complete transcript of a streamed dictation on the
        clipboard when the PTT key is a remapping modifier (AltGr).

        Such mid-hold blocks can never reach terminals: those only paste via
        a letter chord, which the held modifier remaps. Without this copy the
        blocks would be silently lost. Runs only for streamed sessions; a
        failure here must never fail the dictation itself.
        """
        if (stream is None or not stream.inserted
                or _streaming_paste_chord() is None):
            return
        parts = stream.texts + ([tail_text] if tail_text else [])
        full = " ".join(parts).strip()
        leave = getattr(self.injector, "leave_in_clipboard", None)
        if not full or leave is None:
            return
        try:
            leave(full)
        except Exception as exc:  # the dictation itself is already inserted
            log(f"recovery transcript failed: {exc}")
            return
        log("streamed dictation: full transcript left on the clipboard "
            "(recovery for mid-hold blocks)")
        notify("transcript on clipboard",
               "mid-hold blocks may be missing — paste with Ctrl+Shift+V")

    def _transcribe_and_insert(self, wav, duration, stream=None):
        with self.busy_lock:
            write_state("transcribing")
            try:
                notify("… transcribing", "")
                lang = configured_lang()
                t0 = time.time()
                skip = stream.committed_end if stream else 0.0
                if skip > 0:
                    # Streaming ticks already transcribed and inserted up to
                    # here — only the tail since then is left.
                    audio = _decode_wav_16k(wav)
                    audio = audio[int(skip * 16000):]
                    segments = self._transcribe(audio, lang)
                else:
                    segments = self._transcribe(wav, lang)
                text = " ".join(s.text.strip() for s in segments).strip()
                model = CURRENT_MODEL or "?"
                log(f"transcribed {duration:.1f}s audio -> {len(text)} chars "
                    f"in {time.time() - t0:.1f}s ({model}, lang={lang or 'auto'})"
                    + (f" [tail after {skip:.1f}s streamed]" if skip > 0 else ""))
                if not text:
                    if stream is not None and stream.inserted:
                        # blocks already went out; an empty tail is fine —
                        # but the recovery copy still applies
                        self._leave_recovery_transcript(stream, "")
                        return
                    rms = _audio_rms(wav)
                    if rms is not None and rms < 0.001:
                        # a dead or disconnected input device records digital
                        # silence — say so, "nothing recognized" sends users
                        # down the wrong path
                        log(f"nothing heard — mic silent (rms={rms:.5f})")
                        notify("nothing heard",
                               "the mic recorded silence — check the input device")
                    else:
                        notify("dictation", "nothing recognized")
                    return
                time.sleep(0.15)  # let the modifier release settle
                self.injector.insert(text + " ")  # space separates dictations
                self._leave_recovery_transcript(stream, text)
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
