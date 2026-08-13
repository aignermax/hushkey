#!/usr/bin/env python3
"""hushkey tray companion: status icon, daemon supervisor, update checks.

Runs the dictation daemon (dictate.py) as a child process and puts the husky
in the system tray, so you can see at a glance whether hushkey is idle,
recording or transcribing — and restart or update it from the menu.

The daemon publishes its state through a small JSON file (write_state() in
dictate.py); the tray only polls that file, so a crashing daemon can never
take the icon down with it. If the tray cannot be shown at all (pystray
missing, no notification area — e.g. bare GNOME/Wayland), the daemon is still
supervised, just without an icon.

Update checks query the GitHub releases API once at startup and then daily
(disable with PTT_UPDATE_CHECK=0). Updates are never installed silently, and
one menu click splits the work into two phases so a running tray never locks
a file the installer replaces (a loaded pillow .pyd is locked on Windows):
phase 1 only swaps the source files; phase 2 (the installer with its pip
installs) is run by update_helper.py — a detached one-shot process that also
brings the new tray up, so a failed handover never leaves nothing running.
If that helper never gets spawned, the next tray start completes a pending
update itself (run_pending_update_if_any).

On Windows, a small always-on-top pill at the top of the screen shows
recording/transcribing state — the indicator the (auto-hidden) taskbar
swallows. PTT_OVERLAY=0 turns it off; PTT_OVERLAY=1 enables it on X11.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.request

DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON = os.path.join(DIR, "dictate.py")
LOGO = os.path.join(DIR, "assets", "logo.png")
REPO = "aignermax/hushkey"
RELEASES_LATEST_URL = f"https://api.github.com/repos/{REPO}/releases/latest"
UPDATE_CHECK_INTERVAL = 4 * 60 * 60  # every 4 h (+ once at startup)
ICON_SIZE = 64

sys.path.insert(0, DIR)
import dictate  # noqa: E402  local module: single source for VERSION/STATE_DIR

VERSION = dictate.VERSION
STATE_PATH = dictate.STATE_PATH
CONFIG_PATH = dictate.CONFIG_PATH
UPDATE_LOG = os.path.join(dictate.STATE_DIR, "update.log")
UPDATE_PENDING = os.path.join(dictate.STATE_DIR, "update-pending")

# (name, download size) shown in the Model submenu
MODELS = [
    ("tiny", "~75 MB"),
    ("base", "~150 MB"),
    ("small", "~500 MB"),
    ("medium", "~1.5 GB"),
    ("large-v3", "~3 GB"),
]

# (config value, display label) shown in the push-to-talk key submenu.
# Only keys that do nothing when held alone: the daemon observes the key
# without swallowing it, so caps_lock would toggle capitals on every
# recording, and ctrl_l is a prefix key for shortcuts.
# menu and alt_gr are listed for compact laptops that have neither a right
# Ctrl nor a usable F-row. alt_gr stays a live modifier, so it only suits
# layouts where AltGr is not typed with (on DE, Ctrl+Alt reaches the same
# glyphs). Note alt_gr is KEY_RIGHTALT: on keyboards whose AltGr emits
# Ctrl+Alt as a pair there is no single key to hold, and the daemon cannot
# use it — pick another key there.
PTT_KEYS = [
    ("ctrl_r", "Right Ctrl"),
    ("f9", "F9"),
    ("f8", "F8"),
    ("menu", "Menu key"),
    ("alt_gr", "AltGr"),
]

# (config value, display label) in the Language submenu. "auto" maps to
# None in the daemon (whisper auto-detect); any other ISO 639-1 code can be
# set via WHISPER_LANG if the menu is not enough.
LANGS = [
    ("auto", "Auto-detect"),
    ("de", "Deutsch"),
    ("en", "English"),
    ("it", "Italiano"),
    ("es", "Español"),
    ("fr", "Français"),
    ("kn", "Kannada (ಕನ್ನಡ)"),
    ("zh", "中文 (Chinese)"),
]

# Imported lazily by load_tray_backend(): the update phase 2 (pip installs)
# must be able to run before these modules lock any of their files.
pystray = None
Image = None
ImageDraw = None


def load_tray_backend():
    """Import pystray/PIL; False when unusable (headless mode).

    Catches everything, not just ImportError: on a headless Linux box
    pystray's xorg backend raises Xlib DisplayNameError at import time —
    and headless fallback is exactly the case we must not crash in.
    """
    global pystray, Image, ImageDraw
    try:
        import pystray as _pystray
        from PIL import Image as _Image
        from PIL import ImageDraw as _ImageDraw
        pystray, Image, ImageDraw = _pystray, _Image, _ImageDraw
        return True
    except Exception:
        return False


# --------------------------------------------------------------------------
# pure helpers (unit-tested)

def version_tuple(version):
    """'v0.1.2' -> (0, 1, 2); unparsable -> (0, 0, 0)."""
    nums = [int(n) for n in re.findall(r"\d+", version)[:3]]
    return tuple((nums + [0, 0, 0])[:3])


def is_newer(latest, current):
    return version_tuple(latest) > version_tuple(current)


def read_state():
    """Daemon state dict; {'state': 'stopped'} when missing/corrupt."""
    try:
        with open(STATE_PATH, encoding="utf-8") as fh:
            data = json.load(fh)
        if not isinstance(data, dict) or not isinstance(data.get("state"), str):
            raise ValueError("unexpected state file shape")
        return data
    except (OSError, ValueError):
        return {"state": "stopped"}


def pid_alive(pid):
    """Cross-platform liveness check; uncertain answers count as alive."""
    if not isinstance(pid, int) or pid <= 0:
        return False
    try:
        if sys.platform == "win32":
            import ctypes
            handle = ctypes.windll.kernel32.OpenProcess(0x1000, False, pid)
            if not handle:
                return False
            ctypes.windll.kernel32.CloseHandle(handle)
            return True
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True  # exists but is not ours (EPERM number varies by OS)
    except OSError:
        return False


def state_from(data):
    """Effective daemon state from a state-file dict: dead pid means stopped."""
    if not pid_alive(data.get("pid")):
        return "stopped"
    return data.get("state", "stopped")


def overlay_wanted():
    """The recording overlay: on for Windows, opt-in on other X11-ish desktops
    (PTT_OVERLAY=1), off with PTT_OVERLAY=0. Linux desktops show their own mic
    indicator; on macOS tkinter and pystray would fight over the main thread,
    so it is never enabled there."""
    if sys.platform == "darwin":
        return False
    default = "1" if sys.platform == "win32" else "0"
    return os.environ.get("PTT_OVERLAY", default) != "0"


_CONFIG_LOCK = threading.Lock()


def write_config(**updates):
    """Merge settings into the tray's config file (atomic, keeps other keys)."""
    with _CONFIG_LOCK:
        os.makedirs(os.path.dirname(CONFIG_PATH), exist_ok=True)
        try:
            with open(CONFIG_PATH, encoding="utf-8") as fh:
                data = json.load(fh)
            if not isinstance(data, dict):
                data = {}
        except (OSError, ValueError):
            data = {}
        data.update(updates)
        tmp = CONFIG_PATH + ".tmp"
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(data, fh)
        os.replace(tmp, CONFIG_PATH)


