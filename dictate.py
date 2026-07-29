#!/usr/bin/env python3
"""whisper-ptt: push-to-talk dictation for any app, fully local.

Hold the push-to-talk key (default: Right Ctrl), speak, release — the
transcript lands in the focused window (terminal, editor, browser, ...).
Nothing is submitted automatically; review and hit Enter yourself.

- Recording: pw-record (PipeWire) from the default source, 16 kHz mono.
- Transcription: faster-whisper, local — GPU (CUDA) when available, else CPU.
  After the one-time model download nothing leaves the machine.
- Key grab and text insertion are backend-specific:
    X11      pynput/XTEST for both. No group membership, no root daemon.
    Wayland  evdev reads the key from /dev/input (needs the 'input' group);
             text goes through the clipboard and a single synthetic Ctrl+V
             via ydotool. Typing it out keycode-by-keycode is not an option:
             ydotool assumes a US layout, so on a 'de' keymap 'z' would come
             out as 'y' and umlauts not at all. The clipboard carries UTF-8,
             so only one layout-stable chord has to be synthesized.
- Log stores metadata only (duration, char count), never dictated text.

Config via environment:
  PTT_KEY          key name (default: ctrl_r; e.g. f9, caps_lock, or a raw
                   evdev name like KEY_RIGHTCTRL)
  WHISPER_MODEL    model size (default: medium on GPU, small on CPU)
  WHISPER_LANG     language code (default: de; empty string = auto-detect)
  PTT_BACKEND      force 'x11' or 'wayland' (default: from XDG_SESSION_TYPE)
  PTT_PASTE_KEY    Wayland paste chord (default: ctrl+v; terminals usually
                   need ctrl+shift+v)
  PTT_KEEP_CLIPBOARD  1 = leave the transcript in the clipboard instead of
                   restoring the previous contents

Run:  .venv/bin/python dictate.py        (usually via whisper-ptt.service)
Stop: Ctrl-C, or systemctl --user stop whisper-ptt.service
"""
from __future__ import annotations

import glob
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime

STATE_DIR = os.path.join(os.environ.get("XDG_STATE_HOME",
                                        os.path.expanduser("~/.local/state")),
                         "whisper-ptt")
LOG_PATH = os.path.join(STATE_DIR, "dictate.log")
PTT_KEY = os.environ.get("PTT_KEY", "ctrl_r")
MIN_SECONDS = 0.5  # shorter recordings count as accidental taps
CLIPBOARD_SETTLE = 0.4  # let the compositor deliver the paste before restoring


def log(msg):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG_PATH, "a", encoding="utf-8") as fh:
        fh.write(f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}\n")


def notify(title, body=""):
    if shutil.which("notify-send"):
        subprocess.Popen(["notify-send", "-t", "2000", title, body],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def backend_name():
    forced = os.environ.get("PTT_BACKEND")
    if forced:
        return forced
    return "wayland" if os.environ.get("XDG_SESSION_TYPE") == "wayland" else "x11"


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
            libs += sorted(glob.glob(os.path.join(pkg_dir, "lib", "*.so*")))
        for lib in libs:
            try:
                ctypes.CDLL(lib, mode=ctypes.RTLD_GLOBAL)
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

class X11Injector:
    """Synthesize the text via XTEST. Layout-independent, no extra setup."""

    def __init__(self):
        self._controller = None

    def check(self):
        return None

    def insert(self, text):
        if self._controller is None:
            from pynput.keyboard import Controller
            self._controller = Controller()
        self._controller.type(text)


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
                capture_output=True, timeout=5)
            return out.stdout if out.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            return None

    def _clipboard_write(self, data: bytes):
        subprocess.run(["wl-copy", "--type", "text/plain"], input=data,
                       timeout=5, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)

    def insert(self, text):
        if self._chord_args is None:
            self._chord_args = self._build_chord(self.chord)
        env = dict(os.environ)
        socket = self._socket_path()
        if socket:
            env["YDOTOOL_SOCKET"] = socket  # keep client and daemon in sync
        previous = None if self.keep_clipboard else self._clipboard_read()
        self._clipboard_write(text.encode("utf-8"))
        subprocess.run(["ydotool", "key"] + self._chord_args, timeout=10,
                       env=env, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL, check=True)
        if previous is not None:
            # Best effort: the paste is asynchronous, so give the focused app
            # time to fetch the selection before handing the clipboard back.
            time.sleep(CLIPBOARD_SETTLE)
            try:
                self._clipboard_write(previous)
            except (OSError, subprocess.SubprocessError):
                pass


# --------------------------------------------------------------------------
# Key listener backends
# --------------------------------------------------------------------------

class X11Listener:
    def __init__(self, key_name):
        self.key_name = key_name

    def check(self):
        from pynput import keyboard
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
            while True:
                for ready, _mask in selector.select():
                    for event in ready.fileobj.read():
                        if event.type != ecodes.EV_KEY or event.code != self.code:
                            continue
                        if event.value == 1:
                            on_press()
                        elif event.value == 0:
                            on_release()
                        # value 2 is auto-repeat while held — ignore it
        finally:
            selector.close()
            for dev in devices:
                dev.close()


def make_backends(key_name):
    if backend_name() == "wayland":
        return EvdevListener(key_name), WaylandInjector()
    return X11Listener(key_name), X11Injector()


# --------------------------------------------------------------------------

class DictationDaemon:
    def __init__(self):
        self.model = None
        self.recording = None  # (Popen, wav_path, start_time)
        self.busy_lock = threading.Lock()
        self.listener, self.injector = make_backends(PTT_KEY)

    def load_model(self):
        device, compute, default_model = pick_device()
        name = os.environ.get("WHISPER_MODEL") or default_model
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
        log(f"model loaded: {name}/{device}")

    def start_recording(self):
        if self.recording is not None:
            return  # ignore key auto-repeat
        if not shutil.which("pw-record"):
            notify("dictation error", "pw-record not found (install pipewire)")
            log("ERROR: pw-record missing")
            return
        wav = os.path.join(tempfile.gettempdir(),
                           f"whisper-ptt-{int(time.time() * 1000)}.wav")
        proc = subprocess.Popen(
            ["pw-record", "--rate", "16000", "--channels", "1", wav],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.recording = (proc, wav, time.time())
        notify("● recording", f"(release {PTT_KEY} to transcribe)")

    def stop_recording(self):
        rec, self.recording = self.recording, None
        if rec is None:
            return
        proc, wav, started = rec
        duration = time.time() - started
        proc.send_signal(signal.SIGINT)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        if duration < MIN_SECONDS or not os.path.isfile(wav) \
                or os.path.getsize(wav) < 1000:
            log(f"ignored short/empty recording ({duration:.2f}s)")
            os.path.exists(wav) and os.remove(wav)
            return
        threading.Thread(target=self._transcribe_and_insert,
                         args=(wav, duration), daemon=True).start()

    def _transcribe_and_insert(self, wav, duration):
        with self.busy_lock:
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

    def run(self):
        which = backend_name()
        for component in (self.listener, self.injector):
            problem = component.check()
            if problem:
                sys.exit(f"{which} backend unusable: {problem}")
        self.load_model()
        notify("dictation ready", f"hold {PTT_KEY} and speak")
        print(f"push-to-talk ready on '{PTT_KEY}' ({which}) — hold to record",
              file=sys.stderr)
        log(f"ready on {PTT_KEY} ({which} backend)")
        self.listener.listen(self.start_recording, self.stop_recording)


def main():
    try:
        DictationDaemon().run()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
