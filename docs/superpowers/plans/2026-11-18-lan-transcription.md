# LAN Transcription Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Ship discoverable, consented, encrypted LAN push-to-talk transcription in the desktop app.

**Architecture:** A new hushkey-net crate owns discovery, identities, encrypted peer connections and bounded inference transport. Core RecordSession consumes a Transcriber trait. The app owns destination choice, approval dialogs, server lifecycle, visible mode and recoverable dictation errors.

**Tech Stack:** Rust, snow Noise XX, serde, existing whisper-rs core and native tray/overlay implementations.

**Spec:** docs/superpowers/specs/2026-11-18-lan-transcription-design.md

## Global Constraints

- Only push-to-talk is supported remotely.
- Audio is 16 kHz mono f32; no lossy compression.
- No server audio/transcript files or content logs.
- Never trust a device name or IP address as identity.
- Network worker never runs on the real-time microphone callback.

## Review Focus

- Spoofed discovery or changed keys cannot bypass handshake-bound approval and identity pinning.
- Lost server, denied request, busy server and malformed peer data produce visible bounded failures.
- Stale responses and a computation switch cannot insert into the wrong recording or insert twice.
- Disabled server/revoked client cannot keep obtaining inference through existing connections.
- Startup and GUI approval must work without loading a Whisper model on the remote-only client.

### Task 1: Core backend boundary and honest session errors

**Files:** rust/crates/hushkey-core/src/{engine,session,lib}.rs; rust/crates/hushkey-core/tests/record_backend.rs

**Interfaces:** Transcriber: Send + Sync with transcribe_cancellable(&self, &[f32], &TranscribeOptions, Option<&dyn Fn()->bool>) -> Result<Transcript>; Engine implements it. RecordSession::start accepts Arc<dyn Transcriber>. Existing Arc<Engine> callers continue to coerce. stop returns final backend errors.

- [x] Write capture/preview/final/failure tests against an actual RecordSession with an in-memory backend, run and observe missing-interface failure.
- [x] Implement trait boundary and preserve final Result through worker and stop.
- [x] Run core and workspace tests; check existing API consumers; commit.

### Task 2: Encrypted LAN transport

**Files:** rust/crates/hushkey-net/**; rust/Cargo.toml; rust/Cargo.lock

**Interfaces:** Identity::load_or_create(&Path); Peer{name,id,address}; Approval{peer,code}; Server::start(identity,bind,approve,compute); Server::set_enabled/revoke_all/local_addr; discover(&Identity,Duration); Client::connect(identity,&Peer,confirm); Client::transcribe(audio,options,abort).

- [x] Write and run failing real loopback protocol, denial and validation tests.
- [x] Implement bounded Noise sessions, pinned persisted peers, UDP discovery and callback server.
- [x] Run transport tests including altered identity/revocation/disconnect and optional real Whisper; commit.

### Task 3: Native desktop integration

**Files:** rust/crates/hushkey-app/src/{network,app,config,tray,strings,main}.rs; platform dialog helpers if needed; packaging/windows/setup.iss

**Interfaces:** Network owns Identity, Server and shared Option<Arc<Engine>> host slot; network events deliver Approval with reply Sender<bool>, discovery and connected clients on the app channel. Config persists selected Peer and server_enabled. NetworkBackend implements Transcriber by Client delegation.

- [x] Add tests for backwards-compatible config, remote/listen rejection, mode label and recoverable final errors; run failing tests.
- [x] Wire nonblocking discovery, tray selection, dialogs, host engine, startup, preview labels and visible errors/retry/discard.
- [x] Prevent mid-dictation switches; share the host engine without remote recursion; add private-network firewall instructions/rules.
- [x] Run format, clippy, tests, native build and UI smoke verification; commit.

### Task 4: Review and release

**Files:** README.md; rust/README.md; CI workflows; rust/Cargo.toml version; docs with verification evidence.

- [x] Document setup, privacy, firewall, limits and remote-only restrictions.
- [x] Run full workspace tests with real-model fixtures and an encrypted real-model roundtrip.
- [x] Fresh read-only review of full branch; reproduce and fix findings with regression tests.
- [x] Open PR to dev (create from master because origin/dev is absent), wait for checks and fix failures.
- [x] Merge after clean review/checks; tag new version and verify release artifacts complete.

Published v0.12.0 with all 12 assets; all 11 payload checksums were verified
after download. The final PR Apple Silicon artifact was reused because GitHub
could not assign a tag-build ARM runner; the complete Git tree matches exactly.
See the verification record for build provenance and remaining physical setup.
