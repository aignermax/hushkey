# AMD Vulkan dictation on Windows and Linux

The requested outcome is a reviewed PR providing local Whisper GPU dictation on
AMD (in particular RX 7600 XT) on Windows and Linux. Existing NVIDIA and CPU
users keep their current engine. Performance parity with NVIDIA is not promised.

Add an opt-in `WHISPER_ENGINE=vulkan` engine using whisper.cpp v1.9.4. A managed
loopback server keeps the model resident across dictations. It accepts the same
audio, language, prompt and segment interface used by dictation and batch mode.
Serialize requests; retain original timestamps for streaming; run the existing
Silero VAD to mask silence without collapsing the audio timeline. Explicit Vulkan
selection must fail clearly if startup did not actually select a Vulkan GPU.

Build the pinned upstream source with a Python setup helper usable on both OSes.
Support an explicit server path and GPU index. Download GGML models on first use
through the existing Hugging Face cache, with support for a local model path.
Package both helper and adapter in the Windows installer and Linux deb.

Bind only to loopback with a random request-path token. A separate supervisor
owns the server and stops it when the application's pipe closes, including forced
tray shutdown on Windows. Do not persist transcripts in diagnostic logs. A server
failure must give an error instead of silently switching to CPU or duplicating text.

Validation: unit/HTTP contract and lifecycle tests; Windows/Linux CI builds and
real tiny-model inference; local Windows AMD GPU inference; full existing suite;
independent branch review and fixes. Linux hardware acceleration remains a
hardware-dependent verification limitation when only a software CI device exists.
