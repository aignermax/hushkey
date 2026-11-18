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
Speech stays on your machine by default. Optionally, let a trusted computer
on your LAN run recognition over an encrypted connection.

## Install

Easiest: **[hushkey.eu](https://hushkey.eu)** has one button per system.

Or take the file for your system straight from the
[latest release](https://github.com/aignermax/hushkey/releases/latest):

| System | File | What it is |
|---|---|---|
| **Windows** 10+, 64-bit | `hushkey-setup-windows-x86_64.exe` | Setup, ~5 MB: installs the program. (`hushkey-windows-x86_64.exe` is the program itself, ~12 MB; run once, it installs itself too.) |
| **Linux** Debian/Ubuntu, 64-bit | `hushkey_<version>_amd64.deb` | Package: `sudo apt install ./hushkey_*.deb` |
| **Linux** Debian/Ubuntu, ARM | `hushkey_<version>_arm64.deb` | Same, for a Raspberry Pi and the like |
| **Linux** any distribution | `hushkey-linux-x86_64`, `…-aarch64` | Plain program: `chmod +x` it, then `./hushkey --install` |
| **macOS** Apple Silicon | `hushkey-macos-arm64.zip` | hushkey.app |
| **macOS** Intel | `hushkey-macos-x86_64.zip` | hushkey.app |

`SHA256SUMS` lists the checksum of every file above; the installers check it
for you. The `.xz` files beside the programs are the same programs
compressed (the program is mostly Vulkan shaders, which pack 9:1); updates
download those.

Or in one line —

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
afterwards). On X11 hushkey also works without it: the X server then
reports the key and takes the paste keystrokes. Wayland needs the group.

**macOS:** grant hushkey **Microphone** and **Accessibility** access when asked
(System Settings › Privacy & Security); the key and the inserting depend on
them.

**Coming from the Python hushkey (whisper-ptt)?** Nothing to do: its updater
offers this version, and installing it moves you over, keeping your model,
key, language and recent dictations. The Python app is switched off; what it
leaves behind (its folder with a Python environment of 2–3 GB, its engines,
and on Linux its .deb, on Windows its entry in Settings › Apps) goes with one
click on *Remove the old hushkey version* in the tray menu, which is there
only while something is left. Removing the old .deb asks for your password in
the usual system dialog.

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

## Use another computer's GPU

Install this version of Hush-Key on both computers. On the powerful computer,
keep **Network → Offer LAN computation (approval required)** enabled and wait
for its local model to load. On the laptop:

1. Choose **Network → Find network computers**. The search uses local IPv4
   broadcasts; no account, cloud directory or internet relay is involved.
2. Open the Network menu again and select the computer by name and device ID.
3. Both computers show the same eight-digit comparison code and a privacy
   notice. Compare the codes and click **OK on both computers**. Cancel if
   they differ. Device names and discovery responses alone are not trusted.
4. Dictate as usual. **Remote · computer name** appears in the preview and
   computation status. Text is inserted on the laptop, including into a
   focused remote-desktop window where that software forwards input.

Permissions and the selected computer survive restarts. Selecting a computer
as a server does not grant it access to your own computation; the reverse
direction requires its own host approval. A remote-only client
does not load or download a Whisper model on startup. The server's loaded
model determines recognition quality; use its Model menu to change it.
**Continuous listening is supported only with local computation.** Choose
**Network → Compute locally** before enabling it.

The direct connection uses Noise XX (X25519, ChaCha20-Poly1305, SHA-256).
Audio and recognition results are encrypted in transit. **The approved server
necessarily decrypts and processes speech in memory.** Hush-Key writes no
server recordings or transcripts. The requesting laptop keeps its normal
dictation history. Only approve computers and administrators you trust.
**Revoke device approvals** removes stored trust and closes active sessions;
both devices must approve a new pairing. Disabling the server immediately
stops discovery replies and disconnects its clients.

If a final request fails, the current recording stays temporarily in memory.
Use **Retry failed dictation** or **Discard failed dictation** in the Network
menu; a new dictation cannot overwrite it. There is no automatic destination
switch or automatic replay. If its address changes or permission needs renewal,
rediscover and reconnect to the same device ID; the retained audio survives.
A server processes one network request at a time. Final requests wait a bounded
time for previews to finish; excess preview requests report busy. Remote requests are
limited to ten minutes of audio. A running computation can finish after a
disconnect, but its late result is discarded.

Both computers must allow **TCP and UDP port 47957** on their trusted local
network. On Windows, **Network → Allow network access (private network)**
asks for administrator permission and creates executable-scoped inbound rules
for the Private profile and local subnet only. If Windows already allowed
the app through its firewall prompt, this extra action is unnecessary. On
Linux/macOS, allow those ports in your firewall. On macOS, also allow Local
Network access when asked (System Settings → Privacy & Security → Local
Network). Guest Wi-Fi client isolation, VPNs and separate broadcast domains
may prevent discovery. No port forwarding
is needed. Discovery reveals the computer name and stable device ID to the
local network; switch off offering computation to stop advertising.

For an isolated portable profile, set `HUSHKEY_DATA_DIR` to a separate folder
before launch. On Windows it also creates a separate single-instance lock;
use a different hotkey if running two profiles. Profiles share the model
cache, but keep settings, history and network keys separately.

## Updates

hushkey looks for a newer release at start and every four hours and offers it
in the tray menu (*Install update: v…*); nothing installs unasked. Installing
downloads the new program, checks it against the release's `SHA256SUMS`,
swaps it in and restarts. Installed from the `.deb`, the update is the next
`.deb`, installed through apt after the system's password dialog. While you run a pre-release, newer pre-releases are
offered too; `"updates": "pre"` or `"off"` in `config.json` changes that.

## Advanced settings

`config.json` in the settings folder also holds what the menu does not:
`restore_clipboard`, `end_silence_ms` (pause that ends a sentence in
continuous listening), `use_gpu`, `commands` (voice commands, see
`docs/superpowers/specs/2026-10-28-rust-core-design.md`) and `brain` (an
OpenAI-compatible server that answers what you say in continuous
listening).

## Uninstall

Windows: Settings › Apps › hushkey › Uninstall, or
`powershell -ExecutionPolicy Bypass -File uninstall.ps1`, or run
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