def current_model():
    """The model the daemon reports, else the configured choice, else None.

    A state file whose pid is gone is stale — trust the configuration then,
    not the model some dead daemon used to run.
    """
    data = read_state()
    if pid_alive(data.get("pid")) and isinstance(data.get("model"), str) \
            and data["model"]:
        return data["model"]
    return dictate.configured_model()


def current_ptt_key():
    """The key the daemon listens on, else the configured choice, else ctrl_r."""
    data = read_state()
    if pid_alive(data.get("pid")) and isinstance(data.get("ptt_key"), str) \
            and data["ptt_key"]:
        return data["ptt_key"]
    return dictate.configured_ptt_key()


def current_lang():
    """The effective dictation language — the config file is authoritative
    the moment it is written (the daemon reads it per dictation), so unlike
    model/key there is no need to consult the state file. 'auto' for detect."""
    return dictate.configured_lang() or "auto"


def clear_env_var(name):
    """Unset a user-level env var so it cannot outrank the tray's config file.

    Windows: delete the setx value from the registry and broadcast the change;
    our own process env is cleaned too, so a daemon we restart inherits the
    cleared state. On Linux/macOS a service-level env var is a deliberate
    manual override in a unit/plist — leave it alone.
    """
    os.environ.pop(name, None)
    if sys.platform != "win32":
        return
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0,
                            winreg.KEY_SET_VALUE) as key:
            winreg.DeleteValue(key, name)
    except FileNotFoundError:
        return
    try:
        import ctypes
        # WM_SETTINGCHANGE broadcast: new processes see the change at once.
        ctypes.windll.user32.SendMessageTimeoutW(
            0xFFFF, 0x1A, 0, "Environment", 0x2, 1000, None)
    except Exception:
        pass  # the next logon refreshes it anyway


