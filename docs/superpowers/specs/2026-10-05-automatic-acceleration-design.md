# Automatic acceleration installation

The user requires every existing installation path on Windows, Linux and macOS
to work without a separate GPU setup command. Existing model/key/language choices
must survive installation and updates. OS microphone/accessibility grants remain
user-controlled; an unsupported GPU still permits CPU dictation.

## Design

Ship pinned, checksum-verified native whisper.cpp engines as release assets:
Windows x64 Vulkan, Linux x64/arm64 Vulkan, macOS x64/arm64 Metal. Bundle matching
archives in Windows and Debian installers; shell/source installs fetch the same
release assets. End users do not compile C++ or install a Vulkan SDK.

All installers invoke `setup_acceleration.py` after Python dependencies and before
autostart. Linux installs runtime Vulkan/Mesa dependencies through its package
manager; .deb declares these dependencies so postinst never nests apt transactions.
Python setup selects a working CUDA backend when available, otherwise probes the
native engine, and persists per-user selection independently from login-shell
environment. Explicit WHISPER_ENGINE overrides remain honored. Missing native
downloads/checksum failures fail setup, rather than falsely reporting GPU success.
CPU-only machines remain usable with an explicit diagnostic. macOS uses Metal
instead of requiring Vulkan. Updates use the same setup routine.

The runtime reads persisted engine settings, retaining existing explicit Vulkan
configuration compatibility. Native probing uses a small public model and confirms
actual backend initialization; software Vulkan is not treated as hardware acceleration.

## Verification

Test installer entry points, mocked failure/hardware selection cases, archive
integrity/path safety, settings persistence, native portable dependency closure,
and clean Windows/Linux/macOS CI environments. Real local AMD inference confirms
automatic installation without pre-set engine environment. Review and fix findings,
then publish an updated release and install it locally under the existing authorization.
