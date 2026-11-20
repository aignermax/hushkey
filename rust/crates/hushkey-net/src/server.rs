use crate::{
    identity::Role,
    wire::{self, Channel, Io, Msg},
    *,
};
use std::{
    collections::HashMap,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicU64, AtomicUsize},
        Mutex, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct Server {
    pub(crate) state: Arc<State>,
    address: SocketAddr,
    listener: Option<JoinHandle<()>>,
    discovery: Mutex<Option<JoinHandle<()>>>,
}

/// Liveness of a specific inbound request. UI code may keep a clone while the
/// user compares the code and discard the request when its connection ends.
#[derive(Clone)]
pub struct ApprovalActive {
    pending: Arc<AtomicBool>,
    server: Weak<State>,
    epoch: u64,
}

impl ApprovalActive {
    pub fn is_active(&self) -> bool {
        self.pending.load(Ordering::Acquire)
            && self.server.upgrade().is_some_and(|s| s.valid(self.epoch))
    }
}

pub type HostApprove = Arc<dyn Fn(Approval, ApprovalActive) -> bool + Send + Sync>;

pub(crate) struct State {
    pub identity: Identity,
    pub stop: AtomicBool,
    pub live: AtomicBool,
    pub policy: Mutex<Policy>,
    connections: Mutex<HashMap<u64, TcpStream>>,
    active: AtomicUsize,
    serial: AtomicU64,
    approval_busy: Arc<AtomicBool>,
    compute_busy: Arc<AtomicBool>,
    waiting_finals: AtomicUsize,
    approve: HostApprove,
    compute: Compute,
}
pub(crate) struct Policy {
    pub enabled: bool,
    pub epoch: u64,
    // Oldest first. A client's ephemeral TCP port can change on every probe.
    clients: Vec<(Peer, Instant)>,
}
const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

impl Policy {
    fn remember(&mut self, peer: Peer, now: Instant) {
        self.clients.retain(|(p, seen)| {
            p.id != peer.id && now.saturating_duration_since(*seen) < CLIENT_TIMEOUT
        });
        // Trust normally bounds this to 256. Also bound it if an external
        // process replaces the grants before our previous observations expire.
        if self.clients.len() == 256 {
            self.clients.remove(0);
        }
        self.clients.push((peer, now));
    }
    fn clients_at(&self, now: Instant) -> Vec<Peer> {
        let mut clients: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, seen)| now.saturating_duration_since(*seen) < CLIENT_TIMEOUT)
            .map(|(peer, _)| peer.clone())
            .collect();
        clients.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        clients
    }
}
impl State {
    pub fn enabled(&self) -> bool {
        !self.stop.load(Ordering::Acquire)
            && self
                .policy
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .enabled
    }
    fn valid(&self, epoch: u64) -> bool {
        let p = self.policy.lock().unwrap_or_else(|e| e.into_inner());
        !self.stop.load(Ordering::Acquire) && p.enabled && p.epoch == epoch
    }
    fn close_connections(&self) {
        for s in self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}
impl Server {
    pub fn start(
        identity: Identity,
        bind: SocketAddr,
        approve: Approve,
        compute: Compute,
    ) -> Result<Self, String> {
        Self::start_with_approval_status(identity, bind, Arc::new(move |a, _| approve(a)), compute)
    }

