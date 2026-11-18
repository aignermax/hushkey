# Verification evidence — 2026-11-18

All edits are confined to this crate. No commits or pushes were made.

## Test-first evidence

1. Wrote real loopback, approval/denial, pinning, revoke/disable, malformed
   framing, cancellation, discovery, busy and lifecycle tests before production
   implementation. The first Cargo attempt stopped at missing workspace
   membership. A temporary crate-local isolated manifest then compiled the
   tests and failed with 31 missing transport API errors (E0422/E0425/E0433).
   Native CMake/sccache setup failures were resolved separately, not counted
   as behavioral red results.
2. After implementation, 11 real transport tests passed.
3. Added simultaneous SAS-dialog regression. It failed with `host denied
   pairing` because the host callback timed out waiting for the client
   callback to become visible. Moved client confirmation after Offer and
   before receiving host Decision. All 12 tests then passed.
4. Added an actual 4096-byte UDP datagram to the discovery test. On Windows
   it failed with `oversized UDP datagram stopped server`. Changed the fixed
   UDP receive buffer to hold a complete packet, retaining the 1024-byte
   parser limit. All 12 tests passed again.
5. Added authenticated malformed binary requests, invalid AEAD tag,
   inference cancellation/no retry, active revocation and bounded handlers
   with blocked approval coverage. Final ordinary suite: **16 passed,
   0 failed, 1 ignored** on Windows, in about four seconds. Unix-only private
   permission repair coverage is included but was not executed here.

## Real Whisper

Explicitly ran `real_model_encrypted_roundtrip` with:

- HUSHKEY_TEST_MODEL pointing to the local `ggml-tiny.bin`
- HUSHKEY_TEST_SPEECH=`C:/dev/tools/hk-testdata/jfk.wav`

Result: **1 passed**, about 0.8 seconds. The host callback uses the real
hushkey-core Engine on CPU and the test checks exact equality of its actual
Transcript with the encrypted client result, plus recognizable JFK speech.
An initial test compared separate Engine invocations and failed on punctuation;
that assumption was corrected because repeated inference is not guaranteed to
produce identical punctuation. No production transport change was needed.

## Commands

From the Rust workspace, with the native developer environment configured:

```text
cargo test -p hushkey-net --lib
cargo test -p hushkey-net real_model_encrypted_roundtrip -- --ignored
cargo clippy -p hushkey-net --all-targets --no-deps -- -D warnings
```

Clippy passed, including the manual example, after final formatting. Used target
`C:/t/hushkey-net`, LIBCLANG_PATH=`C:/dev/tools/libclang/clang/native`, Visual
Studio vcvars64, Ninja, GGML_CCACHE=OFF, CXXFLAGS=/Z7 and
CMAKE_BUILD_PARALLEL_LEVEL=1. The UDP discovery test needs port 47957 free.

Manual desktop smoke client:

```text
cargo run -p hushkey-net --example peer -- STATE_DIR discover
cargo run -p hushkey-net --example peer -- STATE_DIR ADDRESS ID NAME WAV
```

The second command is TEST ONLY: client confirmation is automatic and labelled;
the desktop host still requires human approval. This crate's automated tests
use real sockets and snow encryption throughout, not mocked encryption.

## Integration contract and limits

Agreed API is preserved. Additional methods are `Server::start_discovery()` and
`Server::alive()`. `revoke_all(&self) -> Result<(), String>` surfaces trust-file
failures, invalidates live sessions and disables service if persistence fails.
`Client` and `Identity` are Clone + Send + Sync. `Server::start` enables service
by default. PORT is 47957 for UDP discovery and default TCP service.

Private identity/trust DTOs and atomic replacement are local to the crate;
Unix identity, lock and trust files are chmod 0600, directory 0700. Device names
are persisted, limited to 64 Unicode characters/256 UTF-8 bytes without controls.

