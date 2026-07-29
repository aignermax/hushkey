# whisper-ptt

Local, offline push-to-talk dictation for **Linux (X11), Windows and macOS** —
hold a key, speak, release, and the transcript is typed into whatever window
has focus. Terminal, editor, browser, chat: any app. Powered by
[faster-whisper](https://github.com/SYSTRAN/faster-whisper), CUDA-accelerated
when an NVIDIA GPU is present (Linux/Windows), CPU otherwise.

Nothing leaves your machine after the one-time model download.

## What you get

- **`dictate.py`** — the push-to-talk daemon (autostarts via systemd on Linux,
  Task Scheduler on Windows, launchd on macOS)
- **`transcribe.py`** — batch-transcribe a folder of audio files into Markdown
  notes (voice memos → text), with skip-unchanged state tracking
- **`recorder.py`** — recording backends: `pw-record` (PipeWire) on Linux,
  `sounddevice`/PortAudio on Windows/macOS
- **`install.sh` / `uninstall.sh`** — one-command setup/removal (Linux, macOS)
- **`install.ps1` / `uninstall.ps1`** — one-command setup/removal (Windows)

## Requirements

| | Linux | Windows | macOS |
|---|---|---|---|
| OS | X11 session (`echo $XDG_SESSION_TYPE` → `x11`); Wayland unsupported | Windows 10+ | recent macOS |
| Audio | PipeWire `pw-record` (standard on Ubuntu ≥ 22.10/Fedora); fallback: `libportaudio2` | any microphone | any microphone |
| Python | ≥ 3.10 with `python3-venv` | ≥ 3.10 ([python.org](https://www.python.org/downloads/), "Add to PATH") | ≥ 3.10 |
| Optional | NVIDIA GPU for CUDA | NVIDIA GPU for CUDA | — |

On macOS you must grant the terminal **Microphone**, **Accessibility** and
**Input Monitoring** permissions when prompted (System Settings → Privacy &
Security) — the global hotkey and synthetic typing depend on them.

## Install

**Linux / macOS:**

```bash
git clone git@github.com:aignermax/whisper-ptt.git
cd whisper-ptt
./install.sh
```

**Windows (PowerShell):**

```powershell
git clone git@github.com:aignermax/whisper-ptt.git
cd whisper-ptt
powershell -ExecutionPolicy Bypass -File install.ps1
```

(`install.ps1 -NoAutostart` skips the autostart entry if you prefer to run
`.venv\Scripts\python.exe dictate.py` manually.)

That's it: the daemon is running and starts on every login.
The first dictation downloads the whisper model (~0.5–1.5 GB) into
`~/.cache/huggingface`; after that everything is offline.

## Use

| Action | How |
|---|---|
| Dictate into any app | Focus the input field, **hold Right Ctrl**, speak, release → text is typed |
| Batch-transcribe voice memos | `.venv/bin/python transcribe.py ~/path/to/audio --out ~/notes` (Windows: `.venv\Scripts\python.exe transcribe.py ...`) |

Dictated text is only *typed* — nothing is submitted; review and hit Enter.
A trailing space is appended so consecutive dictations don't stick together.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PTT_KEY` | `ctrl_r` | Push-to-talk key (`f9`, `caps_lock`, … any `pynput.keyboard.Key`) |
| `WHISPER_MODEL` | `medium` (GPU) / `small` (CPU) | Whisper model size |
| `WHISPER_LANG` | `de` | Language code; empty string = auto-detect |

How to set them:

- **Linux**: add `Environment=...` lines to
  `~/.config/systemd/user/whisper-ptt.service`, then
  `systemctl --user daemon-reload && systemctl --user restart whisper-ptt`.
- **Windows**: `setx PTT_KEY f9` (etc.) in a terminal; the autostart entry
  picks it up at the next logon, or immediately after restarting the daemon
  (`schtasks /end /tn whisper-ptt` + `schtasks /run /tn whisper-ptt`, or run
  `uninstall.ps1` + `install.ps1` again).
- **macOS**: edit `~/Library/LaunchAgents/com.whisper-ptt.plist`
  (`EnvironmentVariables` section is prepared), then
  `launchctl kickstart -k gui/$(id -u)/com.whisper-ptt`.

## Manage

```bash
# Linux
systemctl --user status whisper-ptt.service      # state
journalctl --user -u whisper-ptt.service -f      # live logs
systemctl --user stop whisper-ptt.service        # pause

# Windows (PowerShell)
schtasks /query /tn whisper-ptt                  # state (if Task Scheduler was used)
schtasks /end /tn whisper-ptt                    # stop
schtasks /run /tn whisper-ptt                    # start
# If task creation is denied (restricted account), install.ps1 falls back to a
# shortcut in the Startup folder instead — same autostart, no rights needed.

# macOS
launchctl kickstart -k gui/$(id -u)/com.whisper-ptt   # restart
launchctl bootout gui/$(id -u)/com.whisper-ptt        # stop

# any OS
./uninstall.sh            # or: powershell -File uninstall.ps1  (Windows)
./uninstall.sh --purge    # also remove the venv  (Windows: -Purge)
```

Operational log (metadata only — durations and character counts, never
dictated text):

- Linux: `~/.local/state/whisper-ptt/dictate.log`
- Windows: `%LOCALAPPDATA%\whisper-ptt\dictate.log`
- macOS: `~/Library/Logs/whisper-ptt/dictate.log` (+ `launchd.log`)

## Development & tests

```bash
pip install -r requirements.txt -r requirements-dev.txt
pytest -v
```

The suite runs without a microphone or display: platform logic is unit-tested
with fakes, and one end-to-end test transcribes a synthetic WAV with the real
`tiny` model (downloads ~75 MB once). CI (`.github/workflows/test.yml`) runs
it on **Ubuntu, Windows and macOS** — so macOS compatibility is verified on
every push without owning a Mac.

## Limitations

- Linux: X11 only. On Wayland this needs a different backend (ydotool + evdev
  or a desktop portal).
- Windows: synthetic typing cannot reach apps running **elevated**
  (as Administrator) unless the daemon itself runs elevated.
- macOS: no CUDA — CPU transcription only; the daemon needs the privacy
  permissions listed above.
- The PTT key is grabbed globally while the daemon runs — pick one you don't
  otherwise use. (On macOS, `ctrl_r` may not exist on laptop keyboards; try
  `f9` or `cmd_r`.)
- Transcription quality of names/jargon can be improved by switching to a
  larger model (`WHISPER_MODEL=large-v3`, needs ~6 GB VRAM) instead of `medium`.
- First run performs an unauthenticated Hugging Face model download (rate
  limits apply); after that it's fully offline.

## License

MIT (personal project; do what you want).
