# AMD Vulkan validation

Validated on 2026-10-05 for PR #29. All audio below is the public 11-second
[`jfk.wav` sample](https://github.com/ggml-org/whisper.cpp/blob/927cfce34f31707e17f2bff35c349632fb9e2c3a/samples/jfk.wav).
No microphone recording or private dictation was used.

## Local hardware and performance

- Windows, AMD Ryzen 9 9900X, Radeon RX 7600 XT (16 GB), driver 32.0.31041.1004.
- whisper.cpp v1.9.4 (`927cfce34f31707e17f2bff35c349632fb9e2c3a`), built with
  Vulkan and verified to select `Vulkan0`, the RX 7600 XT.
- Model: multilingual `medium`; English pinned; beam size 5; VAD enabled.

| Engine | First transcription | Second | Third |
| --- | ---: | ---: | ---: |
| whisper.cpp Vulkan, FP16 | 1.079 s | 0.505 s | 0.469 s |
| faster-whisper 1.2.1 CPU, INT8 | 3.558 s | 3.424 s | 3.416 s |

The warm Vulkan calls were about **7 times faster** than the existing CPU
engine on this sample. Model download/loading is excluded; the Vulkan model
stays resident. Different compute precision/backends and one short English
sample mean this is a practical spot check, not a general accuracy or speed
benchmark. There was no NVIDIA comparison.

## Integration checks

- Full local suite with native tests enabled: **192 passed, 49 skipped**.
  Skips are existing tests requiring other platforms or unavailable desktop
  dependencies. Both new native tests ran, rather than being skipped.
- Real tiny-model Vulkan inference, repeated requests, silence handling,
  Unicode model filename, and press/release → native inference → insertion.
  Microphone and desktop injection are test doubles; no text was typed into
  another application during testing.
- Real native CPU-only startup is rejected when Vulkan was requested.
- Supervisor subprocess tests cover normal close, repeated close, startup
  timeout, forced application termination, and Windows `pythonw` pipe handling.
- Windows setup builds and installs the statically linked engine using detected
  Visual Studio/CMake. Both installers include the adapter and setup helper.
- CI builds on Windows and Ubuntu. Linux inference uses Mesa lavapipe with
  explicit `GGML_VK_VISIBLE_DEVICES=0`; this tests Vulkan API compatibility,
  not acceleration on a Linux AMD card. Windows CI checks real model loading
  with Unicode paths and rejection of CPU fallback, without requiring a GPU.

## Review and fixes

An independent code reviewer found three issues, all addressed:

1. Missing `spirv-headers` in Linux prerequisites: added to setup documentation
   and CI, then verified with the native Linux build.
2. Native verbose JSON enabled token-level wrapping that could split words:
   explicitly disable token timestamps while retaining segment timestamps.
3. Windows ANSI argv broke non-ASCII model paths: embed a UTF-8 active-code-page
   manifest and test a real model named `模型-é.bin`.

Additional integration findings fixed during validation:

- PyAV 19 removed `metadata_errors`, still passed by faster-whisper 1.2.1:
  constrain PyAV below 19; existing real audio test passes again.
- Detect Visual Studio 2022/2026 and request UTF-8 output from `vswhere`.
- Automatically injected Vulkan layers caused multi-minute stalls locally.
  Disabling **only implicit layers**, with the AMD driver unchanged, reduced
  repeated tiny-model 4-second inference to approximately 0.05 seconds.
  The adapter applies this setting only to its child process and permits an
  explicit debugging opt-in. No registry or system driver changes were made.

## Reproduce

Build using `python setup_vulkan.py` after installing the README prerequisites.
Point `WHISPER_CPP_SERVER` at that binary if using a custom prefix, set
`HUSHKEY_TEST_VULKAN=1` and `HUSHKEY_TEST_CPP_BINARY=1`, and run `python -m pytest -q`.
For software Vulkan CI, additionally set `GGML_VK_VISIBLE_DEVICES=0`; do not set
this on a normal installation expecting GPU acceleration.
