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
| **Windows** 10+, 64-bit | `hushkey-setup-windows-x86_64.exe` | Windows installer. You can also run the standalone `hushkey-windows-x86_64.exe`. |
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
| Recent dictations | Open **Settings → Recent dictations** to read or copy the last three entries. They are also available from the tray menu. |

While you speak, a small pill at the top of the screen with the mouse shows
what has been heard so far.

Click the tray icon to open **Settings**. On Linux, choose **Settings…** in
the indicator menu. **Dictation** holds the talk key, language, model, GPU
preference and insertion settings, and shows where recognition runs. **Connections** shows the selected server, measured
response time, host readiness and active clients. **General** holds startup,
updates, appearance and the running program's version and build identifier.
Changes take effect in the running app. Close the window to keep dictating
from the tray. Right-click the tray icon for shortcuts and recent dictations.

On a new installation, **Settings → Dictation → Local model** starts on
**Automatic**. Before loading a local model, hushkey checks the devices its
recognition backend can use and their available memory. It chooses a model
for that hardware and shows the loaded model and actual device in Settings.
Turn off GPU use to have Automatic pick a CPU model. To enable hardware-based
selection on an existing installation, change its saved model to **Automatic**.

On Windows, Settings uses Direct3D 12 and can display the same window with
Microsoft's WARP software renderer when a hardware adapter is unavailable.
The GPU preference under **Dictation** controls speech recognition.

## Use another computer's GPU

Install this version of Hush-Key on both computers. On the powerful computer,
keep **Network → Offer LAN computation (approval required)** enabled and wait
for its local model to load. On the laptop:

1. Open **Settings → Connections** and switch to the client view. Refresh the
   list of computers on your LAN.
2. Check the device ID against **Connections → As host** on the computer you
   intend to use. Clicking **Connect** authorizes that device to process your
   dictation. The client shows the pairing code while it waits. You can cancel
   the request there.
3. On the host, HushKey opens **Connections → As host**. Compare the eight
   digits with the code on the client, then allow the requesting computer to
   dictate. Decline the request if the codes differ or you did not expect it.
4. After pairing, HushKey tests the connection and displays the reply time.
   Use **Test connection** to repeat the encrypted round trip.
   The host window lists active clients. Its model status shows when it is
   ready to transcribe. These checks continue after you close the window.
5. Dictate as usual. **Remote · computer name** appears in the preview.
   On Windows, release the key over RustDesk and wait for the clipboard hint.
   Press **Ctrl+V** in the remote session with RustDesk clipboard sync enabled.

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
**Revoke device approvals** removes stored trust and closes active sessions.
Pair again with **Connect** on the client and a new approval on the host.
Disabling the server immediately
stops discovery replies and disconnects its clients.

If a final request fails, the current recording stays temporarily in memory.
Use **Retry** or **Discard** in the connection window. Hush-Key checks the
selected host periodically and looks for its saved identity if its address
changes. A restored connection leaves the choice to resend the recording
with you. If permission was revoked, click **Connect** to pair again.
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

Open **Settings → General** and choose **Check for updates** next to the
installed version. Enable **Include prereleases** to receive release
candidates as well. Automatic checks run after startup and every four hours
by default. You can turn them off and still check manually.

When a newer eligible version is available, the settings window shows an
update notice. Choose **Install update** to download it, verify it against
the release's `SHA256SUMS`, and restart HushKey. You can also right-click
the Windows tray icon and choose **Check for updates**, then **Install update**.
This also works if an older version cannot open Settings.
On a Debian package installation, apt installs the verified `.deb`
after the system's password dialog.

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
