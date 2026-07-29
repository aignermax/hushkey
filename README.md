# whisper-ptt

Local, offline push-to-talk dictation for Linux — hold a key, speak, release,
and the transcript lands in whatever window has focus. Terminal, editor,
browser, chat: any app. Works on **X11 and Wayland** (including GNOME on
Ubuntu). Powered by
[faster-whisper](https://github.com/SYSTRAN/faster-whisper), CUDA-accelerated
when an NVIDIA GPU is present, CPU otherwise.

Nothing leaves your machine after the one-time model download.

## What you get

- **`dictate.py`** — the push-to-talk daemon (autostarts via systemd user service)
- **`transcribe.py`** — batch-transcribe a folder of audio files into Markdown
  notes (voice memos → text), with skip-unchanged state tracking
- **`install.sh` / `uninstall.sh`** — one-command setup/removal

## Requirements

- Linux with X11 **or** Wayland (`echo $XDG_SESSION_TYPE`). The installer picks
  the matching backend; Wayland needs a little extra setup, see below.
- PipeWire with `pw-record` (standard on Ubuntu ≥ 22.10 / Fedora)
- Python ≥ 3.10 with `python3-venv`
- A microphone
- Optional: NVIDIA GPU (faster; ~1.5 GB CUDA pip packages are installed
  automatically when `nvidia-smi` is found)

**On Wayland additionally** — the installer handles all of this and will ask
for your password once:

- `ydotool` and `wl-clipboard` packages
- a udev rule giving the `input` group access to `/dev/uinput`
- your user added to the `input` group (**requires one logout**)

## Install

```bash
git clone https://github.com/aignermax/whisper-ptt.git
cd whisper-ptt
./install.sh
```

On X11 that's it: the daemon is running and starts on every login. On Wayland,
if the installer had to add you to the `input` group, it says so and you need
to log out once — the service is already enabled and comes up by itself
afterwards.

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
| `PTT_KEY` | `ctrl_r` | Push-to-talk key (`f9`, `caps_lock`, … or a raw evdev name like `KEY_RIGHTCTRL`) |
| `WHISPER_MODEL` | `medium` (GPU) / `small` (CPU) | Whisper model size |
| `WHISPER_LANG` | `de` | Language code; empty string = auto-detect |
| `PTT_BACKEND` | from `XDG_SESSION_TYPE` | Force `x11` or `wayland` |
| `PTT_PASTE_KEY` | `ctrl+v` | Wayland only: the paste chord. **Terminals need `ctrl+shift+v`** |
| `PTT_KEEP_CLIPBOARD` | unset | Wayland only: `1` leaves the transcript in the clipboard instead of restoring the previous contents |

### The Wayland paste chord

On Wayland the text arrives via the clipboard plus one synthetic paste
keystroke, and `Ctrl+V` is not universal: GNOME Terminal, Ptyxis, Konsole and
friends paste with `Ctrl+Shift+V`. There is no reliable way to ask a Wayland
compositor which application has focus, so the daemon cannot switch chords by
itself. Pick the one matching where you dictate most, and override it when you
need the other:

```
Environment=PTT_PASTE_KEY=ctrl+shift+v
```

If you dictate into both terminals and GUI apps, `PTT_KEEP_CLIPBOARD=1` plus a
manual paste is the escape hatch — the transcript is simply waiting in the
clipboard.

## Manage

```bash
systemctl --user status whisper-ptt.service      # state
journalctl --user -u whisper-ptt.service -f      # live logs
systemctl --user stop whisper-ptt.service        # pause
systemctl --user status ydotoold.service         # Wayland: the typing helper
./uninstall.sh                                   # remove services
./uninstall.sh --purge                           # also remove the venv
./uninstall.sh --purge-system                    # also remove udev rule + 'input' group
```

Also useful when something is off: run the daemon in the foreground and it
prints exactly which backend it picked, or refuses with the reason why:

```bash
.venv/bin/python dictate.py
```

Operational log (metadata only — durations and character counts, never
dictated text): `~/.local/state/whisper-ptt/dictate.log`.

## How the Wayland backend works

Wayland deliberately denies clients both halves of what a dictation tool needs,
so each is solved below the compositor:

**Reading the key.** There is no global key grab for clients, so the daemon
reads the key events straight from `/dev/input/event*` via `evdev`. That needs
membership in the `input` group.

**Inserting the text.** Clients cannot synthesize input either. `ydotool`
creates a virtual keyboard through `/dev/uinput`, which is a kernel device — the
compositor sees an ordinary keyboard and accepts the event.

**Why the clipboard, and not just typing it out.** `ydotool type` maps
characters to Linux keycodes assuming a US layout. On a German (`de`) keymap
that turns `z` into `y`, and `ä ö ü ß` are not in its table at all — precisely
the characters German dictation produces. So the transcript travels as UTF-8
through the clipboard, where no keycode mapping happens, and only the
layout-stable paste chord is synthesized.

> **Security note.** Both steps are real privilege grants. Membership in the
> `input` group lets any process running as you read *every* keystroke on the
> machine, and write access to `/dev/uinput` lets it inject any input. That is
> inherent to doing this on Wayland at all, not specific to this tool — but only
> set it up on a machine you trust. `./uninstall.sh --purge-system` reverts both.

The Debian/Ubuntu `ydotool` package ships its own `ydotool.service` user unit,
which starts `ydotoold` on the *default* socket path. We install a separate
`ydotoold.service` that pins the socket to `$XDG_RUNTIME_DIR/.ydotool_socket`,
so both sides agree on one path. Leave the packaged unit disabled — enabling it
too just runs a second daemon.

## Limitations

- **Wayland: the paste chord is fixed per configuration** — see "The Wayland
  paste chord" above. Terminals and GUI apps disagree, and the compositor won't
  tell us which one has focus.
- **Wayland: the clipboard is borrowed.** The previous contents are restored
  after ~0.4 s, which is best effort — a slow application may still fetch the
  restored value instead of the transcript. Only *text* clipboards are restored:
  if you had an image copied, the transcript stays and the image is not put
  back, because rewriting arbitrary bytes as `text/plain` would corrupt it.
  `PTT_KEEP_CLIPBOARD=1` disables the restore entirely.
- The PTT key is grabbed globally while the daemon runs — pick one you don't
  otherwise use.
- Transcription quality of names/jargon can be improved by switching to a
  larger model (`WHISPER_MODEL=large-v3`, needs ~6 GB VRAM) instead of `medium`.
- First run performs an unauthenticated Hugging Face model download (rate
  limits apply); after that it's fully offline.

## License

MIT (personal project; do what you want).
