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
(disable with PTT_UPDATE_CHECK=0). Updates are never installed silently —
one menu click runs the installer and hands over to the new code.
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
WEB_INSTALL = {
    "win32": ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
              f"irm https://raw.githubusercontent.com/{REPO}/master/install.ps1 | iex"],
    "posix": ["bash", "-c",
              f"curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | bash"],
}
UPDATE_CHECK_INTERVAL = 24 * 60 * 60
ICON_SIZE = 64

sys.path.insert(0, DIR)
import dictate  # noqa: E402  local module: single source for VERSION/STATE_DIR

VERSION = dictate.VERSION
STATE_PATH = dictate.STATE_PATH
UPDATE_LOG = os.path.join(dictate.STATE_DIR, "update.log")

try:
    import pystray
    from PIL import Image, ImageDraw
    HAVE_TRAY = True
except ImportError:
    HAVE_TRAY = False


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
    except OSError as exc:
        # EPERM means the process exists but is not ours — still alive.
        return getattr(exc, "errno", None) == 13


# --------------------------------------------------------------------------
# daemon supervision

class DaemonSupervisor:
    """Keep dictate.py running; restart with backoff, give up after 5 fast deaths."""

    def __init__(self):
        self.proc = None
        self.failures = 0
        self._stopping = threading.Event()
        self._lock = threading.Lock()

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
        self._stop_child()
        self.failures = 0
        self.start()

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
                dictate.log("tray: daemon died 5 times in a row — giving up")
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


def refresh_and_install():
    """Fetch the newest code and re-run the installer (idempotent).

    Git checkouts pull; one-liner installs (no .git) re-run the same web
    bootstrap the user installed with, which refreshes the standard install
    directory and re-runs the installer from there.
    """
    with open(UPDATE_LOG, "ab") as out:
        out.write(f"\n--- update {time.strftime('%Y-%m-%d %H:%M:%S')} ---\n".encode())
        if os.path.isdir(os.path.join(DIR, ".git")):
            subprocess.run(["git", "-C", DIR, "pull", "--ff-only"],
                           check=True, timeout=180,
                           stdout=out, stderr=subprocess.STDOUT)
            if sys.platform == "win32":
                cmd = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass",
                       "-File", os.path.join(DIR, "install.ps1")]
            else:
                cmd = ["bash", os.path.join(DIR, "install.sh")]
        else:
            cmd = WEB_INSTALL["win32" if sys.platform == "win32" else "posix"]
        subprocess.run(cmd, cwd=DIR, check=True, timeout=900,
                       stdout=out, stderr=subprocess.STDOUT)


def restart_self():
    """Hand over to the freshly installed code.

    The new instance waits for our single-instance lock, so the overlap is
    safe. Exit code 1 on Linux makes systemd's Restart=on-failure bring up a
    clean service-managed instance; the spawned one then loses the lock and
    bows out. macOS launchd (KeepAlive) and Windows both tolerate exit 0 —
    the spawned instance becomes the new tray.
    """
    kwargs = {"cwd": DIR, "stdout": subprocess.DEVNULL,
              "stderr": subprocess.DEVNULL, "close_fds": True}
    if sys.platform == "win32":
        kwargs["creationflags"] = (subprocess.DETACHED_PROCESS
                                   | subprocess.CREATE_NEW_PROCESS_GROUP)
    subprocess.Popen([sys.executable, os.path.abspath(__file__)], **kwargs)
    os._exit(1 if sys.platform.startswith("linux") else 0)