Limits: eight accepted handlers; one host approval callback; one inference
callback reserved before upload; 256 trusted keys; 256 discovery peers; 32 UDP
responses/second; 64 broadcast destinations. Frames 65,535 ciphertext bytes;
control messages 16 KiB; chunks 60,000 plaintext bytes; reply 2 MiB; audio
9,600,000 finite samples at 16 kHz mono. Prompt 8192 bytes; beam width <=32.
Connect and normal I/O idle timeout two seconds; approval 120 seconds; compute
180 seconds; global connection deadline 240 seconds; cancellation polling
<=100 ms during network waits. Discovery accepts timeouts up to ten seconds.

Synchronous caller callbacks cannot be forcibly interrupted safely. A stuck
callback retains its bounded slot; network handlers and Server::drop do not wait
for its completion. A running computation may finish after cancellation but its
reply is discarded. No automatic request retry or host content logging exists.

Dependencies, all declared in this crate: hushkey-core (path), snow 0.10, sha2
0.10, serde 1 (derive), serde_json 1, if-addrs 0.13, fs2 0.4, tempfile 3. The last
two provide interprocess identity-file locking and private atomic replacement.
Workspace membership/dependency and root lockfile integration belong to the
main worker. The crate uses version.workspace=true.

## Independent security review follow-up

Implemented only inside hushkey-net after the independent review:

- Split outbound `pinned_servers` from inbound `compute_grants`; neither role
  authorizes the other. Private Hello/Offer booleans explicitly represent
  `server_pinned` and `compute_granted`. Wire/discovery protocol is v2.
- Store a checked, persisted revocation epoch alongside the role sets, serialized
  by identity.lock. Capture before handshake and condition both endpoints' trust
  commits on that epoch. Public Identity::clear_trust clears both sets and bumps
  the epoch. Server::revoke_all additionally invalidates policy and live sockets.
- Fail closed on old role-ambiguous trust: migrate to empty v2 permissions while
  preserving device keys. Bounds are 256 IDs per role; file limit is 64 KiB.
- Finals (beam_size >1) wait at most 30 seconds for inference, before accepting
  audio. Client acceptance idle is 32 seconds, restored to two seconds after
  Accepted. Preview requests (beam_size <=1) fail immediately when busy or when
  any final is waiting. A drop guard counts waiting finals; cancellation,
  revocation and deadlines release it. All waiting handlers share the cap of 8.

Red/green evidence:

1. Five new real-encryption regressions failed against the previous production
   code: outbound-pin reverse compute authorization, inbound-grant outbound pin,
   revocation before delayed Ready, revocation during host approval, and final
   scheduling after preview cancellation. After fixes all five passed. The final
   scheduling regression holds the original compute for 2.3 seconds while a
   final is pending, proving acceptance survives the old two-second idle limit.
2. Added migration/epoch reload and final-queue cancellation/revoke coverage.
3. Added a deterministic final-priority test using a real network preview with
   a registered final waiter and a free compute permit. It failed because the
   preview computed; after adding priority checks it passed. No crypto mocks.
4. Full tests exposed a production Windows lock-contention bug: the unchanged
   malformed/disconnect test repeatedly failed with connection reset, including
   in isolation. Epoch reads now occur before handshake, increasing contention.
   fs2 reports Windows ERROR_LOCK_VIOLATION rather than ErrorKind::WouldBlock;
   the old check rejected ordinary contention immediately. Added a failing
   held-lock/reloaded-Identity regression, then recognized fs2's platform-specific
   contention error while retaining the finite two-second lock deadline.
5. Final verification on Windows, version 0.12.0:
   `cargo test -p hushkey-net --all-targets -- --include-ignored` with the model/WAV
   fixture environment: **26 passed, 0 failed, 0 ignored** (plus example target
   compiled, zero example tests), about four seconds for the test suite.
   `cargo clippy -p hushkey-net --all-targets --no-deps -- -D warnings`: passed.
   Unix-only file permission coverage remains unexecuted on this Windows host.

No new dependencies or public signature changes except the requested promotion
of Identity::clear_trust to public. Port remains 47957. Raw temporary run logs
and machine-specific command wrappers were removed after recording this summary.
