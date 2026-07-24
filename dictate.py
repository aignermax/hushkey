#!/usr/bin/env python3
"""whisper-ptt: push-to-talk dictation for any X11 app, fully local.

Hold the push-to-talk key (default: Right Ctrl), speak, release — the
transcript is typed into the focused window (terminal, editor, browser, ...).
Nothing is submitted automatically; review and hit Enter yourself.

- Recording: pw-record (PipeWire) from the default source, 16 kHz mono.
- Transcription: faster-whisper, local — GPU (CUDA) when available, else CPU.
  After the one-time model download nothing leaves the machine.
- Typing: pynput (X11/XTEST). No sudo, no root daemon.
- Log stores metadata only (duration, char count), never dictated text.

Config via environment:
  PTT_KEY          pynput key name (default: ctrl_r; e.g. f9, caps_lock)
  WHISPER_MODEL    model size (default: medium on GPU, small on CPU)
  WHISPER_LANG     language code (default: de; empty string = auto-detect)

Run:  .venv/bin/python dictate.py        (usually via whisper-ptt.service)
Stop: Ctrl-C, or systemctl --user stop whisper-ptt.service
Note: X11 only. Wayland needs a different backend (ydotool/evdev).
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


def log(msg):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG_PATH, "a", encoding="utf-8") as fh:
        fh.write(f"{datetime.now():%Y-%m-%d %H:%M:%S} {msg}\n")


def notify(title, body=""):
    if shutil.which("notify-send"):
        subprocess.Popen(["notify-send", "-t", "2000", title, body],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


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


class DictationDaemon:
    def __init__(self):
        self.model = None
        self.recording = None  # (Popen, wav_path, start_time)
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
                if self.controller is None:
                    from pynput.keyboard import Controller
                    self.controller = Controller()
                self.controller.type(text + " ")  # space separates dictations
            except Exception as exc:
                log(f"ERROR: {exc}")
                notify("dictation error", str(exc)[:80])
            finally:
                try:
                    os.remove(wav)
                except OSError:
                    pass

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
