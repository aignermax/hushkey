# hushkey-net

Noise `XX_25519_ChaChaPoly_SHA256` over framed TCP, plus unauthenticated IPv4
UDP discovery hints. Identity is the lowercase SHA256 of the authenticated
static public key, never the name or address. Protocol v2 is bound into the
Noise prologue. No plaintext transport or downgrade exists.

The UI owns consent wording and must explain that the host sees audio and
text in memory. Both approval callbacks must return true on first pairing;
the client callback must compare the displayed eight decimal digits with the
host. Discovery alone never grants trust. Callbacks run off network threads.

## Resource contract

16 kHz mono finite f32 audio, at most 9,600,000 samples (ten minutes).
Transport frames are limited to 65,535 bytes including the Noise tag. JSON
control messages and transcript replies have explicit smaller/aggregate limits.
At most eight accepted handlers, one pending host approval, and one inference
callback per server. Previews (beam size <=1) return server busy immediately.
Finals (beam size >1) wait at most 30 seconds, with priority over new previews,
before audio upload/allocation. Waiting finals share the eight-handler cap.

`Client::connect` performs pairing without audio. Each `transcribe` opens a new
pinned session. Outbound server pins and inbound compute grants are separate.
Selecting a server never grants it reverse compute access. Both roles require
their own consent. Requests are never retried internally: a lost reply is an error
and caller-side insertion must happen only once after a successful return.
Cancellation closes the connection and discards any eventual result.

Callbacks are synchronous user code and cannot be safely forcibly stopped.
Network waits and handler lifetimes are bounded; a blocked callback holds its
bounded worker slot until it returns. A running inference may finish after
cancellation, disable, revocation, or server drop; its result is then discarded.
No audio or transcript is written to disk or logged by this crate.

`Server::start` enables service by default and starts discovery on UDP `PORT`
for normal binds. For a loopback ephemeral test bind, explicitly call
`start_discovery()` if discovery is needed. UDP bind/start errors are returned.
Discovery sends limited broadcast, loopback and interface directed broadcasts;
firewalls and separate broadcast domains can prevent discovery. Advertisements
are plaintext, untrusted, and reveal only stable device identity and name.

Dependencies are declared locally so only workspace membership and an optional
`hushkey-net = { path = "crates/hushkey-net" }` workspace dependency are needed.
The crate deliberately does not implement the application's Transcriber trait.

Current limits and verification commands are recorded in [TESTING.md](TESTING.md).
The manual `peer` example has a discovery JSON mode and a TEST ONLY client
which automatically confirms its displayed SAS; it is for desktop smoke tests.


## Revocation and migration

`Identity::clear_trust()` is public and persistently clears **both** role sets,
advancing a revocation epoch under the interprocess identity lock. When hosting,
use `Server::revoke_all()` to additionally abort live network sessions. Pending
pairing commits on either endpoint check the epoch captured before the Noise
handshake, under the same lock. Separately loaded Identity handles see the same
persisted epoch; a delayed Ready or stale dialog cannot restore revoked trust.

Trust format v2 stores `pinned_servers` separately from `compute_grants`.
Legacy role-ambiguous trust sets are discarded on load and require re-pairing.
The Noise prologue and discovery version are v2, rejecting older wire semantics.
There are at most 256 pins and 256 compute grants. Device private keys and IDs
are preserved during this migration.
