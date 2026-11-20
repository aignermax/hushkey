//! Desktop ownership of LAN computation. No network worker touches native UI.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

use hushkey_core::{Engine, Error, TranscribeOptions, Transcriber, Transcript};
use hushkey_net::{Approval, ApprovalActive, Client, Identity, Peer, PendingClient, Server};

use crate::{config, strings};

/// The selected transcription destination, from the client's point of view.
/// Only a reply to an authenticated probe can establish measured reachability.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) enum Link {
    Local,
    Connecting,
    Waiting,
    Online { rtt_ms: u64 },
    Legacy,
    Offline,
    Revoked,
}

pub enum Event {
    Checked {
        generation: u64,
        peer: Peer,
        result: Result<CheckedPeer, String>,
    },
    Approval {
        id: u64,
        active: ApprovalActive,
        request: Approval,
        reply: mpsc::SyncSender<bool>,
        expires: Instant,
    },
    ApprovalEnded {
        id: u64,
    },
    PairingCode {
        generation: u64,
        request: Approval,
    },
    Discovered(std::result::Result<Vec<Peer>, String>),
    Connected {
        generation: u64,
        result: std::result::Result<(Peer, PendingConnection), String>,
    },
}

pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;

pub struct Network {
    pub identity: Identity,
    pub server: Option<Server>,
    pub server_error: Option<String>,
    pub engine: Arc<Mutex<Option<Arc<Engine>>>>,
    notify: Notify,
    bind: std::net::SocketAddr,
    outbound_cancel: Option<Arc<AtomicBool>>,
}

impl Network {
    pub fn start(enabled: bool, notify: Notify) -> std::result::Result<Self, String> {
        let identity = Identity::load_or_create(&config::state_dir().join("network"))?;
        Ok(Self::with_identity(
            identity,
            enabled,
            ([0, 0, 0, 0], hushkey_net::PORT).into(),
            notify,
        ))
    }

    fn with_identity(
        identity: Identity,
        enabled: bool,
        bind: std::net::SocketAddr,
        notify: Notify,
    ) -> Self {
        let engine: Arc<Mutex<Option<Arc<Engine>>>> = Arc::new(Mutex::new(None));
        let mut network = Self {
            identity,
            server: None,
            server_error: None,
            engine,
            notify,
            bind,
            outbound_cancel: None,
        };
        if enabled {
            let _ = network.set_enabled(true);
        }
        network
    }

    pub fn set_enabled(&mut self, enabled: bool) -> std::result::Result<(), String> {
        if !enabled {
            self.server.take();
            self.server_error = None;
            return Ok(());
        }
        if self.server.as_ref().is_some_and(Server::alive) {
            return Ok(());
        }
        self.server.take();
        let host_engine = self.engine.clone();
        let server = Server::start_with_approval_status(
            self.identity.clone(),
            self.bind,
            host_approval(self.notify.clone()),
            Arc::new(move |audio, options| {
                let local = host_engine.lock().map_err(|_| "local engine unavailable")?.clone()
                    .ok_or("The server's local model is not ready. Select local computation and wait for it to load.")?;
                local.transcribe(audio, options).map_err(|e| e.to_string())
            }),
        );
        match server {
            Ok(server) => {
                self.server = Some(server);
                self.server_error = None;
                Ok(())
            }
            Err(e) => {
                self.server_error = Some(e.clone());
                Err(e)
            }
        }
    }

