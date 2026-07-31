#!/usr/bin/env python3
"""whisper-ptt: push-to-talk dictation, fully local — Linux, Windows, macOS.

Hold the push-to-talk key (default: Right Ctrl), speak, release — the
transcript is typed into the focused window (terminal, editor, browser, ...).
Nothing is submitted automatically; review and hit Enter yourself.

- Recording: pw-record (PipeWire) on Linux where available, otherwise
  sounddevice/PortAudio (Windows WASAPI, macOS Core Audio) — see recorder.py.
- Transcription: faster-whisper, local — GPU (CUDA) when available, else CPU.
  After the one-time model download nothing leaves the machine.
- Typing: pynput. No sudo, no root daemon. On Linux this needs an X11
  session; on macOS grant the terminal Accessibility + Input Monitoring.
- Log stores metadata only (duration, char count), never dictated text.

Config via environment:
  PTT_KEY          pynput key name (default: ctrl_r; e.g. f9, caps_lock)
  WHISPER_MODEL    model size (default: medium on GPU, small on CPU)
  WHISPER_LANG     language code (default: de; empty string = auto-detect)
  PTT_TYPE_DELAY   seconds between typed characters (default: 0.01); heavy
                   editors drop/reorder fast synthetic keystrokes — raise it
                   (e.g. 0.03) if dictation arrives garbled

Run:  .venv/bin/python dictate.py        (usually via service/scheduled task)
Stop: Ctrl-C, or stop the service (systemctl / Task Scheduler / launchctl)
Note: Linux Wayland needs a different backend (ydotool/evdev).
"""
from __future__ import annotations

import glob
import json
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
LOG_PATH = os.path.join(STATE_DIR, "dictate.log")
PTT_KEY = os.environ.get("PTT_KEY", "ctrl_r")
try:
    TYPE_DELAY = float(os.environ.get("PTT_TYPE_DELAY", "0.01"))
except ValueError:
    TYPE_DELAY = 0.01
MIN_SECONDS = 0.5  # shorter recordings count as accidental taps


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


class DictationDaemon:
    def __init__(self):
        self.model = None
        self.recorder = pick_recorder()
        self.recording = None  # start_time while a recording is active
        self.busy_lock = threading.Lock()
        self.controller = None

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
            return
        if duration < MIN_SECONDS or not wav or os.path.getsize(wav) < 1000:
            log(f"ignored short/empty recording ({duration:.2f}s)")
            if wav and os.path.exists(wav):
                os.remove(wav)
            return
        threading.Thread(target=self._transcribe_and_type,
                         args=(wav, duration), daemon=True).start()

    def _transcribe_and_type(self, wav, duration):
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
                self._type_text(text + " ")  # space separates dictations
            except Exception as exc:
                log(f"ERROR: {exc}")
                notify("dictation error", str(exc)[:80])
            finally:
                try:
                    os.remove(wav)
                except OSError:
                    pass

    def _type_text(self, text):
        # Type char-by-char with a small delay: heavy apps (Electron editors,
        # browsers) drop or reorder keystrokes fired at full speed.
        if self.controller is None:
            from pynput.keyboard import Controller
            self.controller = Controller()
        for ch in text:
            self.controller.press(ch)
            self.controller.release(ch)
            if TYPE_DELAY:
                time.sleep(TYPE_DELAY)

    def run(self):
        from pynput import keyboard
        key = getattr(keyboard.Key, PTT_KEY, None)
        if key is None:
            sys.exit(f"unknown PTT_KEY: {PTT_KEY} (see pynput.keyboard.Key)")
        self.load_model()
        notify("dictation ready", f"hold {PTT_KEY} and speak")
        print(f"push-to-talk ready on '{PTT_KEY}' — hold to record", file=sys.stderr)

        def on_press(k):
            if k == key:
                self.start_recording()

        def on_release(k):
            if k == key:
                self.stop_recording()

        with keyboard.Listener(on_press=on_press, on_release=on_release) as listener:
            listener.join()


def main():
    try:
        DictationDaemon().run()
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