# --------------------------------------------------------------------------
# daemon supervision

class DaemonSupervisor:
    """Keep dictate.py running; restart with backoff, give up after 5 fast deaths."""

    def __init__(self, on_give_up=None):
        self.proc = None
        self.failures = 0
        self.on_give_up = on_give_up
        self._stopping = threading.Event()
        self._lock = threading.Lock()
        self._thread = None

    @property
    def alive(self):
        return self.proc is not None and self.proc.poll() is None

    def start(self):
        with self._lock:
            if self.alive:
                return
            self.proc = subprocess.Popen(
                [sys.executable, DAEMON], cwd=DIR,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop(self):
        """Stop daemon and supervision for good (quit / update handover)."""
        self._stopping.set()
        self._stop_child()

    def _stop_child(self):
        with self._lock:
            if self.alive:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
            self.proc = None

    def restart(self):
        """Restart the daemon and make sure supervision is actually running."""
        self._stop_child()
        self.failures = 0
        self.ensure_supervising()

    def ensure_supervising(self):
        """Start the supervise loop unless one is already running."""
        if self._thread is None or not self._thread.is_alive():
            self._stopping.clear()
            self._thread = threading.Thread(target=self.supervise, daemon=True)
            self._thread.start()

    def supervise(self):
        """Blocking loop: (re)start the daemon until told to stop."""
        while not self._stopping.is_set():
            started = time.time()
            self.start()
            while self.alive and not self._stopping.is_set():
                time.sleep(0.5)
            if self._stopping.is_set():
                break
            # A run longer than a minute proves the daemon is healthy.
            self.failures = 0 if time.time() - started > 60 else self.failures + 1
            if self.failures > 5:
                dictate.log("tray: daemon died 5 times in a row - giving up")
                if self.on_give_up:
                    self.on_give_up()
                break
            time.sleep(min(2 ** self.failures, 30))
        self._stop_child()


# --------------------------------------------------------------------------
# updates

def latest_release_tag():
    req = urllib.request.Request(RELEASES_LATEST_URL,
                                 headers={"User-Agent": f"hushkey/{VERSION}"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.load(resp)["tag_name"]


def refresh_code():
    """Phase 1 of an update: swap the source files, nothing else.

    No pip installs here — this process has pystray/PIL loaded and would
    lock their files on Windows. The installer runs in phase 2 instead.
    """
    with open(UPDATE_LOG, "ab") as out:
        out.write(f"\n--- update phase 1 {time.strftime('%Y-%m-%d %H:%M:%S')} ---\n".encode())
        if os.path.isdir(os.path.join(DIR, ".git")):
            subprocess.run(["git", "-C", DIR, "pull", "--ff-only"],
                           check=True, timeout=180,
                           env=dict(os.environ, GIT_TERMINAL_PROMPT="0"),
                           stdout=out, stderr=subprocess.STDOUT)
        else:  # one-liner install without .git: fetch the archive
            _fetch_archive_over_dir()


def _strip_top_folder(name):
    parts = name.split("/", 1)
    return parts[1] if len(parts) == 2 and parts[1] else None


def _fetch_archive_over_dir():
    """Download the master archive and unpack it over DIR (strip top folder)."""
    kind = "zip" if sys.platform == "win32" else "tar.gz"
    url = f"https://github.com/{REPO}/archive/refs/heads/master.{kind}"
    req = urllib.request.Request(url, headers={"User-Agent": f"hushkey/{VERSION}"})
    with urllib.request.urlopen(req, timeout=60) as resp:
        blob = resp.read()  # complete in memory before any file is touched
    import io
    if sys.platform == "win32":
        import zipfile
        with zipfile.ZipFile(io.BytesIO(blob)) as zf:
            for info in zf.infolist():
                rel = _strip_top_folder(info.filename)
                if not rel:
                    continue
                target = os.path.join(DIR, rel)
                if info.is_dir():
                    os.makedirs(target, exist_ok=True)
                else:
                    with zf.open(info) as src, open(target, "wb") as dst:
                        dst.write(src.read())
    else:
        import tarfile
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as tf:
            for member in tf.getmembers():
                rel = _strip_top_folder(member.name)
                if not rel:
                    continue
                target = os.path.join(DIR, rel)
                if member.isdir():
                    os.makedirs(target, exist_ok=True)
                elif member.isfile():
                    with tf.extractfile(member) as src, open(target, "wb") as dst:
                        dst.write(src.read())


def run_pending_update_if_any():
    """Phase 2 of an update: the installer, run by the NEW tray at startup.

    Runs before pystray/PIL are imported, so pip can replace their files.
    A tray the installer itself tries to start just waits on our instance
    lock and bows out.
    """
    if not os.path.exists(UPDATE_PENDING):
        return
    try:
        os.remove(UPDATE_PENDING)
    except FileNotFoundError:
        return  # the update helper consumed it first
    with open(UPDATE_LOG, "ab") as out:
        out.write(b"--- update phase 2 (installer) ---\n")
        if sys.platform == "win32":
            cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
                   "-File", os.path.join(DIR, "install.ps1")]
        else:
            cmd = ["bash", os.path.join(DIR, "install.sh")]
        try:
            subprocess.run(cmd, cwd=DIR, check=True, timeout=900,
                           stdout=out, stderr=subprocess.STDOUT)
        except (OSError, subprocess.SubprocessError) as exc:
            dictate.log(f"tray: update installer failed: {exc}")


def spawn_update_helper():
    """Spawn the detached update helper and hand it our pid.

    The helper waits for this process to exit, runs the installer and starts
    the new tray — the handover no longer depends on a process that is
    halfway through dying. Its stderr lands in update.log, so even an
    import-time crash stays diagnosable.
    """
    errlog = open(UPDATE_LOG, "ab")
    kwargs = {"cwd": DIR, "stdout": errlog, "stderr": subprocess.STDOUT,
              "close_fds": True}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.DETACHED_PROCESS
                                   | subprocess.CREATE_NEW_PROCESS_GROUP)
    else:
        kwargs["start_new_session"] = True
    subprocess.Popen([sys.executable, os.path.join(DIR, "update_helper.py"),
                      str(os.getpid())], **kwargs)


def acquire_lock(wait=0):
    """Single-instance file lock; returns the open handle or None.

    Waiting a few seconds lets a successor (spawned by the update helper, the
    service manager or the installer) take over from the tray that is about
    to exit — without ever running two trays side by side.
    """
    if sys.platform == "win32":
        import msvcrt
    else:
        import fcntl
    path = os.path.join(dictate.STATE_DIR, "tray.lock")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fh = open(path, "a+b")
    deadline = time.time() + wait
    while True:
        try:
            fh.seek(0)
            fh.write(b"\0")
            fh.flush()
            if sys.platform == "win32":
                fh.seek(0)
                msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fh
        except OSError:
            if time.time() >= deadline:
                fh.close()
                return None
            time.sleep(0.5)


# --------------------------------------------------------------------------
# the tray icon

BADGE_COLORS = {"recording": "#e53e3e", "transcribing": "#dd6b20",
                "update": "#2b6cb0"}  # blue: an update is waiting


def icon_key_for(state, update_pending):
    """Which image the tray shows: activity badges beat the update badge,
    a stopped daemon beats everything (it needs attention first)."""
    if state == "stopped":
        return "stopped"
    if state in BADGE_COLORS and state != "update":
        return state
    return "update" if update_pending else state


def _ui_lang():
    """Tray UI language: German on German systems, English everywhere else."""
    try:
        if sys.platform == "win32":
            import ctypes
            lang_id = ctypes.windll.kernel32.GetUserDefaultUILanguage()
            return "de" if (lang_id & 0x3FF) == 0x07 else "en"  # 0x07 = German
        lang = os.environ.get("LC_ALL") or os.environ.get("LANG") or ""
        return "de" if lang.lower().startswith("de") else "en"
    except Exception:
        return "en"


_STRINGS = {
    "en": {
        "idle": "ready",
        "recording": "recording …",
        "transcribing": "transcribing …",
        "starting": "starting …",
        "stopped": "daemon stopped",
        "title": "hushkey — {state}",
        "status": "hushkey {version} · {model} — {state}",
        "model_menu": "Model",
        "model_switching_title": "hushkey model",
        "model_switching": "switching to {model} — first use downloads {size}",
        "key_menu": "Push-to-talk key",
        "key_switching_title": "hushkey key",
        "key_switching": "push-to-talk key is now {key} — active after the daemon restart",
        "lang_menu": "Language",
        "lang_switching_title": "hushkey language",
        "lang_switching": "language is now {lang} — active on the next dictation",
        "update_item": "Install update: v{version}",
        "check_now": "Check for updates",
        "restart": "Restart daemon",
        "open_logs": "Open logs",
        "quit": "Quit",
        "update_available_title": "hushkey update available",
        "update_available": "v{version} — install via the tray menu",
        "update_installing_title": "hushkey update",
        "update_installing": "installing — the tray restarts itself",
        "update_failed_title": "hushkey update failed",
        "update_failed": "see {log}",
        "up_to_date_title": "hushkey",
        "up_to_date": "up to date (v{version})",
        "check_failed_title": "hushkey update check failed",
        "give_up_title": "hushkey daemon",
        "give_up": "the daemon keeps crashing — check the logs",
    },
    "de": {
        "idle": "bereit",
        "recording": "Aufnahme …",
        "transcribing": "transkribiert …",
        "starting": "startet …",
        "stopped": "Daemon gestoppt",
        "title": "hushkey — {state}",
        "status": "hushkey {version} · {model} — {state}",
        "model_menu": "Modell",
        "model_switching_title": "hushkey Modell",
        "model_switching": "wechsle zu {model} — beim ersten Mal werden {size} geladen",
        "key_menu": "Push-to-talk-Taste",
        "key_switching_title": "hushkey Taste",
        "key_switching": "Push-to-talk-Taste ist jetzt {key} — aktiv nach dem Daemon-Neustart",
        "lang_menu": "Sprache",
        "lang_switching_title": "hushkey Sprache",
        "lang_switching": "Sprache ist jetzt {lang} — ab dem nächsten Diktat aktiv",
        "update_item": "Update installieren: v{version}",
        "check_now": "Nach Updates suchen",
        "restart": "Daemon neu starten",
        "open_logs": "Logs öffnen",
        "quit": "Beenden",
        "update_available_title": "hushkey Update verfügbar",
        "update_available": "v{version} — Installation per Tray-Menü",
        "update_installing_title": "hushkey Update",
        "update_installing": "wird installiert — der Tray startet sich neu",
        "update_failed_title": "hushkey Update fehlgeschlagen",
        "update_failed": "siehe {log}",
        "up_to_date_title": "hushkey",
        "up_to_date": "aktuell (v{version})",
        "check_failed_title": "hushkey Update-Prüfung fehlgeschlagen",
        "give_up_title": "hushkey Daemon",
        "give_up": "der Daemon stürzt laufend ab — Logs prüfen",
    },
}
S = _STRINGS.get(_ui_lang(), _STRINGS["en"])


def load_images():
    base = Image.open(LOGO).convert("RGBA").resize((ICON_SIZE, ICON_SIZE))
    images = {"idle": base, "starting": base}
    for state, color in BADGE_COLORS.items():
        img = base.copy()
        draw = ImageDraw.Draw(img)
        draw.ellipse((36, 36, 62, 62), fill=color, outline="#2d3542", width=3)
        images[state] = img
    images["stopped"] = base.convert("L").convert("RGBA")  # greyed out
    return images


OVERLAY_COLORS = {"recording": "#e53e3e", "transcribing": "#dd6b20",
                  "starting": "#718096"}
OVERLAY_BG = "#2d3542"


def pill_for(state):
    """(color, text) for the overlay, or None when it should stay hidden."""
    if state in OVERLAY_COLORS:
        return OVERLAY_COLORS[state], S.get(state, state)
    return None


class RecordingOverlay:
    """Small always-on-top pill at the top of the screen while the daemon is
    recording or transcribing — the indicator an auto-hidden taskbar swallows.

    Fully self-contained: its own tkinter loop in its own thread, polling the
    state file directly. Nobody calls into it from outside, so there is no
    cross-thread tkinter usage to go wrong.
    """

    TITLE = "hushkey-overlay"

    def start(self):
        threading.Thread(target=self._run, daemon=True).start()

    def _run(self):
        try:
            import tkinter as tk
        except ImportError:
            # Only reached when the overlay was actually asked for, so staying
            # silent left the user with no window and no reason for it. tkinter
            # ships with Python on Windows but is a separate distro package on
            # Linux, and nothing in the project used to mention that.
            dictate.log("tray: overlay requested (PTT_OVERLAY) but tkinter is "
                        "missing — install python3-tk (Debian/Ubuntu), "
                        "python3-tkinter (Fedora) or tk (Arch)")
            return
        try:
            root = tk.Tk()
        except tk.TclError:
            return  # no display (headless) — same deal
        root.title(self.TITLE)
        root.overrideredirect(True)  # Windows: Tk itself gives these WS_EX_TOOLWINDOW
        root.attributes("-topmost", True)
        # no -alpha: layered windows are invisible to screen captures and can
        # glitch on some setups — a solid dark pill is the reliable choice
        frame = tk.Frame(root, bg=OVERLAY_BG)
        dot = tk.Label(frame, text="●", bg=OVERLAY_BG, font=("Segoe UI", 11))
        text = tk.Label(frame, bg=OVERLAY_BG, fg="#ffffff",
                        font=("Segoe UI", 10, "bold"))
        dot.pack(side="left", padx=(12, 6), pady=5)
        text.pack(side="left", padx=(0, 14), pady=5)
        frame.pack()
        root.withdraw()

        def tick():
            pill = pill_for(state_from(read_state()))
            if pill:
                color, label = pill
                dot.config(fg=color)
                text.config(text=label)
                root.update_idletasks()
                x = (root.winfo_screenwidth() - root.winfo_reqwidth()) // 2
                root.geometry(f"+{x}+6")
                root.deiconify()
            else:
                root.withdraw()
            root.after(200, tick)

        tick()
        root.mainloop()


class Tray:
    def __init__(self):
        self.images = load_images()
        self.state = "starting"
        self.pending_update = None
        self.stopping = threading.Event()
        self.daemon = DaemonSupervisor(on_give_up=self._on_give_up)
        self.icon = pystray.Icon("hushkey", self.images["starting"],
                                 "hushkey", self._menu())

    def _menu(self):
        # pystray.MenuItem: text/visible callables get the item (1 arg),
        # actions get (icon, item) — pystray._base.MenuItem adapts the rest.
        item = pystray.MenuItem
        return pystray.Menu(
            item(lambda _m: S["status"].format(
                     version=VERSION, model=current_model() or "…",
                     state=S.get(self.state, self.state)),
                 None, enabled=False),
            item(S["model_menu"], self._model_menu()),
            item(S["key_menu"], self._key_menu()),
            item(S["lang_menu"], self._lang_menu()),
            pystray.Menu.SEPARATOR,
            item(lambda _m: S["update_item"].format(version=self.pending_update),
                 self._on_update, visible=lambda _m: self.pending_update is not None),
            item(S["check_now"], self._on_check_now),
            item(S["restart"], lambda _i, _m: self.daemon.restart()),
            item(S["open_logs"], self._on_open_logs),
            pystray.Menu.SEPARATOR,
            item(S["quit"], self._on_quit),
        )

    def _model_menu(self):
        # the thunks resolve the module-level getter at call time, so tests
        # (and code) can monkeypatch tray.current_model / current_ptt_key
        return self._choice_menu([(name, f"{name} ({size})") for name, size in MODELS],
                                 lambda: current_model(), self._set_model)

    def _key_menu(self):
        return self._choice_menu(list(PTT_KEYS),
                                 lambda: current_ptt_key(), self._set_ptt_key)

    def _lang_menu(self):
        return self._choice_menu(list(LANGS),
                                 lambda: current_lang(), self._set_lang)

    def _choice_menu(self, entries, current_getter, setter):
        """Radio submenu from (value, display-text) pairs."""
        # pystray accepts at most 2-arg actions — bind the value via factory,
        # not via a default argument (that would count towards co_argcount).
        item = pystray.MenuItem

        def make_action(value):
            return lambda _i, _m: setter(value)

        def make_checked(value):
            return lambda _m: current_getter() == value

        return pystray.Menu(*[
            item(text, make_action(value), checked=make_checked(value),
                 radio=True)
            for value, text in entries
        ])

    # -- menu actions ------------------------------------------------------

    def _on_update(self, icon, _item):
        threading.Thread(target=self._update_worker, daemon=True).start()

    def _update_worker(self):
        self._notify(S["update_installing_title"], S["update_installing"])
        try:
            self.daemon.stop()
            refresh_code()
            open(UPDATE_PENDING, "w").close()  # tells the helper to run the installer
            # systemd kills our cgroup (and with it any helper we spawn), so
            # there the restarted service completes the update via the safety
            # net in main() instead.
            if not os.environ.get("INVOCATION_ID"):
                spawn_update_helper()  # while we are fully alive — see #12
        except (OSError, subprocess.SubprocessError) as exc:
            dictate.log(f"tray: update failed: {exc}")
            self._notify(S["update_failed_title"],
                         S["update_failed"].format(log=UPDATE_LOG))
            self.daemon.restart()  # restores supervision, not just the process
            return
        try:
            self.icon.stop()  # NIM_DELETE now, else a ghost icon lingers
        except Exception:
            pass
        time.sleep(0.3)
        # systemd (Restart=on-failure) and launchd (KeepAlive) bring up a
        # service-managed instance; the helper's spawn wins the lock elsewhere.
        os._exit(1 if sys.platform.startswith("linux") else 0)

    def _on_check_now(self, _icon, _item):
        threading.Thread(target=self.check_updates, kwargs={"manual": True},
                         daemon=True).start()

    def _set_model(self, name):
        threading.Thread(target=self._model_worker, args=(name,),
                         daemon=True).start()

    def _model_worker(self, name):
        if name == current_model():
            return  # re-clicking the active model must not restart the daemon
        clear_env_var("WHISPER_MODEL")  # an env var would mask the tray's choice
        write_config(model=name)
        size = dict(MODELS).get(name, "")
        self._notify(S["model_switching_title"],
                     S["model_switching"].format(model=name, size=size))
        self.daemon.restart()  # loads (and maybe downloads) the new model

    def _set_ptt_key(self, name):
        threading.Thread(target=self._key_worker, args=(name,),
                         daemon=True).start()

    def _key_worker(self, name):
        if name == current_ptt_key():
            return  # re-clicking the active key must not restart the daemon
        clear_env_var("PTT_KEY")
        write_config(ptt_key=name)
        label = dict(PTT_KEYS).get(name, name)
        self._notify(S["key_switching_title"],
                     S["key_switching"].format(key=label))
        self.daemon.restart()  # re-grabs the listener on the new key

    def _set_lang(self, name):
        threading.Thread(target=self._lang_worker, args=(name,),
                         daemon=True).start()

    def _lang_worker(self, name):
        """No restart needed — the daemon re-reads the config per dictation.
        Exception: an env var inside the daemon's process env masks the
        config, and only a restart clears that."""
        if name == current_lang():
            return
        had_env = "WHISPER_LANG" in os.environ
        clear_env_var("WHISPER_LANG")
        try:
            write_config(lang=name)
        except OSError as exc:
            self._notify(S["lang_switching_title"], str(exc)[:120])
            return
        label = dict(LANGS).get(name, name)
        self._notify(S["lang_switching_title"],
                     S["lang_switching"].format(lang=label))
        if had_env:
            self.daemon.restart()
        else:
            self.icon.update_menu()  # move the checkmark right away

    def _on_open_logs(self, _icon, _item):
        if sys.platform == "win32":
            os.startfile(dictate.STATE_DIR)
        elif sys.platform == "darwin":
            subprocess.Popen(["open", dictate.STATE_DIR])
        else:
            subprocess.Popen(["xdg-open", dictate.STATE_DIR],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def _on_quit(self, icon, _item):
        self.stopping.set()
        self.daemon.stop()
        icon.stop()

    def _on_give_up(self):
        self._notify(S["give_up_title"], S["give_up"])

    # -- background loops --------------------------------------------------

    def _notify(self, title, message):
        try:
            self.icon.notify(message, title)
        except Exception:  # no notification server (Linux), headless, …
            pass

    def poll_state(self):
        while not self.stopping.is_set():
            data = read_state()
            pid = data.get("pid")
            if os.path.exists(STATE_PATH) and pid_alive(pid):
                state = data.get("state", "idle")
            elif self.daemon.alive:
                # Child is up but its state is missing or stale: first boot or
                # a model download after a switch can take minutes.
                state = "starting"
            else:
                state = "stopped"
            if state != self.state:
                self.state = state
                self._apply_icon()
                self.icon.title = S["title"].format(state=S.get(state, state))
                self.icon.update_menu()  # the status line rebuilds only on this
            time.sleep(0.3)

    def _apply_icon(self):
        key = icon_key_for(self.state, self.pending_update is not None)
        self.icon.icon = self.images.get(key, self.images["idle"])

    def check_updates(self, manual=False):
        try:
            tag = latest_release_tag()
        except Exception as exc:
            dictate.log(f"tray: update check failed: {exc}")
            if manual:
                self._notify(S["check_failed_title"], str(exc)[:120])
            return
        if is_newer(tag, VERSION):
            self.pending_update = tag.lstrip("v")
            self._apply_icon()  # blue badge: an update is waiting
            self.icon.update_menu()
            self._notify(S["update_available_title"],
                         S["update_available"].format(version=self.pending_update))
        elif manual:
            self._notify(S["up_to_date_title"], S["up_to_date"].format(version=VERSION))

    def update_loop(self):
        if os.environ.get("PTT_UPDATE_CHECK", "1") == "0":
            return
        while not self.stopping.is_set():
            self.check_updates()
            self.stopping.wait(UPDATE_CHECK_INTERVAL)

    def run(self):
        self.daemon.ensure_supervising()
        threading.Thread(target=self.poll_state, daemon=True).start()
        threading.Thread(target=self.update_loop, daemon=True).start()
        try:
            self.icon.run()  # blocks until Quit; must be the main thread (macOS)
        except Exception as exc:
            # No usable notification area: keep supervising without an icon.
            # The supervisor is already running — a second one would spawn a
            # duplicate daemon, and two daemons type every dictation twice.
            print(f"tray icon unavailable ({exc}) - supervising headless",
                  file=sys.stderr)
            while not self.stopping.is_set():
                self.stopping.wait(3600)


def supervise_headless():
    """No tray possible at all: just keep the daemon alive."""
    supervisor = DaemonSupervisor()
    try:
        supervisor.supervise()
    except KeyboardInterrupt:
        supervisor.stop()
    return 0


def main():
    lock = acquire_lock(wait=15)
    if lock is None:
        print("another hushkey tray is already running", file=sys.stderr)
        return 1
    run_pending_update_if_any()  # before any GUI import locks files (Windows)
    if overlay_wanted():
        RecordingOverlay().start()
    if not load_tray_backend():
        print("pystray/pillow missing - supervising daemon without tray icon",
              file=sys.stderr)
        return supervise_headless()
    try:
        tray = Tray()
    except Exception as exc:
        # Icon construction failed (no display, no notification area):
        # no daemon is running yet at this point, so a headless fallback
        # cannot duplicate anything.
        print(f"tray icon unavailable ({exc}) - supervising headless",
              file=sys.stderr)
        return supervise_headless()
    tray.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
