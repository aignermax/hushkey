"""Repro of the tray's restart_self(): a pythonw process spawns its
replacement detached and exits immediately — does the child survive
and does it get the lock?"""
import os
import subprocess
import sys
import time

LOCK = os.path.join(os.environ["LOCALAPPDATA"], "whisper-ptt", "repro.lock")
RESULT = os.path.join(os.environ["LOCALAPPDATA"], "whisper-ptt", "repro.txt")

mode = sys.argv[1] if len(sys.argv) > 1 else "parent"

if mode == "parent":
    import msvcrt
    fh = open(LOCK, "a+b")
    fh.seek(0); fh.write(b"\0"); fh.flush()
    fh.seek(0)
    msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
    kwargs = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL,
              "close_fds": True,
              "creationflags": subprocess.DETACHED_PROCESS
              | subprocess.CREATE_NEW_PROCESS_GROUP}
    p = subprocess.Popen([sys.executable, os.path.abspath(__file__), "child"],
                         **kwargs)
    with open(RESULT, "w") as out:
        out.write(f"parent spawned child pid={p.pid}\n")
    os._exit(0)  # exactly like restart_self()
else:
    import msvcrt
    fh = open(LOCK, "a+b")
    ok = False
    deadline = time.time() + 15
    while time.time() < deadline:
        try:
            fh.seek(0); fh.write(b"\0"); fh.flush(); fh.seek(0)
            msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
            ok = True
            break
        except OSError:
            time.sleep(0.5)
    with open(RESULT, "a") as out:
        out.write(f"child alive, lock acquired: {ok}\n")
    time.sleep(20)  # stay observable
