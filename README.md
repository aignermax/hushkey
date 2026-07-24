# whisper-ptt

Local, offline push-to-talk dictation for Linux (X11) — hold a key, speak,
release, and the transcript is typed into whatever window has focus. Terminal,
editor, browser, chat: any app. Powered by
[faster-whisper](https://github.com/SYSTRAN/faster-whisper), CUDA-accelerated
when an NVIDIA GPU is present, CPU otherwise.

Nothing leaves your machine after the one-time model download.

## What you get

- **`dictate.py`** — the push-to-talk daemon (autostarts via systemd user service)
- **`transcribe.py`** — batch-transcribe a folder of audio files into Markdown
  notes (voice memos → text), with skip-unchanged state tracking
- **`install.sh` / `uninstall.sh`** — one-command setup/removal

## Requirements

- Linux with an **X11 session** (`echo $XDG_SESSION_TYPE` → `x11`).
  Wayland is *not* supported (global key grab + synthetic typing need X11).
- PipeWire with `pw-record` (standard on Ubuntu ≥ 22.10 / Fedora)
- Python ≥ 3.10 with `python3-venv`
- A microphone
- Optional: NVIDIA GPU (faster; ~1.5 GB CUDA pip packages are installed
  automatically when `nvidia-smi` is found)

## Install

```bash
git clone git@github.com:aignermax/whisper-ptt.git
cd whisper-ptt
./install.sh
```

That's it: the daemon is running and starts on every login.
The first dictation downloads the whisper model (~0.5–1.5 GB) into
`~/.cache/huggingface`; after that everything is offline.

## Use

| Action | How |
|---|---|
| Dictate into any app | Focus the input field, **hold Right Ctrl**, speak, release → text is typed |
| Batch-transcribe voice memos | `.venv/bin/python transcribe.py ~/path/to/audio --out ~/notes` |

Dictated text is only *typed* — nothing is submitted; review and hit Enter.
A trailing space is appended so consecutive dictations don't stick together.

## Configuration

Environment variables (put `Environment=...` lines into
`~/.config/systemd/user/whisper-ptt.service`, then
`systemctl --user daemon-reload && systemctl --user restart whisper-ptt`):

| Variable | Default | Meaning |
|---|---|---|
| `PTT_KEY` | `ctrl_r` | Push-to-talk key (`f9`, `caps_lock`, … any `pynput.keyboard.Key`) |
| `WHISPER_MODEL` | `medium` (GPU) / `small` (CPU) | Whisper model size |
| `WHISPER_LANG` | `de` | Language code; empty string = auto-detect |

## Manage

```bash
systemctl --user status whisper-ptt.service      # state
journalctl --user -u whisper-ptt.service -f      # live logs
systemctl --user stop whisper-ptt.service        # pause
./uninstall.sh                                   # remove service
./uninstall.sh --purge                           # also remove the venv
```

Operational log (metadata only — durations and character counts, never
dictated text): `~/.local/state/whisper-ptt/dictate.log`.

## Limitations

- X11 only. On Wayland this needs a different backend (ydotool + evdev or a
  desktop portal).
- The PTT key is grabbed globally while the daemon runs — pick one you don't
  otherwise use.
- Transcription quality of names/jargon can be improved by switching to a
  larger model (`WHISPER_MODEL=large-v3`, needs ~6 GB VRAM) instead of `medium`.
- First run performs an unauthenticated Hugging Face model download (rate
  limits apply); after that it's fully offline.

## License

MIT (personal project; do what you want).
