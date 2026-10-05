# Automatic acceleration implementation plan

**Goal:** Every existing installation path automatically provisions and selects
the appropriate inference backend without a separate build/setup command.

**Architecture:** A shared Python bootstrap installs pinned native release assets
and stores per-user backend settings; existing shell/PowerShell/package entry
points call it. CI builds portable Vulkan and Metal engines and bundles them.

**Spec:** ../specs/2026-10-05-automatic-acceleration-design.md

## Interfaces and ownership

- Runtime task: `setup_acceleration.py` CLI (no arguments needed), `acceleration.py`
  settings, adapter Metal support and dictate/transcribe integration. Native files
  are ZIP archives `hushkey-engine-{windows,linux,macos}-{x64,arm64}.zip`, containing
  `bin/whisper-server[.exe]`. `native-manifest.json` maps asset names to SHA256
  strings under `assets`; top-level `version` is `v0.9.0`. Bundled files live in
  repository/application `native/`; otherwise download from v0.9.0 release assets.
- Installer task: install.sh, install.ps1, Linux package scripts/control, Windows
  Inno script and related tests. Call venv Python `setup_acceleration.py`; preserve
  its nonzero status and do not start a misleading successful install on failure.
- Build task: setup_vulkan.py Metal/portable builds; native packaging helper,
  workflows, release integration and documentation.

## Global constraints

No source compilation on ordinary end-user installs. Preserve user settings.
No silent claimed GPU success. OS permissions are never bypassed. Test actual
native startup and audio on available hardware; do not claim physical Mac/Linux
GPU coverage from software-only CI.

## Tasks

- [ ] Add failing tests for engine choice, persisted settings, corruption and
  traversal, overrides, missing/unsupported GPUs; implement runtime bootstrap.
- [ ] Add installer routing tests and prerequisites for every existing vector;
  implement common setup invocation before autostart, including update paths.
- [ ] Add build tests for Metal, portable libraries and archives; create native
  build matrix and bundle resulting assets into release installers.
- [ ] Run local regression/native AMD tests and fresh CI matrix; resolve failures.
- [ ] Independent review of installer ordering, .deb package-manager locking,
  environment precedence, device fallback and release asset bootstrapping.
- [ ] Publish release, install via actual packaged installer without pre-set
  backend environment, and verify persisted automatic AMD selection/inference.