def acquire_lock(wait=0):
    """Single-instance file lock; returns the open handle or None.

    Waiting a few seconds lets an updated instance take over from the one
    that is about to exit (see restart_self) without ever running two trays.
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

BADGE_COLORS = {"recording": "#e53e3e", "transcribing": "#dd6b20"}


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
        "stopped": "daemon stopped",
        "title": "hushkey — {state}",
        "status": "hushkey {version} — {state}",
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
    },
    "de": {
        "idle": "bereit",
        "recording": "Aufnahme …",
        "transcribing": "transkribiert …",
        "stopped": "Daemon gestoppt",
        "title": "hushkey — {state}",
        "status": "hushkey {version} — {state}",
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


class Tray:
    def __init__(self):
        self.images = load_images()
        self.state = "starting"
        self.pending_update = None
        self.stopping = threading.Event()
        self.daemon = DaemonSupervisor()
        self.icon = pystray.Icon("hushkey", self.images["starting"],
                                 "hushkey", self._menu())

    def _menu(self):
        # pystray.MenuItem: text/visible callables get the item (1 arg),
        # actions get (icon, item) — pystray._base.MenuItem adapts the rest.
        item = pystray.MenuItem
        return pystray.Menu(
            item(lambda _m: S["status"].format(
                     version=VERSION, state=S.get(self.state, self.state)),
                 None, enabled=False),
            pystray.Menu.SEPARATOR,
            item(lambda _m: S["update_item"].format(version=self.pending_update),
                 self._on_update, visible=lambda _m: self.pending_update is not None),
            item(S["check_now"], self._on_check_now),
            item(S["restart"], lambda _i, _m: self.daemon.restart()),
            item(S["open_logs"], self._on_open_logs),
            pystray.Menu.SEPARATOR,
            item(S["quit"], self._on_quit),
        )

    # -- menu actions ------------------------------------------------------

    def _on_update(self, icon, _item):
        threading.Thread(target=self._update_worker, daemon=True).start()

    def _update_worker(self):
        self._notify(S["update_installing_title"], S["update_installing"])
        try:
            self.daemon.stop()
            refresh_and_install()
        except (OSError, subprocess.SubprocessError) as exc:
            dictate.log(f"tray: update failed: {exc}")
            self._notify(S["update_failed_title"], S["update_failed"].format(log=UPDATE_LOG))
            self.daemon.restart()
            return
        restart_self()

    def _on_check_now(self, _icon, _item):
        threading.Thread(target=self.check_updates, kwargs={"manual": True},
                         daemon=True).start()

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

    # -- background loops --------------------------------------------------

    def _notify(self, title, message):
        try:
            self.icon.notify(message, title)
        except Exception:  # no notification server (Linux), headless, …
            pass

    def poll_state(self):
        while not self.stopping.is_set():
            data = read_state()
            state = data.get("state", "stopped")
            if state != "stopped" and not pid_alive(data.get("pid")):
                state = "stopped"
            if state != self.state:
                self.state = state
                self.icon.icon = self.images.get(state, self.images["idle"])
                self.icon.title = S["title"].format(state=S.get(state, state))
            time.sleep(0.3)

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
        self.daemon.start()
        threading.Thread(target=self.daemon.supervise, daemon=True).start()
        threading.Thread(target=self.poll_state, daemon=True).start()
        threading.Thread(target=self.update_loop, daemon=True).start()
        self.icon.run()  # blocks until Quit; must be the main thread (macOS)


def main():
    lock = acquire_lock(wait=15)
    if lock is None:
        print("another hushkey tray is already running", file=sys.stderr)
        return 1
    if not HAVE_TRAY:
        print("pystray/pillow missing — supervising daemon without tray icon",
              file=sys.stderr)
        supervisor = DaemonSupervisor()
        try:
            supervisor.supervise()
        except KeyboardInterrupt:
            supervisor.stop()
        return 0
    try:
        Tray().run()
    except Exception as exc:
        # No usable notification area (bare GNOME/Wayland, headless session):
        # dictation must survive even when the icon cannot be shown.
        print(f"tray icon unavailable ({exc}) — supervising headless",
              file=sys.stderr)
        supervisor = DaemonSupervisor()
        try:
            supervisor.supervise()
        except KeyboardInterrupt:
            supervisor.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