    pub fn revoke_all(&self) -> std::result::Result<(), String> {
        if let Some(cancelled) = &self.outbound_cancel {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(server) = &self.server {
            server.revoke_all()
        } else {
            self.identity.clear_trust()
        }
    }

    pub fn discover(&self) {
        let (identity, notify) = (self.identity.clone(), self.notify.clone());
        std::thread::spawn(move || {
            notify(Event::Discovered(hushkey_net::discover(
                &identity,
                Duration::from_secs(3),
            )))
        });
    }

    pub fn connect(&mut self, peer: Peer, generation: u64) {
        self.cancel_connect();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.outbound_cancel = Some(cancelled.clone());
        let (identity, notify) = (self.identity.clone(), self.notify.clone());
        std::thread::spawn(move || {
            let report = notify.clone();
            let stale = cancelled.clone();
            let show_code = Arc::new(move |request| {
                if stale.load(Ordering::Acquire) {
                    return false;
                }
                report(Event::PairingCode {
                    generation,
                    request,
                });
                true
            });
            let result = Client::prepare_connect(identity, &peer, show_code, &|| {
                cancelled.load(Ordering::Acquire)
            })
            .map(|client| (peer, PendingConnection { client, cancelled }));
            notify(Event::Connected { generation, result });
        });
    }

    pub fn cancel_connect(&mut self) {
        if let Some(flag) = self.outbound_cancel.take() {
            flag.store(true, Ordering::Release);
        }
    }

    /// Recover only the saved device. Discovery provides an address hint;
    /// the probe checks the pinned key before the app adopts a new address.
    pub fn check(&self, peer: Peer, generation: u64, rediscover: bool) {
        let (identity, notify) = (self.identity.clone(), self.notify.clone());
        std::thread::spawn(move || {
            let mut peer = peer;
            let mut result = check_peer(&identity, &peer);
            if result.is_err() && rediscover {
                if let Ok(peers) = hushkey_net::discover(&identity, Duration::from_secs(2)) {
                    if let Some(moved) = peers
                        .into_iter()
                        .find(|p| p.id == peer.id && p.address != peer.address)
                    {
                        let candidate = Peer {
                            address: moved.address,
                            ..peer.clone()
                        };
                        if let Ok(checked) = check_peer(&identity, &candidate) {
                            result = Ok(checked);
                            peer = candidate;
                        }
                    }
                }
            }
            notify(Event::Checked {
                generation,
                peer,
                result,
            });
        });
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.cancel_connect();
    }
}

pub struct PendingConnection {
    client: PendingClient,
    cancelled: Arc<AtomicBool>,
}

impl PendingConnection {
    /// Called on the UI thread after matching the generation and selected peer.
    pub fn finish(self) -> Result<Arc<Recoverable>, String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("connection cancelled".into());
        }
        let client = self.client.finish()?;
        Ok(Arc::new(Recoverable::new(Arc::new(Remote(client)))))
    }
}

pub type CheckedPeer = (Option<Duration>, Arc<Recoverable>);

fn check_peer(identity: &Identity, peer: &Peer) -> Result<CheckedPeer, String> {
    let client = Client::resume(identity.clone(), peer)?;
    let rtt = match client.probe() {
        Ok(elapsed) => Some(elapsed),
        Err(e) if e == hushkey_net::PROBE_UNSUPPORTED => None,
        Err(e) => return Err(e),
    };
    Ok((rtt, Arc::new(Recoverable::new(Arc::new(Remote(client))))))
}

fn host_approval(notify: Notify) -> hushkey_net::HostApprove {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    Arc::new(move |request, active| {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = mpsc::sync_channel(1);
        let expires = Instant::now() + Duration::from_secs(85);
        notify(Event::Approval {
            id,
            active: active.clone(),
            request,
            reply,
            expires,
        });
        let allowed = loop {
            if !active.is_active() || Instant::now() >= expires {
                break false;
            }
            match answer.recv_timeout(Duration::from_millis(50)) {
                Ok(allowed) => break allowed && active.is_active() && Instant::now() < expires,
                Err(mpsc::RecvTimeoutError::Disconnected) => break false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        drop(answer);
        notify(Event::ApprovalEnded { id });
        allowed
    })
}

struct Remote(Client);
impl Transcriber for Remote {
    fn transcribe_cancellable(
        &self,
        audio: &[f32],
        options: &TranscribeOptions,
        abort: Option<&dyn Fn() -> bool>,
    ) -> hushkey_core::Result<Transcript> {
        self.0.transcribe(audio, options, abort).map_err(|e| {
            if abort.is_some_and(|f| f()) {
                Error::Cancelled
            } else {
                Error::Transcribe(e)
            }
        })
    }
}

/// Keep the final request until GUI acceptance, then retain failures only.
pub struct Recoverable {
    backend: Mutex<Arc<dyn Transcriber>>,
    retained: Mutex<Option<FinalRequest>>,
}

#[derive(Clone)]
struct FinalRequest {
    audio: Vec<f32>,
    options: TranscribeOptions,
    failed: bool,
}
impl Recoverable {
    pub fn new(backend: Arc<dyn Transcriber>) -> Self {
        Self {
            backend: Mutex::new(backend),
            retained: Mutex::new(None),
        }
    }

    pub fn has_failed(&self) -> bool {
        self.retained
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|request| request.failed)
    }

    pub fn discard(&self) {
        self.retained
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }

    pub fn acknowledge(&self) {
        self.retained
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }

    pub fn reject_result(&self) {
        if let Some(request) = self
            .retained
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            request.failed = true;
        }
    }

    /// Re-pair the same pinned destination without losing retained audio.
    pub fn reconnect(&self, connection: &Recoverable) {
        let backend = connection
            .backend
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        *self.backend.lock().unwrap_or_else(|p| p.into_inner()) = backend;
    }

    pub fn retry(&self) -> hushkey_core::Result<Transcript> {
        let request = self
            .retained
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|request| request.failed)
            .ok_or_else(|| Error::Transcribe("no failed recording to retry".into()))?;
        self.transcribe(&request.audio, &request.options)
    }
}
impl Transcriber for Recoverable {
    fn transcribe_cancellable(
        &self,
        audio: &[f32],
        options: &TranscribeOptions,
        abort: Option<&dyn Fn() -> bool>,
    ) -> hushkey_core::Result<Transcript> {
        let backend = self
            .backend
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let result = backend.transcribe_cancellable(audio, options, abort);
        if options.beam_size > 1 {
            *self.retained.lock().unwrap_or_else(|p| p.into_inner()) = Some(FinalRequest {
                audio: audio.to_vec(),
                options: options.clone(),
                failed: result.is_err(),
            });
        }
        result
    }
}

