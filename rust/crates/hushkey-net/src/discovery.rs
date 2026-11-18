use crate::{server::State, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    net::{Ipv4Addr, UdpSocket},
    sync::atomic::Ordering,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const MAX: usize = 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Packet {
    version: u8,
    kind: String,
    name: String,
    id: String,
    port: u16,
}
fn parse(bytes: &[u8]) -> Option<Packet> {
    if bytes.is_empty() || bytes.len() > MAX {
        return None;
    }
    let p: Packet = serde_json::from_slice(bytes).ok()?;
    if p.version != 2
        || !valid_name(&p.name)
        || !valid_id(&p.id)
        || !matches!(p.kind.as_str(), "query" | "reply")
    {
        return None;
    }
    Some(p)
}
pub(crate) fn parse_reply(bytes: &[u8], source: SocketAddr) -> Option<Peer> {
    let p = parse(bytes)?;
    if p.kind != "reply"
        || p.port == 0
        || !source.is_ipv4()
        || source.ip().is_unspecified()
        || source.ip().is_multicast()
    {
        return None;
    }
    Some(Peer {
        name: p.name,
        id: p.id,
        address: SocketAddr::new(source.ip(), p.port),
    })
}
pub(crate) fn listen(state: Arc<State>, address: SocketAddr) -> Result<JoinHandle<()>, String> {
    if !address.is_ipv4() {
        return Err("IPv4 discovery requires an IPv4 server bind".into());
    }
    let sock = UdpSocket::bind((address.ip(), PORT))
        .map_err(|e| format!("UDP discovery bind on port {PORT}: {e}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(err)?;
    sock.set_write_timeout(Some(Duration::from_millis(100)))
        .map_err(err)?;
    thread::Builder::new()
        .name("hushkey-net-discovery".into())
        .spawn(move || {
            let reply = serde_json::to_vec(&Packet {
                version: 2,
                kind: "reply".into(),
                name: state.identity.name(),
                id: state.identity.id(),
                port: address.port(),
            })
            .unwrap_or_default();
            // Receive a whole UDP packet: Windows reports WSAEMSGSIZE for truncation.
            // The parser still rejects anything larger than MAX before decoding.
            let mut bytes = [0; 65536];
            let mut window = Instant::now();
            let mut replies = 0;
            while !state.stop.load(Ordering::Acquire) {
                match sock.recv_from(&mut bytes) {
                    Ok((n, source)) => {
                        if window.elapsed() >= Duration::from_secs(1) {
                            window = Instant::now();
                            replies = 0;
                        }
                        if replies >= 32 || !state.enabled() {
                            continue;
                        }
                        // A loopback-bound test server must never advertise an unreachable LAN address.
                        if address.ip().is_loopback() && !source.ip().is_loopback() {
                            continue;
                        }
                        if let Some(p) = parse(&bytes[..n]) {
                            if p.kind == "query" && p.port == 0 && p.id != state.identity.id() {
                                replies += 1;
                                let _ = sock.send_to(&reply, source);
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        state.stop.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        })
        .map_err(err)
}
/// Broadcast discovery returns untrusted hints; connect verifies the selected key.
pub fn discover(identity: &Identity, timeout: Duration) -> Result<Vec<Peer>, String> {
    if timeout.is_zero() || timeout > Duration::from_secs(10) {
        return Err("discovery timeout must be between zero and ten seconds".into());
    }
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|e| format!("discovery socket: {e}"))?;
    sock.set_broadcast(true).map_err(err)?;
    sock.set_write_timeout(Some(Duration::from_millis(100)))
        .map_err(err)?;
    let query = serde_json::to_vec(&Packet {
        version: 2,
        kind: "query".into(),
        name: identity.name(),
        id: identity.id(),
        port: 0,
    })
    .map_err(err)?;
    let mut targets = BTreeSet::from([Ipv4Addr::LOCALHOST, Ipv4Addr::BROADCAST]);
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if let if_addrs::IfAddr::V4(a) = interface.addr {
                if let Some(b) = a.broadcast {
                    targets.insert(b);
                }
            }
        }
    }
    let targets: Vec<_> = targets.into_iter().take(64).collect();
    let until = Instant::now() + timeout;
    let mut next = Instant::now();
    let mut peers = HashMap::new();
    let mut bytes = [0; 65536];
    let mut sent = false;
    while Instant::now() < until {
        if Instant::now() >= next {
            for target in &targets {
                sent |= sock.send_to(&query, (*target, PORT)).is_ok();
            }
            next = Instant::now() + Duration::from_millis(300);
        }
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(remaining.min(Duration::from_millis(50))))
            .map_err(err)?;
        match sock.recv_from(&mut bytes) {
            Ok((n, source)) => {
                if let Some(p) = parse_reply(&bytes[..n], source) {
                    if p.id != identity.id() && (peers.len() < 256 || peers.contains_key(&p.id)) {
                        peers.entry(p.id.clone()).or_insert(p);
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("discovery receive: {e}")),
        }
    }
    if !sent {
        return Err("unable to send any discovery request".into());
    }
    let mut result: Vec<_> = peers.into_values().collect();
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}
