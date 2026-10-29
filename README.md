<p align="center">
  <img src="assets/logo.png" width="200" alt="hushkey logo — a winking husky holding a key">
</p>
<h1 align="center">hushkey</h1>

Local, offline push-to-talk dictation for **Windows, Linux (X11 and Wayland)
and macOS**: hold <kbd>Caps Lock</kbd>, speak, release, and the transcript
lands in whatever window has focus. Terminal, editor, browser, chat: any app.

One small native program (Rust, [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
inside): GPU-accelerated through **Vulkan** on Windows and Linux (AMD, NVIDIA,
Intel) and **Metal** on macOS, CPU where there is no GPU. No Python, no setup.
Nothing leaves your machine after the one-time model download.

## Install

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/aignermax/hushkey/master/install.ps1 | iex
```

**Linux and macOS**:

```bash
curl -fsSL https://raw.githubusercontent.com/aignermax/hushkey/master/install.sh | bash
```

The installer downloads the program for your system from the
[latest release](https://github.com/aignermax/hushkey/releases/latest), checks
it against `SHA256SUMS`, and the program installs itself: a menu entry, start
at login, a tray icon. Add `-Pre` (PowerShell) or `--pre` (bash) for
pre-releases. You can also download the program from the release yourself and
run it with `--install`.

| | program | settings, history, log |
|---|---|---|
| Windows | `%LOCALAPPDATA%\Programs\hushkey\hushkey.exe` | `%LOCALAPPDATA%\hushkey-rs` |
| Linux | `~/.local/bin/hushkey` | `~/.local/share/hushkey-rs` |
| macOS | `~/Applications/hushkey.app` | `~/Library/Application Support/hushkey-rs` |

**Linux:** the key is read from `/dev/input` and text is inserted through a
virtual keyboard (`/dev/uinput`); both need the `input` group. The installer
sets this up once (it asks for your password; log out and in once
afterwards).

**macOS:** grant hushkey **Microphone** and **Accessibility** access when asked
(System Settings › Privacy & Security); the key and the inserting depend on
them.

**Coming from the Python hushkey (whisper-ptt)?** Nothing to do: its updater
offers this version, and installing it moves you over, keeping your model,
key, language and recent dictations. The Python app is switched off; its
folder stays and can be deleted.

## Use

| | |
|---|---|
| Dictate | Hold the key, speak, release: the text is inserted where the cursor is (nothing is sent; review and press Enter yourself) |
| Caps Lock | A short tap still toggles capitals; only a hold dictates |
| Continuous listening | Tray menu › *Continuous listening*: every sentence you say is inserted, no key needed; one listening session is one history entry |
| Recent dictations | Tray menu › *Recent dictations*: click one to copy it again |

While you speak, a small pill at the top of the screen with the mouse shows
what has been heard so far.

The tray menu holds the settings: **model** (tiny … large-v3; large-v3-turbo
is the default and a good balance), **key** (Caps Lock, Right Ctrl, F8, F9,
Scroll Lock), **language** (or auto-detect), **insert method** (paste via the
clipboard, which is restored afterwards, or typed keystrokes), **start at
login**, **logs**, and **updates**.

## Updates

hushkey looks for a newer release at start and every four hours and offers it
in the tray menu (*Install update: v…*); nothing installs unasked. Installing
downloads the new program, checks it against the release's `SHA256SUMS`,
swaps it in and restarts. While you run a pre-release, newer pre-releases are
offered too; `"updates": "pre"` or `"off"` in `config.json` changes that.

## Advanced settings

`config.json` in the settings folder also holds what the menu does not:
`restore_clipboard`, `end_silence_ms` (pause that ends a sentence in
continuous listening), `use_gpu`, `commands` (voice commands, see
`docs/superpowers/specs/2026-10-28-rust-core-design.md`) and `brain` (an
OpenAI-compatible or Claudio server that answers what you say in continuous
listening).

## Uninstall

Windows: `powershell -ExecutionPolicy Bypass -File uninstall.ps1`, or run
`hushkey.exe --uninstall`. Linux and macOS: `bash uninstall.sh`, or run
`hushkey --uninstall`. Settings, history and log stay.

## Build from source

The Rust workspace is in [`rust/`](rust/): the core library with a C ABI
(`hushkey-core`, `hushkey-ffi`, header in `rust/include/hushkey.h`), the
command-line tool (`hushkey-cli`) and the desktop app (`hushkey-app`).

```bash
cd rust
cargo build --release -p hushkey-app --features vulkan   # or metal, or neither for CPU
```

whisper.cpp is built by `whisper-rs-sys` and needs CMake, a C++ compiler and
libclang; Vulkan needs the Vulkan SDK (Windows) or `libvulkan-dev` and
`glslc` (Linux). Releases are built by `.github/workflows/release.yml`.

## Limitations

- Text cannot be inserted into programs running as administrator (Windows)
  unless hushkey runs elevated too.
- On Wayland the overlay pill cannot choose its position; the compositor
  places it.
- The first use of a model downloads it from Hugging Face (75 MB to 3 GB);
  after that hushkey is fully offline.

## License

MIT.
