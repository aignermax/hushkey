# AMD Vulkan implementation plan

> Execute inline with superpowers:executing-plans, then an independent whole-branch review.

**Goal:** Reviewed PR for AMD GPU dictation on Windows and Linux.
**Spec:** ../specs/2026-10-05-amd-vulkan-design.md
**Architecture:** A persistent whisper.cpp process adapts to the existing model
interface. Keep faster-whisper as the default; explicit Vulkan never silently
falls back to CPU. Setup builds pinned upstream source.
**Tech stack:** Python, whisper.cpp 1.9.4, CMake, Vulkan, existing PyAV/Silero.

## Global constraints

- Windows and Linux; Python >= 3.10.
- Existing NVIDIA/CPU behavior and public model settings continue to work.
- No cloud inference, transcript logging, or automatic system driver changes.

## Review focus

- Process cleanup when tray forcibly terminates a daemon.
- Wrong executable, wrong GPU, startup timeout, and CPU fallback.
- Chinese prompt isolation and original streaming timestamps after silence.
- Spaces/Unicode in paths, local models, and missing build prerequisites.
- Installer payload completeness and actual Windows/Linux execution.

## Tasks

- [x] Adapter and integration: run tests/test_whisper_cpp.py RED; implement
  whisper_cpp.py, wire dictate.load_model and transcribe.main; verify GREEN.
  Include lifecycle cleanup, invalid engine, silent audio and HTTP failures.
- [x] Setup and distribution: add setup_vulkan.py, include runtime/helper in both
  packages, document exact prerequisites and commands, test setup failures and
  package payloads; add Windows/Linux real engine CI.
- [x] Validate full suite and local RX 7600 XT inference, commit, independent
  review, fix findings with regression tests, create PR, inspect all CI checks.

## Execution notes

- Baseline: 167 passed, 49 skipped, 1 failure: PyAV 19 removed metadata_errors,
  which faster-whisper 1.2.1 still passes. Constrain this incompatible dependency
  and rerun the existing real-audio regression test.
- Authorization: user explicitly requested end-to-end implementation, PR,
  review and fixes. Routine implementation decisions proceed within that scope.
- Independent review completed; all three findings fixed with regression checks.
  See ../../amd-vulkan-validation.md for hardware results and integration evidence.
- Ruling: opt-in engine preserves current installations; installing Vulkan build
  tools remains an explicit documented setup step rather than a system change
  performed by the application. Cost: AMD users run one extra setup command.
- Ruling: implicit Vulkan layers disabled only in the inference child after a
  controlled experiment isolated severe local stalls to those layers. Cost:
  capture/overlay debugging requires WHISPER_CPP_ALLOW_LAYERS=1.
- Final-head CI acceptance is tracked on PR #29; keep the PR draft until its
  platform checks are green. Prior native Windows/Linux builds and inference
  checks passed; the final test-only change removes reverse DNS from local HTTP
  fixtures, which exceeded the fixture startup budget on hosted macOS.
