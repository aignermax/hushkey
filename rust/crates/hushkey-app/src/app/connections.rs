use super::*;
use hushkey_net::{Peer, Server};

const HEARTBEAT: Duration = Duration::from_secs(4);
const REDISCOVER: Duration = Duration::from_secs(20);

pub(super) struct Connections {
    pub peers: Vec<Peer>,
    pub search_status: String,
    pub quiet_search: bool,
    pub client_status: String,
    pub host_error: Option<String>,
    pub request: Option<Peer>,
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
            host_error: None,
            request: None,
            paused: false,
            checking: false,
            next_check: now,
            next_discovery: now,
            next_host_check: now + HEARTBEAT,
            link: Link::Local,
        }
    }
}

impl App {
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
        if self.connections.checking || self.connections.paused || self.connecting || self.revoking
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
