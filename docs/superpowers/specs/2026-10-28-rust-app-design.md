# hushkey app in Rust (phase 4 of the Rust core)

## Why

Phases 1 to 3 made hushkey's recognition a library: push-to-talk sessions,
continuous listening, voice commands and brains, behind a C ABI. The user
still runs the Python app for dictation. Phase 4 builds the app on the Rust
core, so one installable binary can replace Python, venv and pip, the part
that has caused most installer trouble. Recognition speed does not change:
it was always whisper.cpp.

## Goals (this phase)

1. `hushkey-app`, a single binary `hushkey` for **Windows first**:
   - hold a key to dictate (default Caps Lock, as the user has it); the key
     is swallowed, so Caps Lock does not toggle;
   - the transcript goes into the focused window by pasting, and the
     previous clipboard text comes back afterwards, with the same safety
     rules as the Python app since 0.10.8 (delayed rendering, restore only
     after the target fetched the text plus a settle time, only while
     hushkey still owns the clipboard); typing (Unicode key events) as the
     fallback and as an option;
   - a tray icon showing idle / recording / transcribing, with a menu:
     push-to-talk or continuous listening, Quit;
   - continuous listening inserts each utterance; with a command set and a
     brain configured, commands are only reported (log) and replies are
     shown as a tray notification;
   - settings in `%LOCALAPPDATA%\hushkey-rs\config.json`, created with
     defaults on first start.
2. Other platforms compile (CI) and report "not supported yet" at start.

## Non-goals

- Replacing the Python app in the installer or auto-update. That needs
  feature parity (preview overlay, history, X11/Wayland/macOS input,
  updates) and is a later decision.
- The preview overlay window, dictation history, update checks.

## Anti-goals

- Pasting the user's previous clipboard instead of the dictation.
- A stuck modifier or Caps Lock state after hushkey exits or crashes.
- Typing into the wrong window: text goes to whatever has focus at
  release, as with the Python app; nothing is buffered across windows.

## Decided

- Windows APIs through `windows-sys`: a `WH_KEYBOARD_LL` hook on its own
  thread with a message loop; `SendInput` for the paste chord and Unicode
  typing; the clipboard through a message-only window that owns it.
- Tray through `tray-icon` on the main thread's message loop; icon images
  drawn in code (no asset files).
- Models resolve like the CLI (HF cache, download), Vulkan via the core's
  feature; `init_process()` first in `main`.

## Left to the builder

Module layout, config field names, icon drawing, how the main loop is
woken by worker threads, test seams.
