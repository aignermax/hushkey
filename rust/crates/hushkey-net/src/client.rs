use crate::{
    identity::Role,
    wire::{self, Channel, Io, Msg, Options},
    *,
};
use std::{
    net::TcpStream,
    time::{Duration, Instant},
};

/// An authenticated v2 host that predates the optional presence exchange.
pub const PROBE_UNSUPPORTED: &str = "this host version does not support connection checks";

/// Cloneable pinned peer handle. A fresh encrypted connection carries each request.
#[derive(Clone)]
pub struct Client {
    identity: Identity,
    peer: Peer,
}

/// Both peers approved the authenticated session. The caller still owns the
/// decision to persist the pin, e.g. after dispatching a result to a UI thread.
pub struct PendingClient {
    client: Client,
    trust_epoch: u64,
    pairing: bool,
}

impl PendingClient {
    pub fn finish(self) -> Result<Client, String> {
        self.client.identity.confirm(
            Role::PinnedServer,
            &self.client.peer.id,
            self.trust_epoch,
            self.pairing,
        )?;
        Ok(self.client)
    }
}

impl Client {
    pub fn connect(identity: Identity, peer: &Peer, confirm: Approve) -> Result<Self, String> {
        Self::prepare_connect(identity, peer, confirm, &|| false)?.finish()
    }

