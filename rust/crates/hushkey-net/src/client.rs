use crate::{
    identity::Role,
    wire::{self, Channel, Io, Msg, Options},
    *,
};
use std::{
    net::TcpStream,
    time::{Duration, Instant},
};

/// Cloneable pinned peer handle. A fresh encrypted connection carries each request.
#[derive(Clone)]
pub struct Client {
    identity: Identity,
    peer: Peer,
}
impl Client {
    pub fn connect(identity: Identity, peer: &Peer, confirm: Approve) -> Result<Self, String> {
        validate_peer(peer)?;
        let _channel = open(&identity, peer, Some(confirm), None)?;
        Ok(Self {
            identity,
            peer: peer.clone(),
        })
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
        let mut ch = open(&self.identity, &self.peer, None, abort)?;
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
fn open(
    id: &Identity,
    p: &Peer,
    confirm: Option<Approve>,
    abort: Option<&dyn Fn() -> bool>,
) -> Result<Channel, String> {
    if abort.is_some_and(|f| f()) {
        return Err("request cancelled".into());
    }
    let trust_epoch = id.epoch()?;
    let server_pinned = id.authorized(Role::PinnedServer, &p.id, trust_epoch)?;
    let stream = TcpStream::connect_timeout(&p.address, Duration::from_secs(2))
        .map_err(|e| format!("cannot connect to peer: {e}"))?;
    let mut ch = Channel::handshake(Io::new(stream)?, id, true, abort)?;
    if ch.remote != p.id {
        return Err("peer identity changed: authenticated key does not match selected peer".into());
    }
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
    id.confirm(Role::PinnedServer, &p.id, trust_epoch, pairing)?;
    ch.io.idle = wire::IDLE;
    Ok(ch)
}