    pub fn start_with_approval_status(
        identity: Identity,
        bind: SocketAddr,
        approve: HostApprove,
        compute: Compute,
    ) -> Result<Self, String> {
        let tcp = TcpListener::bind(bind).map_err(|e| format!("TCP server bind {bind}: {e}"))?;
        let address = tcp.local_addr().map_err(err)?;
        tcp.set_nonblocking(true).map_err(err)?;
        let state = Arc::new(State {
            identity,
            stop: AtomicBool::new(false),
            live: AtomicBool::new(true),
            policy: Mutex::new(Policy {
                enabled: true,
                epoch: 0,
                clients: Vec::new(),
            }),
            connections: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
            serial: AtomicU64::new(0),
            approval_busy: Arc::new(AtomicBool::new(false)),
            compute_busy: Arc::new(AtomicBool::new(false)),
            waiting_finals: AtomicUsize::new(0),
            approve,
            compute,
        });
        let st = state.clone();
        let listener = thread::Builder::new()
            .name("hushkey-net-accept".into())
            .spawn(move || accept(tcp, st))
            .map_err(err)?;
        let server = Self {
            state,
            address,
            listener: Some(listener),
            discovery: Mutex::new(None),
        };
        if !(bind.ip().is_loopback() && bind.port() == 0) {
            server.start_discovery()?;
        }
        Ok(server)
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub fn alive(&self) -> bool {
        self.state.live.load(Ordering::Acquire) && !self.state.stop.load(Ordering::Acquire)
    }
    /// Verified clients seen in the last 15 seconds, ordered by device ID.
    /// Addresses are observed TCP source addresses, including ephemeral ports.
    pub fn clients(&self) -> Vec<Peer> {
        if !self.alive() {
            return Vec::new();
        }
        self.state
            .policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clients_at(Instant::now())
    }
    pub fn set_enabled(&self, enabled: bool) {
        {
            let mut p = self.state.policy.lock().unwrap_or_else(|e| e.into_inner());
            p.enabled = enabled;
            if !enabled {
                p.epoch = p.epoch.wrapping_add(1);
                p.clients.clear();
            }
        }
        if !enabled {
            self.state.close_connections();
        }
    }
    /// Clear persisted device grants and invalidate all accepted sessions.
    pub fn revoke_all(&self) -> Result<(), String> {
        let result = {
            let mut p = self.state.policy.lock().unwrap_or_else(|e| e.into_inner());
            p.epoch = p.epoch.wrapping_add(1);
            p.clients.clear();
            let r = self.state.identity.clear_trust();
            if r.is_err() {
                p.enabled = false;
            }
            r
        };
        self.state.close_connections();
        result
    }
    /// Explicit discovery startup for loopback ephemeral test servers. Idempotent.
    pub fn start_discovery(&self) -> Result<(), String> {
        let mut handle = self.discovery.lock().map_err(err)?;
        if handle.is_none() {
            *handle = Some(crate::discovery::listen(self.state.clone(), self.address)?);
        }
        Ok(())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        self.state.close_connections();
        if let Some(h) = self.listener.take() {
            let _ = h.join();
        }
        if let Some(h) = self
            .discovery
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = h.join();
        }
    }
}
struct Connection {
    state: Arc<State>,
    number: u64,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.state
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.number);
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }
}
fn accept(tcp: TcpListener, state: Arc<State>) {
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    while !state.stop.load(Ordering::Acquire) {
        let mut i = 0;
        while i < threads.len() {
            if threads[i].is_finished() {
                let _ = threads.swap_remove(i).join();
            } else {
                i += 1;
            }
        }
        match tcp.accept() {
            Ok((socket, addr)) => {
                if !state.enabled() || state.active.load(Ordering::Acquire) >= 8 {
                    let _ = socket.shutdown(Shutdown::Both);
                    continue;
                }
                let clone = match socket.try_clone() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let number = state.serial.fetch_add(1, Ordering::Relaxed);
                state
                    .connections
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(number, clone);
                state.active.fetch_add(1, Ordering::AcqRel);
                let guard = Connection {
                    state: state.clone(),
                    number,
                };
                let st = state.clone();
                if let Ok(t) = thread::Builder::new()
                    .name("hushkey-net-peer".into())
                    .spawn(move || {
                        let _guard = guard;
                        let _ = serve(socket, addr, st);
                    })
                {
                    threads.push(t);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            // Some platforms report an individual reset/abort on accept.
            // The listening socket can continue serving the other peers.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                state.stop.store(true, Ordering::Release);
                state.close_connections();
                break;
            }
        }
    }
    // Network waits and callback-result waits observe stop at <=100ms intervals.
    for t in threads {
        let _ = t.join();
    }
    state.live.store(false, Ordering::Release);
}
fn serve(socket: TcpStream, addr: SocketAddr, state: Arc<State>) -> Result<(), String> {
    let epoch = state.policy.lock().map_err(err)?.epoch;
    let trust_epoch = state.identity.epoch()?;
    let cancelled = || !state.valid(epoch);
    let mut ch = Channel::handshake(Io::new(socket)?, &state.identity, false, Some(&cancelled))?;
    let result = session(&mut ch, addr, &state, epoch, trust_epoch);
    if let Err(ref e) = result {
        ch.error(e);
    }
    result
}
fn session(
    ch: &mut Channel,
    addr: SocketAddr,
    state: &Arc<State>,
    epoch: u64,
    trust_epoch: u64,
) -> Result<(), String> {
    let cancelled = || !state.valid(epoch);
    let (name, server_pinned) = match ch.recv_msg(Some(&cancelled))? {
        Msg::Probe { name } if valid_name(&name) => {
            // A periodic check must never initiate a pairing dialog.
            if !state
                .identity
                .authorized(Role::ComputeGrant, &ch.remote, trust_epoch)?
            {
                return Err("device permission revoked; reconnect explicitly to pair".into());
            }
            let mut policy = state.policy.lock().map_err(err)?;
            if !policy.enabled || policy.epoch != epoch {
                return Err("server unavailable or permission revoked".into());
            }
            policy.remember(
                Peer {
                    name,
                    id: ch.remote.clone(),
                    address: addr,
                },
                Instant::now(),
            );
            drop(policy);
            return ch.send_msg(&Msg::Pong, Some(&cancelled));
        }
        Msg::Hello {
            name,
            server_pinned,
        } if valid_name(&name) => (name, server_pinned),
        _ => return Err("invalid peer hello".into()),
    };
    let compute_granted = state
        .identity
        .authorized(Role::ComputeGrant, &ch.remote, trust_epoch)?;
    let pairing = !server_pinned || !compute_granted;
    let peer = Peer {
        name,
        id: ch.remote.clone(),
        address: addr,
    };
    ch.send_msg(
        &Msg::Offer {
            name: state.identity.name(),
            compute_granted,
        },
        Some(&cancelled),
    )?;
    let allowed = if pairing {
        let permit = Permit::take(&state.approval_busy)?;
        let approval = Approval {
            peer: peer.clone(),
            code: ch.code.clone(),
        };
        // The client waits for our Decision before sending another frame.
        // A nonblocking peek lets an idle approval notice EOF without
        // consuming any bytes from the encrypted transport.
        ch.io.stream.set_nonblocking(true).map_err(err)?;
        let active = ApprovalActive {
            pending: Arc::new(AtomicBool::new(true)),
            server: Arc::downgrade(state),
            epoch,
        };
        let status = active.clone();
        let f = state.approve.clone();
        let rx = job(permit, move || f(approval, status))?;
        let result = wait(rx, wire::APPROVAL, &|| {
            cancelled()
                || match ch.io.stream.peek(&mut [0; 1]) {
                    // EOF or unsolicited data invalidates this pending exchange.
                    Ok(_) => true,
                    Err(e) => !matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ),
                }
        });
        active.pending.store(false, Ordering::Release);
        ch.io.stream.set_nonblocking(false).map_err(err)?;
        result?
    } else {
        true
    };
    ch.send_msg(&Msg::Decision { allowed }, Some(&cancelled))?;
    if !allowed {
        return Err("host denied pairing".into());
    }
    ch.io.idle = wire::APPROVAL;
    if !matches!(
        ch.recv_msg(Some(&cancelled))?,
        Msg::Decision { allowed: true }
    ) {
        return Err("client denied pairing".into());
    }
    {
        // Serialize trust commit with disable/revoke, so a stale dialog cannot regrant trust.
        let mut p = state.policy.lock().map_err(err)?;
        if !p.enabled || p.epoch != epoch || state.stop.load(Ordering::Acquire) {
            return Err("server unavailable or pairing revoked".into());
        }
        state
            .identity
            .confirm(Role::ComputeGrant, &ch.remote, trust_epoch, pairing)?;
        p.remember(peer, Instant::now());
    }
    ch.send_msg(&Msg::Ready, Some(&cancelled))?;
    ch.io.idle = wire::IDLE;
    let (samples, options) = match ch.recv_msg(Some(&cancelled))? {
        Msg::Request { samples, options } if samples > 0 && samples <= wire::SAMPLES => {
            (samples, options.core()?)
        }
        _ => return Err("invalid request or sample count".into()),
    };
    if !state
        .identity
        .authorized(Role::ComputeGrant, &ch.remote, trust_epoch)?
    {
        return Err("device permission revoked".into());
    }
    // Wait only for final requests, without retaining or allocating any audio.
    let permit = reserve_compute(
        state,
        options.beam_size > 1,
        ch.io.until,
        &cancelled,
        &ch.remote,
        trust_epoch,
    )?;
    ch.send_msg(&Msg::Accepted, Some(&cancelled))?;
    let mut audio = Vec::with_capacity(samples);
    while audio.len() < samples {
        let bytes = ch.recv(wire::CHUNK, Some(&cancelled))?;
        if bytes.is_empty() || bytes.len() % 4 != 0 || bytes.len() / 4 > samples - audio.len() {
            return Err("invalid audio chunk".into());
        }
        for b in bytes.chunks_exact(4) {
            let x = f32::from_le_bytes(b.try_into().map_err(err)?);
            if !x.is_finite() {
                return Err("non-finite audio sample".into());
            }
            audio.push(x);
        }
    }
    if cancelled() {
        return Err("server unavailable or permission revoked".into());
    }
    let compute = state.compute.clone();
    let rx = job(permit, move || compute(&audio, &options))?;
    let remaining = ch
        .io
        .until
        .saturating_duration_since(Instant::now())
        .min(wire::COMPUTE);
    let transcript = wait(rx, remaining, &cancelled)??;
    if cancelled()
        || !state
            .identity
            .authorized(Role::ComputeGrant, &ch.remote, trust_epoch)?
    {
        return Err("server unavailable or permission revoked".into());
    }
    let bytes = wire::encode_result(transcript)?;
    ch.send_msg(&Msg::Reply { bytes: bytes.len() }, Some(&cancelled))?;
    for chunk in bytes.chunks(wire::CHUNK) {
        ch.send(chunk, Some(&cancelled))?;
    }
    Ok(())
}

