# hushkey (Rust) everywhere: Linux, macOS, self-update, and the bridge from Python

Status: in progress (branch `feat/rust-everywhere`). Builds on
`2026-10-28-rust-app-design.md`.

## Goals

1. The Rust app runs on Windows, Linux (X11 and Wayland) and macOS with the
   same features: hold-to-talk key, paste/type, tray menu (model, key,
   language, insert, history, listening, autostart, logs, update), overlay.
2. The app updates itself from GitHub releases.
3. Existing Python installs move to the Rust app through their own updater
   (the "bridge"), on every system, keeping their settings and history.
4. Releases carry one ready-to-run download per system.

## Versions, tags, assets

- The Rust app continues the Python version line: workspace version
  `0.11.0`. Pre-releases are tagged `v0.11.0-rc.N` and published as GitHub
  *pre-releases*; the bridge release is `v0.11.0` (a normal release).
- `releases/latest` (what the Python updater asks) ignores pre-releases, so
  release candidates never reach Python installs.
- Assets on every release (built by `.github/workflows/release.yml`):
  - `hushkey-windows-x86_64.exe` (Vulkan build)
  - `hushkey-linux-x86_64` (plain executable, Vulkan build; libvulkan1 at
    runtime)
  - `hushkey-macos-arm64.zip` (`hushkey.app`, Metal build)
  - `SHA256SUMS` (sha256sum format, one line per asset)

## The platform interface

`crates/hushkey-app/src/app.rs` is the app, the same everywhere. Everything
that differs per system lives in `src/win/`, `src/linux/`, `src/macos/`,
re-exported as `crate::platform` (see `main.rs`). Each provides:

| item | meaning |
|---|---|
| `init()` | first thing at start (DPI on Windows, GTK init on Linux, NSApplication on macOS) |
| `fatal(&str) -> ExitCode` | show a start failure to the user, log it |
| `already_running() -> bool` | single instance per user session |
| `pump() -> bool`, `wait(Duration)` | run the system's event loop (the tray lives on it); `pump` false = quit |
| `Hotkey::install(name, Fn(KeyEvent))`, `KeyEvent {Down, Up}` | the hold-to-talk key by config name (`caps_lock`, `right_ctrl`, `f8`, …) |
| `since_last_seen()`, `forget_press()` | release-unseen watchdog (may return `None` where releases are always seen) |
| `tapped(name)` | the key was only tapped: give it its own job back if it was swallowed |
| `insert(text, paste, restore) -> Result<Restore, String>`, `Restore: Debug` | put text into the focused window |
| `copy(text)` | put text on the clipboard |
| `Overlay::new()`, `show(rgb, text)`, `hide()` | the preview pill near the top of the screen with the mouse |
| `autostart::{enabled(), set(bool)}` | start at login |
| `open_folder(path)` | show the log folder |
| `setup::*` | install, uninstall, update apply, Python retirement (below) |

The tray (`src/tray.rs`) is shared: `tray-icon`/`muda` on all three systems.

## Install and self-update

The program installs itself; installers are thin.

- `hushkey --install [--from-python]`: copy the running executable (macOS:
  its `.app`) to the install location, add the menu entry, enable autostart,
  with `--from-python` import the Python settings and history and retire the
  Python app (autostart and menu entry off, process stopped; its files stay),
  then start the installed copy.
- `hushkey --uninstall`: autostart, menu entry and program removed; settings
  and log stay.

| | program | menu entry | autostart |
|---|---|---|---|
| Windows | `%LOCALAPPDATA%\Programs\hushkey\hushkey.exe` | Start Menu `hushkey.lnk` | HKCU Run `hushkey-rs` |
| Linux | `~/.local/bin/hushkey` | `~/.local/share/applications/hushkey.desktop` | `~/.config/autostart/hushkey.desktop` |
| macOS | `~/Applications/hushkey.app` | (the app itself) | `~/Library/LaunchAgents/com.hushkey.app.plist` |

Self-update (`src/update.rs`, shared): at start and every 4 h, ask
`GET /repos/aignermax/hushkey/releases`, take the highest version tag that
carries this system's asset (pre-releases only when the running version is
one, or `"updates": "pre"` in config.json). A newer one shows "Install
update vX" in the tray menu; nothing installs unasked. Installing: download
the asset and `SHA256SUMS`, check the hash, then `platform::setup::apply`
puts it in place of the running program (Windows: rename the running exe
aside, then move the new one in; Linux: rename over; macOS: swap the
`.app`), starts it with `--after-update`, and the old process quits. The new
process removes leftovers.

## The bridge from Python

The Python updater (tray.py) checks `releases/latest`; when it is newer than
its version it replaces its files with **master**'s and runs master's
`install.ps1` / `install.sh` (update_helper.py), then starts `tray.py` again.
So the bridge is master's content at the time `v0.11.0` is released:

- `install.ps1` / `install.sh` download this system's asset from the latest
  release, check `SHA256SUMS`, and run `hushkey --install --from-python`.
  The same scripts are the one-line installers for new users.
- `tray.py` becomes a small shim: if the Rust app is installed it starts it
  (when not running) and exits; otherwise it runs the installer. This covers
  the update helper's restart, the systemd unit's restart (Linux) and the
  LaunchAgent (macOS) until `--from-python` removed them.

Python settings imported: `model`, `ptt_key` (`ctrl_r`→`right_ctrl`,
`alt_r`/`alt_gr`→`right_alt`, others by name), `insert`, `lang` (`auto` →
detect), history.json (same `text` entries).