/// Keep the destination visible even when platform overlays retain only the
/// final 500 characters. Reserve space, then take the end of the preview.
pub fn overlay_text(peer: Option<&Peer>, text: &str) -> String {
    let Some(peer) = peer else {
        return text.to_string();
    };
    let prefix = format!("Remote · {} — ", peer.name);
    let limit = 450usize.saturating_sub(prefix.chars().count());
    let tail: String = text
        .chars()
        .skip(text.chars().count().saturating_sub(limit))
        .collect();
    prefix + &tail
}

/// A queued remote result cannot outlive the permission generation it used.
pub fn current_result(remote_epoch: Option<u64>, current: u64) -> bool {
    remote_epoch.is_none_or(|epoch| epoch == current)
}

pub fn german(s: &strings::Strings) -> bool {
    std::ptr::eq(s, &strings::DE)
}

pub fn words<'a>(s: &strings::Strings, de: &'a str, en: &'a str) -> &'a str {
    if german(s) {
        de
    } else {
        en
    }
}

/// The normal application-scoped Windows firewall prompt can be dismissed
/// accidentally. This explicit menu action installs private-LAN-only rules
/// via UAC, with executable paths passed as argv data to a fixed script.
#[cfg(windows)]
pub fn allow_firewall() -> std::result::Result<(), String> {
    use std::os::windows::process::CommandExt;
    const SCRIPT: &str = r#"param([string]$Exe)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw 'Hush-Key executable not found' }
foreach ($Protocol in @('TCP', 'UDP')) {
    $Name = 'HushKey-LAN-' + $Protocol
    Get-NetFirewallRule -Name $Name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -Name $Name -DisplayName ('Hush-Key LAN ' + $Protocol) -Direction Inbound -Action Allow -Program $Exe -Protocol $Protocol -LocalPort 47957 -Profile Private -RemoteAddress LocalSubnet | Out-Null
}
"#;
    let dir = config::state_dir();
    let script = dir.join("allow-lan-firewall.ps1");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(&script, SCRIPT).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Start-Process ArgumentList needs Windows command-line quoting. Values
    // arrive through $args, never as PowerShell program text.
    const ELEVATE: &str = r#"param([string]$Script, [string]$Exe)
$ErrorActionPreference='Stop'
$a=@('-NoProfile','-ExecutionPolicy','Bypass','-File',('"'+$Script+'"'),'-Exe',('"'+$Exe+'"'))
$p=Start-Process -FilePath powershell.exe -ArgumentList $a -Verb RunAs -WindowStyle Hidden -PassThru -Wait
exit $p.ExitCode
"#;
    let launcher = dir.join("request-lan-firewall.ps1");
    std::fs::write(&launcher, ELEVATE).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(launcher)
        .arg("-Script")
        .arg(script)
        .arg("-Exe")
        .arg(exe)
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Firewall permission was cancelled or could not be changed.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_network(enabled: bool) -> (tempfile::TempDir, Network, mpsc::Receiver<Event>) {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let network = Network::with_identity(
            Identity::load_or_create(dir.path()).unwrap(),
            enabled,
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        );
        (dir, network, rx)
    }

    fn event(rx: &mpsc::Receiver<Event>) -> Event {
        rx.recv_timeout(Duration::from_secs(3)).unwrap()
    }

    #[test]
    fn inline_pairing_cancels_denies_and_repairs_only_on_connect() {
        let (_host_dir, host, host_events) = test_network(true);
        let (_client_dir, mut client, client_events) = test_network(false);
        let peer = Peer {
            id: host.identity.id(),
            name: host.identity.name(),
            address: host.server.as_ref().unwrap().local_addr(),
        };
        for generation in 1..=5 {
            client.connect(peer.clone(), generation);
            let Event::PairingCode {
                generation: seen,
                request: sas,
            } = event(&client_events)
            else {
                panic!("client should show the code inline")
            };
            assert_eq!(seen, generation);
            assert_eq!(sas.peer.id, peer.id);
            let Event::Approval {
                id,
                active,
                request,
                reply,
                ..
            } = event(&host_events)
            else {
                panic!("host must receive an explicit approval")
            };
            assert_eq!(request.peer.id, client.identity.id());
            assert_eq!(request.code, sas.code);
            assert!(active.is_active());
            if generation == 1 {
                client.cancel_connect();
            } else {
                reply.send(generation != 2).unwrap();
            }
            let Event::ApprovalEnded { id: ended } = event(&host_events) else {
                panic!("host request should close")
            };
            assert_eq!(id, ended);
            let Event::Connected {
                generation: finished,
                result,
            } = event(&client_events)
            else {
                panic!("expected connection result")
            };
            assert_eq!(finished, generation);
            match generation {
                1 | 2 => {
                    if generation == 1 {
                        assert!(!active.is_active());
                        assert!(reply.send(true).is_err(), "late approval accepted");
                    }
                    assert!(result.is_err());
                    assert!(host.server.as_ref().unwrap().clients().is_empty());
                    assert!(Client::resume(client.identity.clone(), &peer).is_err());
                }
                3 => {
                    // Even a completed network callback queued on the UI thread
                    // cannot persist a pin once the selection has changed.
                    client.cancel_connect();
                    assert!(result.unwrap().1.finish().is_err());
                    assert!(Client::resume(client.identity.clone(), &peer).is_err());
                }
                4 => {
                    result.unwrap().1.finish().unwrap();
                    let saved = Client::resume(client.identity.clone(), &peer).unwrap();
                    saved.probe().unwrap();
                    host.revoke_all().unwrap();
                    assert!(saved.probe().is_err());
                    assert!(host_events.try_recv().is_err());
                }
                _ => {
                    result.unwrap().1.finish().unwrap();
                    Client::resume(client.identity.clone(), &peer)
                        .unwrap()
                        .probe()
                        .unwrap();
                }
            }
        }
    }

    #[test]
    fn saved_destination_recovers_after_host_restart_without_approval_or_audio() {
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        let host = Identity::load_or_create(host_dir.path()).unwrap();
        let client = Identity::load_or_create(client_dir.path()).unwrap();
        let start = |address, allow| {
            Server::start(
                host.clone(),
                address,
                Arc::new(move |_| allow),
                Arc::new(|_, _| panic!("a connection check must never submit audio")),
            )
            .unwrap()
        };
        let server = start("127.0.0.1:0".parse().unwrap(), true);
        let peer = Peer {
            name: host.name(),
            id: host.id(),
            address: server.local_addr(),
        };
        Client::connect(client.clone(), &peer, Arc::new(|_| true)).unwrap();
        drop(server);
        assert!(check_peer(&client, &peer).is_err());
        let _server = start(peer.address, false);
        let (rtt, _) = check_peer(&client, &peer).expect("recover the saved connection");
        assert!(rtt.is_some());
    }

    #[test]
    #[ignore = "requires HUSHKEY_TEST_MODEL and HUSHKEY_TEST_SPEECH"]
    fn remote_record_session_finalizes_while_a_real_preview_is_in_flight() {
        use hushkey_core::{
            EngineConfig, Event as CoreEvent, RecordConfig, RecordSession, WavReplay,
        };
        let host_dir = tempfile::tempdir().unwrap();
        let client_dir = tempfile::tempdir().unwrap();
        let host_id = Identity::load_or_create(host_dir.path()).unwrap();
        let client_id = Identity::load_or_create(client_dir.path()).unwrap();
        let mut config =
            EngineConfig::new(std::env::var("HUSHKEY_TEST_MODEL").expect("model fixture"));
        config.use_gpu = false;
        let engine = Arc::new(Engine::new(config).unwrap());
        let (began, active) = mpsc::channel();
        let first = Arc::new(AtomicUsize::new(0));
        let server = Server::start(
            host_id.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(|_| true),
            Arc::new(move |audio, options| {
                if first.fetch_add(1, Ordering::SeqCst) == 0 {
                    began.send(()).unwrap();
                    // Exercise real key release before the first inference can finish.
                    std::thread::sleep(Duration::from_millis(100));
                }
                engine.transcribe(audio, options).map_err(|e| e.to_string())
            }),
        )
        .unwrap();
        let peer = Peer {
            name: host_id.name(),
            id: host_id.id(),
            address: server.local_addr(),
        };
        let client = Client::connect(client_id, &peer, Arc::new(|_| true)).unwrap();
        let backend = Arc::new(Recoverable::new(Arc::new(Remote(client))));
        let (events_tx, events) = mpsc::channel();
        let mut session = RecordSession::start(
            backend.clone(),
            Box::new(WavReplay::new(
                std::env::var("HUSHKEY_TEST_SPEECH").expect("speech fixture"),
                false,
            )),
            RecordConfig {
                preview_interval: Duration::from_millis(10),
                options: TranscribeOptions {
                    language: Some("en".into()),
                    ..TranscribeOptions::accurate()
                },
                ..Default::default()
            },
            Arc::new(move |e| {
                events_tx.send(e).unwrap();
            }),
        )
        .unwrap();
        active.recv_timeout(Duration::from_secs(5)).unwrap();
        let transcript = session.stop().unwrap().unwrap();
        assert!(
            transcript.text.to_lowercase().contains("country"),
            "{}",
            transcript.text
        );
        assert!(!backend.has_failed());
        let events: Vec<_> = events.try_iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, CoreEvent::Final { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, CoreEvent::Stopped))
                .count(),
            1
        );
        assert!(
            !events.iter().any(|e| matches!(e, CoreEvent::Error { .. })),
            "{events:?}"
        );
    }

    #[test]
    fn server_bind_failure_does_not_disable_the_client_role() {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (tx, rx) = mpsc::channel();
        let mut network = Network::with_identity(
            identity,
            true,
            occupied.local_addr().unwrap(),
            Arc::new(move |e| {
                tx.send(e).unwrap();
            }),
        );
        assert!(network.server.is_none());
        assert!(network.server_error.is_some());
        let host_dir = tempfile::tempdir().unwrap();
        let host_id = Identity::load_or_create(host_dir.path()).unwrap();
        let host = Server::start(
            host_id.clone(),
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(|_| true),
            Arc::new(|_, _| {
                Ok(Transcript {
                    text: "remote still works".into(),
                    ..Default::default()
                })
            }),
        )
        .unwrap();
        network.connect(
            Peer {
                name: host_id.name(),
                id: host_id.id(),
                address: host.local_addr(),
            },
            1,
        );
        loop {
            match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
                Event::PairingCode { generation: 1, .. } => {}
                Event::Connected { result, .. } => {
                    let (_, pending) = result.unwrap();
                    let backend = pending.finish().unwrap();
                    assert_eq!(
                        backend
                            .transcribe(&[0.1], &TranscribeOptions::accurate())
                            .unwrap()
                            .text,
                        "remote still works"
                    );
                    break;
                }
                _ => panic!("unexpected event"),
            }
        }
    }

    #[test]
    fn disabled_server_does_not_bind_an_inbound_socket() {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let network = Network::with_identity(
            identity,
            false,
            occupied.local_addr().unwrap(),
            Arc::new(|_| {}),
        );
        assert!(network.server.is_none());
        assert!(network.server_error.is_none());
    }

    struct Flaky(AtomicUsize);
    impl Transcriber for Flaky {
        fn transcribe_cancellable(
            &self,
            audio: &[f32],
            options: &TranscribeOptions,
            _: Option<&dyn Fn() -> bool>,
        ) -> hushkey_core::Result<Transcript> {
            assert_eq!(audio, [0.25, -0.5]);
            assert_eq!(options.language.as_deref(), Some("de"));
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(Error::Transcribe("disconnected".into()))
            } else {
                Ok(Transcript {
                    text: "Recovered".into(),
                    ..Default::default()
                })
            }
        }
    }

    #[test]
    fn failed_final_can_be_retried_without_recording_again() {
        let backend = Recoverable::new(Arc::new(Flaky(AtomicUsize::new(0))));
        let options = TranscribeOptions {
            language: Some("de".into()),
            ..TranscribeOptions::accurate()
        };
        assert!(backend.transcribe(&[0.25, -0.5], &options).is_err());
        assert!(backend.has_failed());
        assert_eq!(backend.retry().unwrap().text, "Recovered");
        assert!(!backend.has_failed());
        assert!(backend.retry().is_err());
    }

    #[test]
    fn reconnecting_replaces_a_dead_connection_and_preserves_failed_audio() {
        struct Offline;
        impl Transcriber for Offline {
            fn transcribe_cancellable(
                &self,
                _: &[f32],
                _: &TranscribeOptions,
                _: Option<&dyn Fn() -> bool>,
            ) -> hushkey_core::Result<Transcript> {
                Err(Error::Transcribe("old address unreachable".into()))
            }
        }
        let retained = Recoverable::new(Arc::new(Offline));
        let options = TranscribeOptions {
            language: Some("de".into()),
            ..TranscribeOptions::accurate()
        };
        assert!(retained.transcribe(&[0.25, -0.5], &options).is_err());
        let new_connection = Recoverable::new(Arc::new(Flaky(AtomicUsize::new(1))));
        retained.reconnect(&new_connection);
        assert!(retained.has_failed());
        assert_eq!(retained.retry().unwrap().text, "Recovered");
        assert!(!retained.has_failed());
    }

    #[test]
    fn preview_failure_does_not_leave_a_retryable_final() {
        let backend = Recoverable::new(Arc::new(Flaky(AtomicUsize::new(0))));
        let options = TranscribeOptions {
            language: Some("de".into()),
            ..TranscribeOptions::fast()
        };
        assert!(backend.transcribe(&[0.25, -0.5], &options).is_err());
        assert!(!backend.has_failed());
    }

    #[test]
    fn queued_remote_results_are_discarded_after_permission_generation_changes() {
        assert!(!current_result(Some(4), 5));
        assert!(current_result(Some(5), 5));
        assert!(current_result(None, 5)); // local dictation has no peer permission
    }

    #[test]
    fn successful_queued_final_remains_recoverable_until_the_app_accepts_it() {
        let backend = Recoverable::new(Arc::new(Flaky(AtomicUsize::new(1))));
        let options = TranscribeOptions {
            language: Some("de".into()),
            ..TranscribeOptions::accurate()
        };
        assert!(backend.transcribe(&[0.25, -0.5], &options).is_ok());
        assert!(!backend.has_failed());
        // Permission changes after backend success but before GUI delivery.
        backend.reject_result();
        assert!(backend.has_failed());
        let reconnected = Recoverable::new(Arc::new(Flaky(AtomicUsize::new(1))));
        backend.reconnect(&reconnected);
        assert_eq!(backend.retry().unwrap().text, "Recovered");
        backend.acknowledge();
        assert!(!backend.has_failed());
        assert!(backend.retry().is_err());
    }

    #[test]
    fn remote_label_retains_destination_after_long_preview() {
        let peer = Peer {
            name: "GPU-PC".into(),
            id: "ab".repeat(32),
            address: "127.0.0.1:47957".parse().unwrap(),
        };
        let text = overlay_text(Some(&peer), &"word ".repeat(300));
        assert!(text.chars().count() <= 450);
        assert!(text.starts_with("Remote · GPU-PC"));
        assert!(text.ends_with("word "));
    }
}
