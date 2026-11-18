# LAN transcription verification

## Scope

The approved feature is push-to-talk only. Desktop client and host roles,
broadcast discovery, comparison-code pairing, role-specific device grants,
encrypted inference, visible remote status and recoverable errors are included.
Continuous listening remains local. Version: 0.12.0.

## Implementation decisions

- Noise XX via snow replaces certificate-based TLS provisioning. No plaintext
  fallback. Device public keys are authenticated and pinned after consent.
- Existing RecordSession sends preview snapshots and one final recording,
  preserving local capture and the existing preview cadence. This trades
  repeated preview audio uploads for a smaller, shared recording implementation.
- Remote capture is capped at ten minutes. A visible cutoff notice is emitted.
- Synchronous native inference cannot be forcibly stopped safely. A cancelled
  preview's result is discarded; final requests get bounded priority and wait
  up to 30 seconds for computation. Preview overload fails quickly.
- Listener failure preserves the client role. Disabled hosts bind no inbound
  socket. The GUI offers a Windows private-subnet firewall action via UAC.
- Failed audio stays in RAM until retry/discard or process exit. Explicit
  reconnection to the same pinned identity preserves it across an address or
  permission change. Changing destination identity requires discarding it.

## Independent review and fixes

The full-branch reviewer inspected the change against base 590d357 and found:

1. Outbound pairing originally reused inbound host grants. Fixed with separate
   pinned-server and compute-grant sets and reverse-direction approval tests.
2. A final request could collide with its cancelled preview. Fixed with bounded
   final scheduling, matching client acceptance deadlines and preview priority
   tests, including a preview longer than the ordinary I/O idle timeout.
3. Blocking pairing dialogs could delay hotkey releases. Replaced with native
   nonblocking requests, cancellation/expiry, and host approval deferral during
   recording/listening/insertion. Quit, disable and revoke cancel prompts.
4. A delayed outbound pairing could restore trust after revocation. Fixed with
   a persisted revocation epoch and atomic, role-specific conditional commits.
5. Failed audio blocked needed reconnection. Fixed by preserving the retained
   request while replacing only the connection to the same device identity.
6. Follow-up review found that laptop-side revocation also needed to cancel
   established outbound requests and invalidate queued results. Captured pin
   epochs now guard every upload/acceptance/reply wait, and permission-generation
   stamps reject queued dictation results before history or insertion. A real
   blocked-inference test failed before the fix and passed afterward; another
   test covers revocation while the final request waits for acceptance.
7. The app review identified continuous listening being enabled during a local
   dictation/final computation. The mode switch now refuses both active states.
8. A successful backend result queued before revocation must remain retryable
   when the GUI rejects it. Final audio is now released only after GUI acceptance;
   rejected queued completions become retryable requests. The regression covers
   success, revocation/rejection, same-device reconnection, retry and acceptance.

The review did not constitute a cryptographic audit. Platform CI and native
smoke checks supplement source inspection. No findings are accepted as waived
solely because they were absent from the original design.

## Local evidence

Windows development environment uses Ninja, libclang, MSVC, GGML_CCACHE=OFF
and a short target directory. Build/cache setup errors were corrected before
feature verification; they were not counted as product test failures.

- Core tests demonstrated RED (missing backend interface, cancel and capture
  limit), then GREEN against actual RecordSession capture and event delivery.
- Configuration and recovery tests demonstrated missing remote-mode behavior,
  then verified persistence, remote/listen rejection, retry and overlay labels.
- Transport tests use actual loopback sockets and Noise encryption, including
  denial, changed keys, revocation, malformed input, cancellation, saturation,
  broadcast discovery and server shutdown.
- Full workspace tests ran with the real tiny Whisper model, public JFK WAV
  and Silero VAD; the explicit encrypted real-model roundtrip also passed.
- Desktop smoke: an isolated CPU/tiny host advertised HushKey-TestHost; the
  broadcast client discovered its name, stable ID and reachable address.
  The native approval window appeared. After approval, the encrypted request
  returned recognizable public JFK speech. Two independent client identities
  were exercised. The test host was stopped and the pre-existing Hush-Key
  process was left running.
- Two isolated desktop profiles showed identical pairing codes concurrently;
  UI Automation exposed the full hosting privacy text. A remote-only client
  configured with an intentionally nonexistent model did not load/download it.
- The real recording-session integration stops while encrypted preview
  inference is still running and successfully obtains exactly one final event.
- The native Windows dialog expiry check returned immediately and closed its
  own dialog automatically (passed in approximately 0.18 seconds).
- CI exposed a restored CMake cache pointing to an MSVC version absent on the
  new Windows runner. The workflow now selects MSVC before restoring and hashes
  its version/location into the native cache identity.
- GitHub expired the macos-latest job without assigning a hosted ARM runner.
  The identical native macOS test suite now uses macos-15-intel, preserving
  all tests; separate release builds still compile both Mac architectures.
- Packaging review corrected the macOS microphone privacy description for
  approved remote recognition and added NSLocalNetworkUsageDescription, as
  required by Apple's local-network privacy guidance. The plist is parsed
  locally and the actual signed bundles are linted in macOS CI.

## Release gate

Run formatting, workspace clippy with warnings denied, the full real-fixture
workspace suite, explicit real-model LAN inference and native dialog expiry
checks after review fixes. PR targets dev, created from master because dev did
not exist. Merge only after checks and review findings are resolved. Publish
v0.12.0 through the existing multi-platform release workflow and verify assets.
