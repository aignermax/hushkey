//! Desktop ownership of LAN computation. No network worker touches native UI.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use hushkey_core::{Engine, Error, TranscribeOptions, Transcriber, Transcript};
use hushkey_net::{Approval, Client, Identity, Peer, Server};

use crate::{config, strings};

pub enum Event {
    Approval {
        request: Approval,
        hosting: bool,
        reply: mpsc::SyncSender<bool>,
        expires: Instant,
    },
    Discovered(std::result::Result<Vec<Peer>, String>),
    Connected {
        generation: u64,
        result: std::result::Result<(Peer, Arc<Recoverable>), String>,
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
        let server = Server::start(
            self.identity.clone(),
            self.bind,
            approval(self.notify.clone(), true),
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

    pub fn connect(&self, peer: Peer, generation: u64) {
        let (identity, notify) = (self.identity.clone(), self.notify.clone());
        std::thread::spawn(move || {
            let result = Client::connect(identity, &peer, approval(notify.clone(), false))
                .map(|client| (peer, Arc::new(Recoverable::new(Arc::new(Remote(client))))));
            notify(Event::Connected { generation, result });
        });
    }
}

fn approval(notify: Notify, hosting: bool) -> hushkey_net::Approve {
    Arc::new(move |request| {
        let (reply, answer) = mpsc::sync_channel(1);
        notify(Event::Approval {
            request,
            hosting,
            reply,
            expires: Instant::now() + Duration::from_secs(85),
        });
        answer
            .recv_timeout(Duration::from_secs(90))
            .unwrap_or(false)
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

/// Retain only a failed final request, in memory, until explicit retry/discard.
pub struct Recoverable {
    backend: Mutex<Arc<dyn Transcriber>>,
    failed: Mutex<Option<(Vec<f32>, TranscribeOptions)>>,
}
impl Recoverable {
    pub fn new(backend: Arc<dyn Transcriber>) -> Self {
        Self {
            backend: Mutex::new(backend),
            failed: Mutex::new(None),
        }
    }

    pub fn has_failed(&self) -> bool {
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    pub fn discard(&self) {
        self.failed.lock().unwrap_or_else(|p| p.into_inner()).take();
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
        let (audio, options) = self
            .failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| Error::Transcribe("no failed recording to retry".into()))?;
        self.transcribe(&audio, &options)
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
            let mut failed = self.failed.lock().unwrap_or_else(|p| p.into_inner());
            *failed = if result.is_err() {
                Some((audio.to_vec(), options.clone()))
            } else {
                None
            };
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

pub fn consent(s: &strings::Strings, request: &Approval, hosting: bool) -> String {
    let intro = if hosting {
        words(
            s,
            "möchte deine Spracherkennung verwenden.",
            "wants to use your speech recognition.",
        )
    } else {
        words(
            s,
            "soll deine Sprache verarbeiten.",
            "will process your speech.",
        )
    };
    let scope = if hosting {
        words(s, "Dieses Gerät darf danach deine lokale Spracherkennung verwenden. Dieser Rechner entschlüsselt und verarbeitet dessen Sprache im Arbeitsspeicher.", "This device will be allowed to use your local speech recognition. This computer decrypts and processes its speech in memory.")
    } else {
        words(s, "Der gewählte Rechner darf deine Diktate verarbeiten und muss sie dazu im Arbeitsspeicher entschlüsseln. Das gibt ihm keinen Zugriff auf deine eigene Spracherkennung.", "The selected computer may process your dictations and must decrypt them in memory. This does not grant it access to your own speech recognition.")
    };
    let disclosure = words(s,
        "Audio und Text werden verschlüsselt übertragen. Hush-Key speichert auf dem Server keine Aufnahmen oder Transkripte. Nur vertrauenswürdige Geräte genehmigen.\n\nVergleiche den Code auf BEIDEN Geräten. Klicke nur auf OK, wenn beide Codes übereinstimmen. Die Freigabe wird für dieses Gerät gespeichert und kann im Netzwerkmenü widerrufen werden.",
        "Audio and text are encrypted in transit. Hush-Key saves no recordings or transcripts on the server. Approve only trusted devices.\n\nCompare the code on BOTH devices. Click OK only if both codes match. Permission is saved for this device and can be revoked in the network menu.");
    format!(
        "{} {intro}\n\nID: {}\n{}\n\nCode: {}\n\n{scope}\n\n{disclosure}",
        request.peer.name,
        request.peer.id,
        request.peer.address.ip(),
        request.code
    )
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
        let network = Network::with_identity(
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
                Event::Approval { reply, .. } => reply.send(true).unwrap(),
                Event::Connected { result, .. } => {
                    let (_, backend) = result.unwrap();
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
