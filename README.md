<p align="center">
  <img src="assets/logo.png" width="200" alt="hushkey logo — a winking husky holding a key">
</p>
<h1 align="center">hushkey</h1>
<p align="center"><em>formerly whisper-ptt</em></p>

Local, offline push-to-talk dictation for **Linux (X11 and Wayland), Windows and
macOS** — hold <kbd>Right Ctrl</kbd>, speak, release, and the transcript lands
in whatever window has focus. Terminal, editor, browser, chat: any app. Powered by
[faster-whisper](https://github.com/SYSTRAN/faster-whisper), CUDA-accelerated
when an NVIDIA GPU is present (Linux/Windows), CPU otherwise.

Nothing leaves your machine after the one-time model download.

## Install

Grab the installer from the
**[latest release](https://github.com/aignermax/hushkey/releases/latest)**:

- **Windows:** `hushkey-setup-….exe` — per-user (no admin), installs Python 3.12
  if missing, sets up autostart. Uninstall via Windows' app list (leaves the
  model cache in `~/.cache/huggingface` and the logs in
  `%LOCALAPPDATA%\whisper-ptt` in place).
- **Linux (Debian/Ubuntu):** `hushkey_…_all.deb` — `sudo apt install ./hushkey_…_all.deb`
  resolves the dependencies and sets up everything (venv, systemd user
  service); `apt purge hushkey` removes it again, including the udev rule.

Prefer the terminal — or are on macOS? The one-liners do the same job, and
re-running them updates in place:

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/aignermax/hushkey/master/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/aignermax/hushkey/master/install.ps1 | iex
```

Both fetch the sources (into `~/.local/share/whisper-ptt` on Linux/macOS,
`%LOCALAPPDATA%\Programs\whisper-ptt` on Windows), install all dependencies and
set up autostart. From your own checkout, `./install.sh` /
`powershell -ExecutionPolicy Bypass -File install.ps1` work the same way
(`install.ps1 -NoAutostart` skips the autostart entry if you prefer to run
`.venv\Scripts\python.exe dictate.py` manually).

That's it: the daemon is running and starts on every login. The one exception is
Linux/Wayland — if the installer had to add you to the `input` group, it says so
and you need to log out once; the service is already enabled and comes up by
itself afterwards.

The first dictation downloads the whisper model (~0.5–1.5 GB) into
`~/.cache/huggingface`; after that everything is offline.

## Why hushkey?

**Speaking is ~3× faster than typing** — and hushkey works in virtually every
app: it types into the focused window. No plugin, no integration, no account.

- **Prompt AI at the speed of thought.** Long prompts are the bottleneck when
  working with AI — and many AI tools have no voice input at all. Hold
  <kbd>Right Ctrl</kbd>, think out loud, release: IDE chat, web UI, terminal —
  the words land where your cursor is.
- **Every app becomes a dictation app.** Email, docs, chat, ticket comments,
  commit messages, search fields — if it takes text, hushkey types into it.
  On Windows and X11, terminals even get full-speed typing automatically.
- **Speaks your language.** German, English, Italian, Spanish, French,
  Kannada, Chinese — plus dozens more via `WHISPER_LANG`, with auto-detect
  for mixed-language teams.
- **Private by design.** Everything runs offline on your machine: no cloud,
  no account, no telemetry — nothing you say ever leaves the box.
- **No subscription.** MIT-licensed and free. Comparable cloud tools charge
  $8–15/month.
- **You see it working.** A tray icon — and on Windows a small on-screen
  pill — shows when the mic is live and when your text is being written.
  Switch between five model sizes (fast ↔ accurate) right from the tray.
- **Voice memos too.** `transcribe.py` turns a whole folder of recordings into
  Markdown notes in one go.

Built for heavy prompters, fast thinkers, slow typists and sore wrists — for
anyone who would rather *say* it than type it.

## Requirements

| | Linux | Windows | macOS |
|---|---|---|---|
| OS | X11 **or** Wayland (`echo $XDG_SESSION_TYPE`); the installer picks the matching backend | Windows 10+ | recent macOS |
| Audio | PipeWire `pw-record` (standard on Ubuntu ≥ 22.10/Fedora); fallback: `libportaudio2` | any microphone | any microphone |
| Python | ≥ 3.10 with `python3-venv` | ≥ 3.10 ([python.org](https://www.python.org/downloads/), "Add to PATH") | ≥ 3.10 |
| Optional | NVIDIA GPU for CUDA | NVIDIA GPU for CUDA | — |

On macOS you must grant the terminal **Microphone**, **Accessibility** and
**Input Monitoring** permissions when prompted (System Settings → Privacy &
Security) — the global hotkey and synthetic typing depend on them.

**On Linux/Wayland additionally** — the installer handles all of this and will
ask for your password once:

- `ydotool` and `wl-clipboard` packages (ydotool 0.1.x and 1.x both work —
  the daemon detects which key syntax and socket path the installed one uses)
- a udev rule giving the `input` group access to `/dev/uinput`
- your user added to the `input` group (**requires one logout**)

## Use

| Action | How |
|---|---|
| Dictate into any app | Focus the input field, **hold <kbd>Right Ctrl</kbd>**, speak, release → text is inserted |
| Batch-transcribe voice memos | `.venv/bin/python transcribe.py ~/path/to/audio --out ~/notes` (Windows: `.venv\Scripts\python.exe transcribe.py ...`) |

Dictated text is only *inserted* — nothing is submitted; review and hit Enter.
A trailing space is appended so consecutive dictations don't stick together.

**No <kbd>Right Ctrl</kbd> on your laptop?** Pick <kbd>F9</kbd> or
<kbd>F8</kbd> instead — right in the tray menu under *Push-to-talk
key* (no config file or env var needed).

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `PTT_KEY` | `ctrl_r` | Push-to-talk key (`f9`, `caps_lock`, … or a raw evdev name like `KEY_RIGHTCTRL`); easiest via the tray menu (**Push-to-talk key**) — picking a key there replaces a user-level `PTT_KEY`; an env var set inside a service unit still wins |
| `WHISPER_MODEL` | `medium` (GPU) / `small` (CPU) | Whisper model size; easiest via the tray menu (**Model**) — picking a model there replaces a user-level `WHISPER_MODEL`; an env var set inside a service unit still wins |
| `WHISPER_LANG` | `de` | Language code; empty string = auto-detect. Easiest via the tray menu (**Language**: Auto/De/En/It/Es/Fr/Kn/Zh) — applies to the next dictation, no restart; an env var set inside a service unit still wins |
| `PTT_BACKEND` | from `XDG_SESSION_TYPE` | Force `pynput` (X11/Windows/macOS; `x11` is accepted as an alias) or `wayland` |
| `PTT_TYPE_DELAY` | `0.01` | `pynput` backend only: seconds between typed characters; raise it (e.g. `0.03`) if dictated text arrives garbled in heavy editors (Electron, browsers) |
| `PTT_TYPE_DELAY_TERMINAL` | `0` | Windows + Linux/X11: delay used instead of `PTT_TYPE_DELAY` when the focused window is a terminal — consoles keep up with full-speed keystrokes, so dictation lands instantly there (Wayland pastes instead of typing, so no pacing applies) |
| `PTT_PASTE_KEY` | `ctrl+v` | Wayland only: the paste chord. **Terminals need `ctrl+shift+v`** |
| `PTT_KEEP_CLIPBOARD` | unset | Wayland only: `1` leaves the transcript in the clipboard instead of restoring the previous contents |
| `PTT_CLIPBOARD_SETTLE` | `0.4` | Wayland only: seconds before the previous clipboard is restored; raise it if a slow app pastes the restored value instead of the transcript |
| `PTT_UPDATE_CHECK` | `1` | Tray only: check GitHub releases at startup and every 4 h; a waiting update shows a notification + a **blue badge** on the tray icon (install is always one manual click); `0` disables the check |
| `PTT_OVERLAY` | `1` on Windows, else `0` | Show a small always-on-top pill at the top of the screen while recording/transcribing — useful when the taskbar hides the tray icon. `1` also enables it on X11. Needs tkinter, which `install.sh` installs on Linux (`python3-tk`); without it the overlay says so in the log and stays away. It is a Tk window, so on Wayland it goes through Xwayland — usually fine, but the tray icon is the supported indicator there |
| `PTT_STREAMING` | `0` | Experimental: while the key is held, completed speech blocks are already transcribed and inserted every few seconds instead of one big paste on release — see "Streaming mode" below |
| `PTT_STREAM_INTERVAL` | `3.0` | Seconds between streaming ticks (streaming mode only) |
| `PTT_CMD_TIMEOUT` | `30` | Seconds a helper (`wl-paste`, `ydotool`) may take before it is given up on. `0` waits indefinitely. Note `wl-copy` is never waited on at all — see below |

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

### Streaming mode (experimental)

With `PTT_STREAMING=1` the daemon already transcribes and inserts finished
blocks every few seconds while you are still holding the key, instead of
pasting everything on release. A block is only inserted once a pause follows
it in the audio — speech that runs to the edge of the current buffer is left
for the next round, so words are never cut in half. Whatever is left over is
transcribed on release, as before.

Notes:

- On CPU the inserted blocks trail your speech by a few seconds; with a GPU
  they are close to live.
- Each block is its own paste/typing burst, so stay in the window you are
  dictating into (sound advice either way, but it matters more here).
- With **AltGr** as the push-to-talk key the held modifier remaps letter
  keysyms at the compositor (AltGr+Shift+V = ‚ on a de layout), so a letter
  chord can never match mid-hold — and a synthetic release cannot clear a
  *physically* held modifier. On Wayland the daemon therefore pastes
  mid-hold blocks with `Shift+Insert` instead (level-stable in every keymap;
  works in GTK, Qt and terminals). On X11 (pynput typing) a modifier as the
  PTT key is not supported for streaming — use a plain key (F9, Menu) there.
- On Wayland every block is a full clipboard save → paste → restore cycle, so
  a clipboard manager records each block and the clipboard flickers briefly
  mid-hold.
- Off by default; enable with `Environment=PTT_STREAMING=1` (see "How to set
  them" above).

## Manage

The **tray icon** (the husky) shows the live state — greyed out when the daemon
is stopped, red badge while recording, amber while transcribing — and a blue
badge when an update is waiting (checked at startup and every 4 h). On Windows a
small always-on-top pill at the top of the screen shows the same state, so an
auto-hidden taskbar cannot swallow it. The menu lets
you switch the whisper model, restart the daemon, open the log folder, check
for updates and install a new release with one click. If the icon cannot be
shown (e.g. GNOME without an AppIndicator extension), the daemon keeps running
regardless — use the commands below then:

```bash
# Linux
systemctl --user status whisper-ptt.service      # state
journalctl --user -u whisper-ptt.service -f      # live logs
systemctl --user stop whisper-ptt.service        # pause
systemctl --user status ydotoold.service         # Wayland: the typing helper

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
./uninstall.sh                  # or: powershell -File uninstall.ps1  (Windows)
./uninstall.sh --purge          # also remove the venv  (Windows: -Purge)
./uninstall.sh --purge-system   # Linux: also remove udev rule + 'input' group
```

Also useful when something is off: run the daemon in the foreground and it
prints exactly which backend it picked, or refuses with the reason why:

```bash
.venv/bin/python dictate.py
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

## How the Wayland backend works

Wayland deliberately denies clients both halves of what a dictation tool needs,
so each is solved below the compositor:

**Reading the key.** There is no global key grab for clients, so the daemon
reads the key events straight from `/dev/input/event*` via `evdev`. That needs
membership in the `input` group.

**Inserting the text.** Clients cannot synthesize input either. `ydotool`
creates a virtual keyboard through `/dev/uinput`, which is a kernel device — the
compositor sees an ordinary keyboard and accepts the event.

**Why `wl-copy` is started and then let go.** Under Wayland the client offering
a selection *is* its owner for as long as the content stays on the clipboard, so
`wl-copy` cannot exit — it has to keep running. Waiting for it therefore blocks
until some other client takes over, which may be never. Worse, waiting *with a
timeout* kills it on the way out, and a killed owner leaves the compositor
referring to a dead client: from then on every clipboard operation in the
session hangs, system-wide, until you log out. So the daemon starts `wl-copy`,
writes the transcript, checks it is still alive (an instant exit means the text
never made it) and then leaves it alone. This is also why `PTT_CMD_TIMEOUT` does
not apply to it.

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

**One ydotoold, not two.** Two ydotoold units do not coexist — the loser exits
with `Another ydotoold is running with the same socket` and, under
`Restart=on-failure`, retries forever. Debian/Ubuntu ship `ydotool.service` and
enable it by preset, so it normally wins while the duplicate crash-loops in the
background; dictation keeps working, which is what makes this easy to miss.

The installer therefore uses the packaged `ydotool.service` when the
distribution provides one, and only installs its own `ydotoold.service` when
there is none. If a previous run left a redundant unit behind, re-running
`./install.sh` removes it.

**ydotool 0.1.x vs 1.x.** Both are supported; the daemon auto-detects which
key syntax (`ctrl+v` vs `KEYCODE:STATE`) and which socket the installed
version uses. Where they differ: 1.x honours `--socket-path` and defaults to
`$XDG_RUNTIME_DIR/.ydotool_socket`; 0.1.x (the Ubuntu 24.04/Debian 12 package)
ignores its arguments and always binds `/tmp/.ydotool_socket`, which is why the
daemon probes both locations rather than trusting one.

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
- Windows: synthetic typing cannot reach apps running **elevated**
  (as Administrator) unless the daemon itself runs elevated.
- macOS: no CUDA — CPU transcription only; the daemon needs the privacy
  permissions listed above.
- **The PTT key is observed, not swallowed.** The daemon watches it globally but
  does not consume the event, so the focused application still receives it —
  taking the key exclusively would mean grabbing the whole keyboard. With the
  default `ctrl_r` that is unnoticeable (a lone modifier does nothing), but a
  key that *acts* on its own will do both jobs: `f9` still triggers whatever
  `f9` does in the focused app, and `caps_lock` still toggles capitals. Prefer a
  modifier, or a key the apps you dictate into ignore. (On macOS, `ctrl_r` may
  not exist on laptop keyboards; try `f9` or `cmd_r`.)
- Transcription quality of names/jargon can be improved by switching to a
  larger model (`WHISPER_MODEL=large-v3`, needs ~6 GB VRAM) instead of `medium`.
- First run performs an unauthenticated Hugging Face model download (rate
  limits apply); after that it's fully offline.

## License

MIT (personal project; do what you want).
