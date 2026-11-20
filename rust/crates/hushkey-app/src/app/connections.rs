use super::*;
use hushkey_net::{ApprovalActive, Peer, Server};

const HEARTBEAT: Duration = Duration::from_secs(4);
const REDISCOVER: Duration = Duration::from_secs(20);

pub(super) struct Outgoing {
    pub pairing: Pairing,
    pub window_session: u64,
}

pub(super) struct Incoming {
    pub window_session: u64,
    pub pairing: Pairing,
    pub active: ApprovalActive,
    pub reply: mpsc::SyncSender<bool>,
    pub expires: Instant,
    pub focus: PairingFocus,
}

pub(super) enum PairingFocus {
    AwaitingWindow,
    Finished,
}

impl PairingFocus {
    fn poll(&mut self, idle: bool, focus_now: impl FnOnce() -> bool) {
        if matches!(self, Self::AwaitingWindow) && (!idle || focus_now()) {
            *self = Self::Finished;
        }
    }
}

impl Incoming {
    pub fn live(&self) -> bool {
        self.active.is_active() && Instant::now() < self.expires
    }
}

pub(super) struct Connections {
    pub peers: Vec<Peer>,
    pub search_status: String,
    pub quiet_search: bool,
    pub client_status: String,
    pub pairing_error: Option<(Peer, String)>,
    pub host_error: Option<String>,
    pub incoming: Option<Incoming>,
    pub paused: bool,
    pub checking: bool,
    pub next_check: Instant,
    next_discovery: Instant,
    next_host_check: Instant,
    pub link: Link,
}

impl Connections {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            peers: Vec::new(),
            search_status: String::new(),
            quiet_search: false,
            client_status: String::new(),
            pairing_error: None,
            host_error: None,
            incoming: None,
            paused: false,
            checking: false,
            next_check: now,
            next_discovery: now,
            next_host_check: now + HEARTBEAT,
            link: Link::Local,
        }
    }

    pub fn can_select(&self, peer: &Peer, saved: Option<&Peer>) -> bool {
        // A failed attempt stays available for an explicit retry when discovery
        // stops listing it. A newer listing always wins over the remembered address.
        if let Some(listed) = crate::settings::selected_peer(&peer.id, &self.peers, None) {
            return listed == peer;
        }
        saved == Some(peer)
            || self
                .pairing_error
                .as_ref()
                .is_some_and(|(failed, _)| failed == peer)
    }

    pub fn pairing_failed(&mut self, peer: Peer, reason: String) -> String {
        let message = format!("{} · {reason}", peer.name);
        self.pairing_error = Some((peer, reason));
        self.next_check = Instant::now();
        message
    }

    fn answer_pairing(&mut self, id: u64, allow: bool) {
        if let Some(incoming) = self.incoming.take_if(|p| p.pairing.id == id) {
            let approved = allow && incoming.live();
            let _ = incoming.reply.send(approved);
        }
    }
}

impl App {
    pub(super) fn receive_pairing(
        &mut self,
        id: u64,
        request: hushkey_net::Approval,
        active: ApprovalActive,
        reply: mpsc::SyncSender<bool>,
        expires: Instant,
    ) {
        // The worker may have ended or a different host session may have started
        // while the event was waiting in the tray thread's queue.
        if self
            .connections
            .incoming
            .as_ref()
            .is_some_and(|p| !p.live())
        {
            self.deny_incoming();
        }
        if !self.config.server_enabled
            || self.revoke_dialog.is_some()
            || !active.is_active()
            || Instant::now() >= expires
            || self.connections.incoming.is_some()
        {
            let _ = reply.send(false);
            return;
        }
        let focus = if self.pairing_can_focus() {
            PairingFocus::AwaitingWindow
        } else {
            PairingFocus::Finished
        };
        let window_session = self.settings.show_incoming();
        self.connections.incoming = Some(Incoming {
            pairing: Pairing {
                id,
                peer: request.peer,
                code: request.code,
            },
            window_session,
            active,
            reply,
            expires,
            focus,
        });
    }

    pub(super) fn decide_pairing(&mut self, id: u64, allow: bool) {
        let hosting = self.config.server_enabled
            && self.revoke_dialog.is_none()
            && self
                .network
                .as_ref()
                .and_then(|n| n.server.as_ref())
                .is_some_and(Server::alive);
        self.connections.answer_pairing(id, allow && hosting);
    }

    pub(super) fn deny_incoming(&mut self) {
        if let Some(incoming) = self.connections.incoming.take() {
            let _ = incoming.reply.send(false);
        }
    }