fn reserve_compute(
    state: &State,
    final_request: bool,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
    remote: &str,
    trust_epoch: u64,
) -> Result<Permit, String> {
    let _waiter = final_request.then(|| WaitingFinal::new(&state.waiting_finals));
    let until = deadline.min(Instant::now() + wire::FINAL_WAIT);
    loop {
        if cancelled() {
            return Err("request cancelled or server unavailable".into());
        }
        if !final_request && state.waiting_finals.load(Ordering::Acquire) > 0 {
            return Err("server busy: final request waiting".into());
        }
        if !state
            .identity
            .authorized(Role::ComputeGrant, remote, trust_epoch)?
        {
            return Err("device permission revoked".into());
        }
        match Permit::take(&state.compute_busy) {
            Ok(p) => {
                // A final may have registered between the first check and CAS.
                if !final_request && state.waiting_finals.load(Ordering::Acquire) > 0 {
                    drop(p);
                    return Err("server busy: final request waiting".into());
                }
                return Ok(p);
            }
            Err(e) if !final_request => return Err(e),
            Err(_) if Instant::now() >= until => {
                return Err("server busy: final scheduling deadline exceeded".into())
            }
            Err(_) => thread::sleep(Duration::from_millis(25)),
        }
    }
}

struct WaitingFinal<'a>(&'a AtomicUsize);
impl<'a> WaitingFinal<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}
impl Drop for WaitingFinal<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;

    #[test]
    fn only_peers_with_recent_authenticated_activity_remain_visible() {
        let start = Instant::now();
        let mut policy = Policy {
            enabled: true,
            epoch: 0,
            clients: Vec::new(),
        };
        let a = Peer {
            id: "a".repeat(64),
            name: "A".into(),
            address: "127.0.0.1:2345".parse().unwrap(),
        };
        let b = Peer {
            id: "b".repeat(64),
            name: "B".into(),
            ..a.clone()
        };
        policy.remember(a.clone(), start);
        policy.remember(b.clone(), start);
        policy.remember(b.clone(), start + Duration::from_secs(5));
        assert_eq!(
            policy.clients_at(start + CLIENT_TIMEOUT - Duration::from_millis(1)),
            vec![a, b.clone()]
        );
        assert_eq!(policy.clients_at(start + CLIENT_TIMEOUT), vec![b]);
        assert!(policy
            .clients_at(start + Duration::from_secs(5) + CLIENT_TIMEOUT)
            .is_empty());
    }

    #[test]
    fn local_revoke_aborts_a_final_waiting_for_acceptance() {
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        let hi = Identity::load_or_create(host_dir.path()).unwrap();
        let ci = Identity::load_or_create(client_dir.path()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let server = Server::start(
            hi.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(|_| true),
            Arc::new(move |_, _| {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(Transcript::default())
            }),
        )
        .unwrap();
        let peer = Peer {
            name: hi.name(),
            id: hi.id(),
            address: server.local_addr(),
        };
        let client = Client::connect(ci.clone(), &peer, Arc::new(|_| true)).unwrap();
        let preview = client.clone();
        let preview_task =
            thread::spawn(move || preview.transcribe(&[0.1], &TranscribeOptions::fast(), None));
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let final_task = thread::spawn(move || {
            result_tx
                .send(client.transcribe(&[0.1], &TranscribeOptions::accurate(), None))
                .unwrap();
        });
        let until = Instant::now() + Duration::from_secs(3);
        while server.state.waiting_finals.load(Ordering::Acquire) == 0 && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        let waiting = server.state.waiting_finals.load(Ordering::Acquire) > 0;
        ci.clear_trust().unwrap();
        let result = result_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        let _ = preview_task.join();
        final_task.join().unwrap();
        assert!(
            waiting,
            "test must revoke while the final awaits acceptance"
        );
        assert!(
            matches!(result, Ok(Err(_))),
            "local revoke must interrupt acceptance wait"
        );
    }

    #[test]
    fn waiting_final_reserves_priority_over_new_network_previews() {
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        let hi = Identity::load_or_create(host_dir.path()).unwrap();
        let ci = Identity::load_or_create(client_dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let server = Server::start(
            hi.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(|_| true),
            Arc::new(move |_, _| {
                n.fetch_add(1, Ordering::SeqCst);
                Ok(Transcript::default())
            }),
        )
        .unwrap();
        let p = Peer {
            name: hi.name(),
            id: hi.id(),
            address: server.local_addr(),
        };
        let client = Client::connect(ci, &p, Arc::new(|_| true)).unwrap();
        // Register the scheduling condition without relying on a poll-loop race.
        // The compute permit is deliberately free: this request must still be busy.
        let waiter = WaitingFinal::new(&server.state.waiting_finals);
        let error = client
            .transcribe(&[0.1], &TranscribeOptions::fast(), None)
            .unwrap_err();
        assert!(error.contains("server busy"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(waiter);
        client
            .transcribe(&[0.1], &TranscribeOptions::fast(), None)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