    /// Perform the encrypted exchange for an explicit selection. Cancellation
    /// interrupts the handshake and consent wait. Dropping the returned value
    /// before calling `finish` leaves the local trust store unchanged.
    pub fn prepare_connect(
        identity: Identity,
        peer: &Peer,
        confirm: Approve,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PendingClient, String> {
        let client = Self {
            identity,
            peer: peer.clone(),
        };
        validate_peer(peer)?;
        let (_, trust_epoch, pairing) =
            open(&client.identity, peer, Some(confirm), Some(cancelled))?;
        Ok(PendingClient {
            client,
            trust_epoch,
            pairing,
        })
    }
    /// Restore a previously pinned identity at a saved or rediscovered address.
    /// This reads local trust only. Call `probe` to verify the remote endpoint.
    pub fn resume(identity: Identity, peer: &Peer) -> Result<Self, String> {
        validate_peer(peer)?;
        require_pin(&identity, peer, identity.epoch()?)?;
        Ok(Self {
            identity,
            peer: peer.clone(),
        })
    }

    /// Time one encrypted ping and reply after verifying the host's static key.
    /// This never requests pairing or compute. Both grants must still be valid.
    pub fn probe(&self) -> Result<Duration, String> {
        let epoch = self.identity.epoch()?;
        require_pin(&self.identity, &self.peer, epoch)?;
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut ch = authenticated_channel(&self.identity, &self.peer, None, Some(deadline))?;
        let ping = Msg::Probe {
            name: self.identity.name(),
        };
        let start = Instant::now();
        ch.send_msg(&ping, None)?;
        let reply = ch.recv_msg(None);
        let rtt = start.elapsed();
        require_pin(&self.identity, &self.peer, epoch)?;
        match reply {
            Ok(Msg::Pong) => Ok(rtt),
            Err(e) if e == "remote: invalid protocol message" => Err(PROBE_UNSUPPORTED.into()),
            Err(e) => Err(e),
            _ => Err("invalid probe reply".into()),
        }
    }
    /// No automatic retries. Cancellation discards any late reply.
    pub fn transcribe(
        &self,
        audio: &[f32],
        options: &TranscribeOptions,
        abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcript, String> {
        if abort.is_some_and(|f| f()) {
            return Err("request cancelled".into());
        }
        if audio.is_empty() || audio.len() > wire::SAMPLES || audio.iter().any(|x| !x.is_finite()) {
            return Err("audio must contain 1..=9600000 finite 16 kHz mono samples".into());
        }
        let epoch = self.identity.epoch()?;
        // Revocation on the requesting computer is just as binding as host
        // revocation: stop uploads and discard replies throughout this request.
        let cancelled = || {
            abort.is_some_and(|f| f())
                || !self
                    .identity
                    .authorized(Role::PinnedServer, &self.peer.id, epoch)
                    .unwrap_or(false)
        };
        let abort: Option<&dyn Fn() -> bool> = Some(&cancelled);
        let options_final = options.beam_size > 1;
        let options = Options::from_core(options)?;
        let (mut ch, _, _) = open(&self.identity, &self.peer, None, abort)?;
        require_pin(&self.identity, &self.peer, epoch)?;
        ch.send_msg(
            &Msg::Request {
                samples: audio.len(),
                options,
            },
            abort,
        )?;
        ch.io.idle = if options_final {
            wire::FINAL_WAIT + wire::IDLE
        } else {
            wire::IDLE
        };
        if !matches!(ch.recv_msg(abort)?, Msg::Accepted) {
            return Err("expected request acceptance".into());
        }
        ch.io.idle = wire::IDLE;
        for chunk in audio.chunks(wire::CHUNK / 4) {
            let mut bytes = Vec::with_capacity(chunk.len() * 4);
            for sample in chunk {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            ch.send(&bytes, abort)?;
        }
        ch.io.idle = wire::COMPUTE;
        let bytes = match ch.recv_msg(abort)? {
            Msg::Reply { bytes } if bytes > 0 && bytes <= wire::REPLY => bytes,
            _ => return Err("invalid reply size or message".into()),
        };
        ch.io.idle = wire::IDLE;
        let mut result = Vec::with_capacity(bytes);
        while result.len() < bytes {
            let part = ch.recv(wire::CHUNK, abort)?;
            if part.is_empty() || part.len() > bytes - result.len() {
                return Err("invalid reply chunk".into());
            }
            result.extend_from_slice(&part);
        }
        if abort.is_some_and(|f| f()) {
            return Err("request cancelled".into());
        }
        wire::decode_result(&result)
    }
}
fn require_pin(id: &Identity, p: &Peer, epoch: u64) -> Result<(), String> {
    if id.authorized(Role::PinnedServer, &p.id, epoch)? {
        Ok(())
    } else {
        Err("host is not paired; reconnect explicitly to pair".into())
    }
}
fn validate_peer(p: &Peer) -> Result<(), String> {
    if !valid_id(&p.id)
        || !valid_name(&p.name)
        || p.address.port() == 0
        || p.address.ip().is_unspecified()
        || p.address.ip().is_multicast()
    {
        return Err("invalid peer identity, name or address".into());
    }
    Ok(())
}
fn authenticated_channel(
    id: &Identity,
    p: &Peer,
    abort: Option<&dyn Fn() -> bool>,
    deadline: Option<Instant>,
) -> Result<Channel, String> {
    if abort.is_some_and(|f| f()) {
        return Err("request cancelled".into());
    }
    let stream = TcpStream::connect_timeout(&p.address, Duration::from_secs(2))
        .map_err(|e| format!("cannot connect to peer: {e}"))?;
    let mut io = Io::new(stream)?;
    if let Some(deadline) = deadline {
        io.until = deadline;
    }
    let ch = Channel::handshake(io, id, true, abort)?;
    if ch.remote != p.id {
        return Err("peer identity changed: authenticated key does not match selected peer".into());
    }
    Ok(ch)
}
fn open(
    id: &Identity,
    p: &Peer,
    confirm: Option<Approve>,
    abort: Option<&dyn Fn() -> bool>,
) -> Result<(Channel, u64, bool), String> {
    let trust_epoch = id.epoch()?;
    let server_pinned = id.authorized(Role::PinnedServer, &p.id, trust_epoch)?;
    let mut ch = authenticated_channel(id, p, abort, None)?;
    ch.send_msg(
        &Msg::Hello {
            name: id.name(),
            server_pinned,
        },
        abort,
    )?;
    ch.io.idle = wire::APPROVAL;
    let (name, compute_granted) = match ch.recv_msg(abort)? {
        Msg::Offer {
            name,
            compute_granted,
        } if valid_name(&name) => (name, compute_granted),
        _ => return Err("invalid pairing offer".into()),
    };
    let pairing = !server_pinned || !compute_granted;
    let allowed = if pairing {
        match confirm {
            Some(f) => {
                let permit = Permit::take(&id.0.approval_busy)?;
                let approval = Approval {
                    peer: Peer {
                        name,
                        id: p.id.clone(),
                        address: p.address,
                    },
                    code: ch.code.clone(),
                };
                let rx = job(permit, move || f(approval))?;
                wait(rx, wire::APPROVAL, &|| {
                    abort.is_some_and(|f| f()) || Instant::now() >= ch.io.until
                })?
            }
            None => false,
        }
    } else {
        true
    };
    if !matches!(ch.recv_msg(abort)?, Msg::Decision { allowed: true }) {
        return Err("host denied pairing".into());
    }
    ch.send_msg(&Msg::Decision { allowed }, abort)?;
    if !allowed {
        return Err("client denied pairing; reconnect explicitly to pair".into());
    }
    if !matches!(ch.recv_msg(abort)?, Msg::Ready) {
        return Err("pairing confirmation missing".into());
    }
    ch.io.idle = wire::IDLE;
    Ok((ch, trust_epoch, pairing))
}