    pub(super) fn cancel_pairing(&mut self, id: u64) {
        if self
            .connecting
            .as_ref()
            .is_none_or(|target| target.pairing.id != id)
        {
            return;
        }
        if let Some(network) = &mut self.network {
            network.cancel_connect();
        }
        self.connect_generation += 1;
        self.connecting = None;
        self.connections.next_check = Instant::now();
        self.tray.recovery_available(self.has_failed());
        self.idle();
    }

    pub(super) fn close_session_pairings(&mut self, session: u64) {
        if self
            .connections
            .incoming
            .as_ref()
            .is_some_and(|p| p.window_session == session)
        {
            self.deny_incoming();
        }
        if let Some(target) = self
            .connecting
            .as_ref()
            .filter(|p| p.window_session == session)
        {
            self.cancel_pairing(target.pairing.id);
        }
    }

    pub(super) fn dismiss_pairings(&mut self) {
        self.deny_incoming();
        if let Some(target) = &self.connecting {
            self.cancel_pairing(target.pairing.id);
        }
    }

    fn pairing_can_focus(&self) -> bool {
        self.config.mode != Mode::Listen
            && self.listening.is_none()
            && self.draining.is_none()
            && self.pressed_at.is_none()
            && platform::since_last_seen().is_none()
            && self.recording.is_none()
            && !self.busy
            && self.pending_inserts == 0
    }

    fn poll_pairing_focus(&mut self) {
        if self.connections.incoming.is_none() {
            return;
        }
        let idle = self.pairing_can_focus();
        if let Some(pending) = &mut self.connections.incoming {
            if pending.live() {
                pending.focus.poll(idle, || {
                    self.settings.window.focus_incoming_now(pending.pairing.id)
                });
            } else {
                self.deny_incoming();
            }
        }
    }

    pub(super) fn find_servers(&mut self, quiet: bool) {
        if self.discovering {
            return;
        }
        let text = network::words(
            self.s,
            "Suche Hush-Key-Rechner im LAN …",
            "Searching for Hush-Key computers on the LAN …",
        );
        if let Some(network) = &self.network {
            self.discovering = true;
            self.connections.quiet_search = quiet;
            self.connections.search_status = text.into();
            self.tray.network_searching(true);
            network.discover();
            if !quiet {
                self.notice(GREY, text);
            }
        } else {
            self.connections.search_status = network::words(
                self.s,
                "Netzwerk nicht verfügbar. Details stehen im Log.",
                "Network unavailable. See the log for details.",
            )
            .into();
            if !quiet {
                self.failure_notice(&self.connections.search_status.clone());
            }
        }
    }

    pub(super) fn begin_check(&mut self) {
        if self.connections.checking
            || self.connections.paused
            || self.connecting.is_some()
            || self.revoke_dialog.is_some()
        {
            return;
        }
        let (Some(net), Some(peer)) = (&self.network, self.config.remote.clone()) else {
            return;
        };
        let now = Instant::now();
        let rediscover = now >= self.connections.next_discovery;
        if rediscover {
            self.connections.next_discovery = now + REDISCOVER;
        }
        if self.connections.link == Link::Local {
            self.connections.link = Link::Waiting;
        }
        self.connections.checking = true;
        net.check(peer, self.connect_generation, rediscover);
    }

    pub(super) fn end_check(&mut self, peer: Peer, result: Result<network::CheckedPeer, String>) {
        let now = Instant::now();
        self.connections.checking = false;
        self.connections.next_check = now + HEARTBEAT;
        let words = |de, en| network::words(self.s, de, en);
        match result {
            Ok((rtt, backend)) => {
                let moved = self
                    .config
                    .remote
                    .as_ref()
                    .is_some_and(|p| p.address != peer.address);
                if moved {
                    self.config.remote = Some(peer.clone());
                    self.save(|c| c.remote = Some(peer));
                }
                let first = self.backend.is_none();
                if let Some(current) = &self.backend {
                    if moved {
                        // Recording and retry handles keep their retained audio.
                        current.reconnect(&backend);
                    }
                } else {
                    self.backend = Some(backend);
                }
                self.connections.client_status = match rtt {
                    Some(time) => format!(
                        "{} · {} ms",
                        words("Datentest bestätigt", "Data test confirmed"),
                        if time.as_millis() == 0 {
                            "<1".into()
                        } else {
                            time.as_millis().to_string()
                        }
                    ),
                    None => words(
                        "Host erreichbar · für den Datentest Hush-Key auf dem Host aktualisieren",
                        "Host reachable · update Hush-Key on the host to enable the data test",
                    )
                    .into(),
                };
                let recovered =
                    !matches!(self.connections.link, Link::Online { .. } | Link::Legacy);
                self.connections.link = match rtt {
                    Some(elapsed) => Link::Online {
                        rtt_ms: elapsed.as_millis() as u64,
                    },
                    None => Link::Legacy,
                };
                if (first || recovered) && self.recording.is_none() && !self.busy {
                    self.idle();
                }
            }
            Err(e) => {
                self.connections.link = Link::Offline;
                self.connections.client_status = format!(
                    "{}\n{e}",
                    words(
                        "Keine bestätigte Verbindung · neuer Versuch folgt automatisch",
                        "Connection unconfirmed · retrying automatically"
                    )
                );
                if self.recording.is_none() && !self.busy {
                    self.tray
                        .show(Badge::Failed, &self.connections.client_status);
                }
            }
        }
    }

