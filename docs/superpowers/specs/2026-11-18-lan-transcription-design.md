# LAN transcription

Approved direction: the user requested full implementation, testing, PR review and fixes, integration into dev and a new release on 2026-11-18. No further design approval is required.

## Intent

A laptop captures speech and inserts the result locally while a trusted LAN computer runs Whisper on its GPU. Each desktop installation includes both roles. Only push-to-talk is supported remotely; continuous listening must explain that it requires local computation.

## Approach

An embedded server in the same app preserves the requested discover/select/approve/use experience. An external Whisper endpoint alone would not provide device approval; automatic multi-server scheduling is unnecessary for this first release.

UDP IPv4 broadcast discovery returns device name, stable public-key identity and protocol version. Untrusted advertisements are hints only. Direct encrypted connections use the standard Noise XX protocol with X25519, ChaChaPoly and SHA256, implemented by snow. This replaces the earlier proposed TLS/WebSocket transport with equivalent authenticated encrypted peer sessions without certificate provisioning. First pairing compares a handshake-bound code on both devices and requires explicit host approval with privacy disclosure, plus client confirmation. Paired keys are pinned, revocable and stored separately from ordinary configuration. Never trust a device name or IP address as identity.

## Behavior

The tray offers network discovery, discovered peers with names and IDs, local computation, server availability, and revoking device permissions. Selecting a peer runs pairing off the UI thread; approval dialogs are delivered through the app event loop. Startup restores the selected peer without downloading a local model. Discovery and pairing work while local models load. The server uses a loaded local engine, with explicit loading/busy errors, and never delegates a remote request again.

Local capture buffers the current dictation. Preview snapshots and the final recording are sent as bounded encrypted audio requests. This deliberately reuses existing RecordSession preview scheduling instead of introducing a second microphone/session implementation. Audio is 16 kHz mono f32; no lossy compression. The network worker never runs on the real-time microphone callback. The final request supersedes preview work. No automatic switch to an unknown server or silent local fallback.

An interchangeable transcription interface preserves existing local API consumers. A failed final transcription is an error, never an empty successful dictation. Errors are visible. Remote recording, preview and final-computation overlays always show the selected server name. Changing computation destination during a recording or pending final is rejected. Audio is retained only in memory for active requests; transport failures allow an explicit retry of the retained final without capturing again or inserting twice. A new recording cannot overwrite a retained failed dictation without explicit discard.

## Security and resilience

Encryption protects transport; the approved server necessarily sees speech and text in memory. Consent says so. No server audio/transcript files or content logs. Limit handshake clients, frame sizes, audio duration, request lifetime and pending approvals. Ignore malformed discovery traffic and self advertisements. Network failure must not crash the app, hang the tray, generate duplicate insertion, or become silent success. Revocation affects future requests and checked active sessions. A model is shared rather than duplicated; inference scheduling is bounded and reports busy.

The server advertises by default while Hush-Key runs, but no audio is accepted before pairing consent. Users can disable availability; UDP discovery and direct connections must respect that setting. Firewall restrictions and separate broadcast domains are documented; Windows installer configures application-scoped private-network rules where elevated installation permits it. No internet relay or port forwarding.

## Verification

Real encrypted loopback tests cover approval/denial, identities, revocation, samples/options/results, malformed input, disconnects and deadlines. Core tests cover fake-backend capture/preview/final/error propagation independently of Whisper. Existing workspace and real-model inference tests remain green. Test actual network Whisper inference with a local model and WAV when available. Review security, lifecycle and GUI integration independently; fix findings before merging. CI builds/tests Windows, Linux and macOS and release assets through the existing workflow.