    pub(super) fn poll_connections(&mut self) {
        self.poll_pairing_focus();
        let now = Instant::now();
        if self.config.remote.is_some() && now >= self.connections.next_check {
            self.begin_check();
        }
        if self.config.server_enabled && now >= self.connections.next_host_check {
            self.connections.next_host_check = now + HEARTBEAT;
            if let Some(network) = &mut self.network {
                // An accept/discovery loop that died or a temporary bind failure.
                if !network.server.as_ref().is_some_and(Server::alive) {
                    self.connections.host_error = network.set_enabled(true).err();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushkey_net::{Client, Identity};

    #[test]
    fn dictation_suppresses_unresolved_pairing_focus_even_after_the_key_is_released() {
        let mut focus = PairingFocus::AwaitingWindow;
        focus.poll(true, || false); // The passive child is still starting.
        focus.poll(false, || panic!("capture must keep focus"));
        focus.poll(true, || {
            panic!("late window readiness must not activate it")
        });

        let mut next_request = PairingFocus::AwaitingWindow;
        next_request.poll(true, || true);
        next_request.poll(true, || {
            panic!("a ready request only gets one foreground attempt")
        });
    }

    #[test]
    fn refused_pairing_keeps_the_active_route_and_only_retries_the_selected_device() {
        let failed = Peer {
            name: "Host B".into(),
            id: "b".repeat(64),
            address: "127.0.0.1:47000".parse().unwrap(),
        };
        let saved = Peer {
            name: "Host A".into(),
            id: "a".repeat(64),
            ..failed.clone()
        };
        let moved = Peer {
            address: "127.0.0.2:47000".parse().unwrap(),
            ..failed.clone()
        };
        for (link, selected) in [
            (Link::Local, None),
            (Link::Online { rtt_ms: 2 }, Some(&saved)),
        ] {
            let mut connections = Connections::new();
            connections.link = link;
            connections.client_status = "route status".into();
            let notice = connections.pairing_failed(failed.clone(), "declined".into());
            assert_eq!(notice, "Host B · declined");
            assert_eq!(connections.link, link);
            assert_eq!(connections.client_status, "route status");
            assert!(connections.can_select(&failed, selected));
            assert!(!connections.can_select(&moved, selected));
            connections.peers.push(moved.clone());
            assert!(!connections.can_select(&failed, selected));
            assert!(connections.can_select(&moved, selected));
        }
    }

    #[test]
    fn stale_click_keeps_the_current_request_and_expiry_denies_a_late_accept() {
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        let host = Identity::load_or_create(host_dir.path()).unwrap();
        let client = Identity::load_or_create(client_dir.path()).unwrap();
        let (events, requests) = mpsc::channel();
        let server = Server::start_with_approval_status(
            host.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(move |request, active| {
                let (reply, answer) = mpsc::sync_channel(1);
                events
                    .send(Incoming {
                        window_session: 1,
                        pairing: Pairing {
                            id: 7,
                            peer: request.peer,
                            code: request.code,
                        },
                        active,
                        reply,
                        expires: Instant::now() + Duration::from_secs(5),
                        focus: PairingFocus::AwaitingWindow,
                    })
                    .unwrap();
                answer.recv_timeout(Duration::from_secs(5)).unwrap_or(false)
            }),
            Arc::new(|_, _| panic!("pairing must not run inference")),
        )
        .unwrap();
        let peer = Peer {
            id: host.id(),
            name: host.name(),
            address: server.local_addr(),
        };
        let attempt =
            std::thread::spawn(move || Client::connect(client, &peer, Arc::new(|_| true)));
        let mut connections = Connections::new();
        connections.incoming = Some(requests.recv_timeout(Duration::from_secs(5)).unwrap());

        connections.answer_pairing(6, true);
        let current = connections
            .incoming
            .as_mut()
            .expect("stale click consumed the active request");
        assert!(current.live());
        current.expires = Instant::now();
        connections.answer_pairing(7, true);
        assert!(connections.incoming.is_none());
        assert!(attempt.join().unwrap().is_err());
    }
}
