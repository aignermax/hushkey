use super::*;
use hushkey_core::{Segment, TranscribeOptions, Transcript};
use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn identity() -> (TempDir, Identity) {
    let d = tempfile::tempdir().unwrap();
    let i = Identity::load_or_create(d.path()).unwrap();
    (d, i)
}
fn yes() -> Approve {
    Arc::new(|_| true)
}
fn host(i: Identity, a: Approve, c: Compute) -> Server {
    Server::start(i, "127.0.0.1:0".parse().unwrap(), a, c).unwrap()
}
fn peer(i: &Identity, s: &Server) -> Peer {
    Peer {
        name: i.name(),
        id: i.id(),
        address: s.local_addr(),
    }
}
fn empty() -> Compute {
    Arc::new(|_, _| Ok(Transcript::default()))
}

// Catches lost samples/options, DTO omissions, mismatched SAS, reprompting and lost persisted keys.
#[test]
fn encrypted_roundtrip_and_persisted_pairing() {
    let (hd, hi) = identity();
    let (cd, ci) = identity();
    let codes = Arc::new(Mutex::new(Vec::new()));
    let hc = codes.clone();
    let cc = codes.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let expected = Transcript {
        text: "Hello LAN".into(),
        segments: vec![Segment {
            start_ms: 10,
            end_ms: 80,
            text: "Hello LAN".into(),
        }],
        language: Some("en".into()),
        no_speech: 0.25,
    };
    let output = expected.clone();
    let s = host(
        hi.clone(),
        Arc::new(move |a| {
            hc.lock().unwrap().push(a.code);
            true
        }),
        Arc::new(move |audio, o| {
            assert_eq!(audio, &[0.0, -0.25, 1.0, 0.125]);
            assert_eq!(o.language.as_deref(), Some("en"));
            assert_eq!(o.prompt.as_deref(), Some("LAN vocabulary"));
            assert_eq!(o.beam_size, 5);
            assert!(o.keep_pauses);
            count.fetch_add(1, Ordering::SeqCst);
            Ok(output.clone())
        }),
    );
    let p = peer(&hi, &s);
    let c = Client::connect(
        ci.clone(),
        &p,
        Arc::new(move |a| {
            cc.lock().unwrap().push(a.code);
            true
        }),
    )
    .unwrap();
    let options = TranscribeOptions {
        language: Some("en".into()),
        prompt: Some("LAN vocabulary".into()),
        beam_size: 5,
        keep_pauses: true,
    };
    assert_eq!(
        c.transcribe(&[0.0, -0.25, 1.0, 0.125], &options, None)
            .unwrap(),
        expected
    );
    let codes = codes.lock().unwrap();
    assert_eq!(codes.len(), 2);
    assert_eq!(codes[0], codes[1]);
    assert_eq!(codes[0].len(), 8);
    assert!(codes[0].bytes().all(|b| b.is_ascii_digit()));
    drop(codes);
    drop(s);
    let hi2 = Identity::load_or_create(hd.path()).unwrap();
    let ci2 = Identity::load_or_create(cd.path()).unwrap();
    assert_eq!(hi2.id(), hi.id());
    assert_eq!(hi2.name(), hi.name());
    let s = host(
        hi2.clone(),
        Arc::new(|_| panic!("trusted host reprompted")),
        empty(),
    );
    let c = Client::connect(
        ci2,
        &peer(&hi2, &s),
        Arc::new(|_| panic!("trusted client reprompted")),
    )
    .unwrap();
    c.transcribe(&[0.1], &TranscribeOptions::default(), None)
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Either denied endpoint must leave no trust and must never invoke compute.
#[test]
fn either_denial_does_not_compute_or_persist_trust() {
    for host_yes in [false, true] {
        let (_hd, hi) = identity();
        let (_cd, ci) = identity();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let s = host(
            hi.clone(),
            Arc::new(move |_| host_yes),
            Arc::new(move |_, _| {
                n2.fetch_add(1, Ordering::SeqCst);
                Ok(Transcript::default())
            }),
        );
        let p = peer(&hi, &s);
        assert!(Client::connect(ci.clone(), &p, Arc::new(move |_| !host_yes)).is_err());
        assert_eq!(n.load(Ordering::SeqCst), 0);
        drop(s);
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let s = host(
            hi.clone(),
            Arc::new(move |_| {
                n2.fetch_add(1, Ordering::SeqCst);
                true
            }),
            empty(),
        );
        Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn advertised_wrong_key_rejected_before_approval() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (_d, other) = identity();
    let s = host(hi.clone(), yes(), empty());
    let mut p = peer(&hi, &s);
    p.id = other.id();
    assert!(Client::connect(ci, &p, Arc::new(|_| panic!("wrong key asked for approval"))).is_err());
}

#[test]
fn revoke_and_disable_block_existing_clients() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let allows = Arc::new(AtomicUsize::new(0));
    let a = allows.clone();
    let s = host(
        hi.clone(),
        Arc::new(move |_| a.fetch_add(1, Ordering::SeqCst) == 0),
        empty(),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    s.set_enabled(false);
    assert!(c
        .transcribe(&[0.1], &TranscribeOptions::default(), None)
        .is_err());
    s.set_enabled(true);
    c.transcribe(&[0.1], &TranscribeOptions::default(), None)
        .unwrap();
    s.revoke_all().unwrap();
    assert!(c
        .transcribe(&[0.1], &TranscribeOptions::default(), None)
        .is_err());
    assert!(s.alive());
}

#[test]
fn rejects_invalid_audio_and_cancelled_request() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(|_, _| panic!("invalid audio reached compute")),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    for audio in [&[][..], &[f32::NAN], &[f32::INFINITY]] {
        assert!(c
            .transcribe(audio, &TranscribeOptions::default(), None)
            .is_err());
    }
    assert!(c
        .transcribe(&vec![0.0; 9_600_001], &TranscribeOptions::default(), None)
        .is_err());
    assert!(c
        .transcribe(&[0.1], &TranscribeOptions::default(), Some(&|| true))
        .is_err());
}

#[test]
fn malformed_oversized_and_disconnect_leave_server_usable() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let s = host(hi.clone(), yes(), empty());
    for payload in [&[255, 255, 255, 255][..], &[0, 0, 0, 1, 0], &[0, 0][..]] {
        let mut stream = TcpStream::connect(s.local_addr()).unwrap();
        stream.write_all(payload).unwrap();
    }
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    c.transcribe(&[0.1], &TranscribeOptions::default(), None)
        .unwrap();
    let addr = s.local_addr();
    let t = Instant::now();
    drop(s);
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err());
}

#[test]
fn unresponsive_handshake_has_finite_timeout() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        let (_s, _) = l.accept().unwrap();
        std::thread::sleep(Duration::from_secs(4));
    });
    let (_d, i) = identity();
    let (_d2, remote) = identity();
    let p = Peer {
        name: remote.name(),
        id: remote.id(),
        address: addr,
    };
    let t = Instant::now();
    assert!(Client::connect(i, &p, yes()).is_err());
    assert!(t.elapsed() < Duration::from_secs(4));
    task.join().unwrap();
}

#[test]
fn discovery_parser_uses_source_and_rejects_bad_advertisements() {
    let id = "a".repeat(64);
    let source: SocketAddr = "192.168.2.9:1234".parse().unwrap();
    let good = format!(r#"{{"version":2,"kind":"reply","name":"Office","id":"{id}","port":4455}}"#);
    let p = crate::discovery::parse_reply(good.as_bytes(), source).unwrap();
    assert_eq!(p.address, "192.168.2.9:4455".parse().unwrap());
    for bad in [
        good.replace("\"version\":2", "\"version\":3"),
        good.replace("Office", ""),
        good.replace(&id, "oops"),
        good.replace("4455", "0"),
        "x".repeat(2000),
    ] {
        assert!(crate::discovery::parse_reply(bad.as_bytes(), source).is_none());
    }
}

#[test]
fn busy_inference_fails_quickly_and_is_serialized() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(Transcript::default())
        }),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    let c2 = c.clone();
    let task =
        std::thread::spawn(move || c2.transcribe(&[0.1], &TranscribeOptions::default(), None));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let t = Instant::now();
    let error = c
        .transcribe(&[0.2], &TranscribeOptions::default(), None)
        .unwrap_err();
    assert!(error.contains("busy"), "{error}");
    assert!(t.elapsed() < Duration::from_secs(2));
    release_tx.send(()).unwrap();
    task.join().unwrap().unwrap();
}

#[test]
fn drop_interrupts_idle_connections() {
    let (_hd, hi) = identity();
    let s = host(hi, yes(), empty());
    let _idle: Vec<_> = (0..8)
        .map(|_| TcpStream::connect(s.local_addr()).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    drop(s);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn real_discovery_respects_enabled_and_excludes_self() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let s = host(hi.clone(), yes(), empty());
    s.start_discovery().unwrap();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.send_to(&vec![0u8; 4096], ("127.0.0.1", PORT)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(s.alive(), "oversized UDP datagram stopped server");
    assert!(discover(&ci, Duration::from_millis(250))
        .unwrap()
        .iter()
        .any(|p| p.id == hi.id() && p.address.port() == s.local_addr().port()));
    assert!(discover(&hi, Duration::from_millis(100))
        .unwrap()
        .iter()
        .all(|p| p.id != hi.id()));
    s.set_enabled(false);
    assert!(discover(&ci, Duration::from_millis(150))
        .unwrap()
        .iter()
        .all(|p| p.id != hi.id()));
}

// Both SAS dialogs must coexist: sequential approvals make comparison impossible.
#[test]
fn pairing_displays_both_codes_concurrently() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (h_tx, h_rx) = std::sync::mpsc::channel();
    let (c_tx, c_rx) = std::sync::mpsc::channel();
    let h_rx = Mutex::new(h_rx);
    let c_rx = Mutex::new(c_rx);
    let s = host(
        hi.clone(),
        Arc::new(move |a| {
            h_tx.send(a.code.clone()).unwrap();
            c_rx.lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(700))
                .is_ok_and(|code| code == a.code)
        }),
        empty(),
    );
    let result = Client::connect(
        ci,
        &peer(&hi, &s),
        Arc::new(move |a| {
            c_tx.send(a.code.clone()).unwrap();
            h_rx.lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(700))
                .is_ok_and(|code| code == a.code)
        }),
    );
    assert!(
        result.is_ok(),
        "both pairing dialogs must be visible together: {:?}",
        result.err()
    );
}

#[test]
#[ignore = "set HUSHKEY_TEST_MODEL and HUSHKEY_TEST_SPEECH to local model and WAV"]
fn real_model_encrypted_roundtrip() {
    use hushkey_core::{read_wav_16k, Engine, EngineConfig};
    let model = std::env::var("HUSHKEY_TEST_MODEL").expect("HUSHKEY_TEST_MODEL");
    let speech = std::env::var("HUSHKEY_TEST_SPEECH").expect("HUSHKEY_TEST_SPEECH");
    let mut config = EngineConfig::new(model);
    config.use_gpu = false;
    config.language = Some("en".into());
    let engine = Arc::new(Engine::new(config).unwrap());
    let audio = read_wav_16k(std::path::Path::new(&speech)).unwrap();
    let options = TranscribeOptions::accurate();
    let captured = Arc::new(Mutex::new(None));
    let output = captured.clone();
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(move |a, o| {
            let t = engine.transcribe(a, o).map_err(|e| e.to_string())?;
            *output.lock().unwrap() = Some(t.clone());
            Ok(t)
        }),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    let actual = c.transcribe(&audio, &options, None).unwrap();
    assert!(actual.text.to_lowercase().contains("country"));
    assert_eq!(actual, captured.lock().unwrap().take().unwrap());
}

// Bypasses client validation, while using a real authenticated Noise session.
fn trusted_channel(id: &Identity, p: &Peer) -> crate::wire::Channel {
    use crate::wire::{Channel, Io, Msg};
    let stream = TcpStream::connect(p.address).unwrap();
    let mut ch = Channel::handshake(Io::new(stream).unwrap(), id, true, None).unwrap();
    ch.send_msg(
        &Msg::Hello {
            name: id.name(),
            server_pinned: true,
        },
        None,
    )
    .unwrap();
    assert!(matches!(
        ch.recv_msg(None).unwrap(),
        Msg::Offer {
            compute_granted: true,
            ..
        }
    ));
    assert!(matches!(
        ch.recv_msg(None).unwrap(),
        Msg::Decision { allowed: true }
    ));
    ch.send_msg(&Msg::Decision { allowed: true }, None).unwrap();
    assert!(matches!(ch.recv_msg(None).unwrap(), Msg::Ready));
    ch
}

#[test]
fn malicious_authenticated_requests_never_reach_compute() {
    use crate::wire::{Msg, Options};
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(|_, _| panic!("malformed request computed")),
    );
    let p = peer(&hi, &s);
    Client::connect(ci.clone(), &p, yes()).unwrap();
    for samples in [0, 9_600_001] {
        let mut ch = trusted_channel(&ci, &p);
        ch.send_msg(
            &Msg::Request {
                samples,
                options: Options::from_core(&TranscribeOptions::default()).unwrap(),
            },
            None,
        )
        .unwrap();
        assert!(ch.recv_msg(None).is_err());
    }
    for bad in [f32::NAN.to_le_bytes().to_vec(), vec![1, 2, 3], vec![0; 8]] {
        let mut ch = trusted_channel(&ci, &p);
        ch.send_msg(
            &Msg::Request {
                samples: 1,
                options: Options::from_core(&TranscribeOptions::default()).unwrap(),
            },
            None,
        )
        .unwrap();
        assert!(matches!(ch.recv_msg(None).unwrap(), Msg::Accepted));
        ch.send(&bad, None).unwrap();
        assert!(ch.recv_msg(None).is_err());
    }
    let mut ch = trusted_channel(&ci, &p);
    ch.io.send(&[0; 32], None).unwrap(); // Invalid AEAD tag, no plaintext fallback.
    assert!(ch.recv_msg(None).is_err());
}

#[test]
fn cancellation_interrupts_inference_wait_without_retry() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(move |_, _| {
            n.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(Transcript::default())
        }),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancelled.clone();
    let t = std::thread::spawn(move || {
        c.transcribe(
            &[0.1],
            &TranscribeOptions::default(),
            Some(&|| flag.load(Ordering::SeqCst)),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let start = Instant::now();
    cancelled.store(true, Ordering::SeqCst);
    assert!(t.join().unwrap().is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    release_tx.send(()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn revoke_discards_running_inference_reply() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(move |_, _| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(Transcript {
                text: "must not arrive".into(),
                ..Transcript::default()
            })
        }),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    let t = std::thread::spawn(move || c.transcribe(&[0.1], &TranscribeOptions::default(), None));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    s.revoke_all().unwrap();
    release_tx.send(()).unwrap();
    assert!(t.join().unwrap().is_err());
}

#[test]
fn bounded_handlers_and_drop_with_blocked_approval() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let s = host(
        hi.clone(),
        Arc::new(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            true
        }),
        empty(),
    );
    let p = peer(&hi, &s);
    let task = std::thread::spawn(move || Client::connect(ci, &p, yes()));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let _idle: Vec<_> = (0..7)
        .map(|_| TcpStream::connect(s.local_addr()).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let (_d, other) = identity();
    let start = Instant::now();
    assert!(Client::connect(other, &peer(&hi, &s), yes()).is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    let start = Instant::now();
    drop(s);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(task.join().unwrap().is_err());
    release_tx.send(()).unwrap();
}

#[cfg(unix)]
#[test]
fn identity_files_are_private_and_permissions_are_repaired() {
    use std::os::unix::fs::PermissionsExt;
    let (d, i) = identity();
    let (_d2, i2) = identity();
    i.confirm(
        crate::identity::Role::PinnedServer,
        &i2.id(),
        i.epoch().unwrap(),
        true,
    )
    .unwrap();
    for name in ["identity.json", "identity.lock", "trusted.json"] {
        let p = d.path().join(name);
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let i = Identity::load_or_create(d.path()).unwrap();
    assert!(i
        .authorized(
            crate::identity::Role::PinnedServer,
            &i2.id(),
            i.epoch().unwrap()
        )
        .unwrap());
    for name in ["identity.json", "identity.lock", "trusted.json"] {
        assert_eq!(
            std::fs::metadata(d.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn review_outbound_pin_never_authorizes_reverse_compute() {
    let (_ad, a) = identity();
    let (_bd, b) = identity();
    let b_host = host(b.clone(), yes(), empty());
    Client::connect(a.clone(), &peer(&b, &b_host), yes()).unwrap();
    let approvals = Arc::new(AtomicUsize::new(0));
    let n = approvals.clone();
    let a_host = host(
        a.clone(),
        Arc::new(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            false
        }),
        empty(),
    );
    let result = Client::connect(b, &peer(&a, &a_host), yes());
    assert!(
        result.is_err(),
        "outbound pin silently granted reverse compute"
    );
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
}

#[test]
fn review_inbound_grant_never_pins_outbound_server() {
    let (_ad, a) = identity();
    let (_bd, b) = identity();
    let a_host = host(a.clone(), yes(), empty());
    Client::connect(b.clone(), &peer(&a, &a_host), yes()).unwrap();
    let b_host = host(b.clone(), yes(), empty());
    let approvals = Arc::new(AtomicUsize::new(0));
    let n = approvals.clone();
    let result = Client::connect(
        a,
        &peer(&b, &b_host),
        Arc::new(move |_| {
            n.fetch_add(1, Ordering::SeqCst);
            false
        }),
    );
    assert!(
        result.is_err(),
        "inbound grant silently pinned outbound server"
    );
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
}

#[test]
fn review_revoke_between_confirmation_and_ready_cannot_restore_pin() {
    use crate::wire::{Channel, Io, Msg};
    let (cd, ci) = identity();
    let (_hd, hi) = identity();
    // A separately loaded Identity must observe and update the same revocation epoch.
    let revoker = Identity::load_or_create(cd.path()).unwrap();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = Peer {
        name: hi.name(),
        id: hi.id(),
        address: tcp.local_addr().unwrap(),
    };
    let (confirmed_tx, confirmed_rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let remote = std::thread::spawn(move || {
        let (socket, _) = tcp.accept().unwrap();
        let mut ch = Channel::handshake(Io::new(socket).unwrap(), &hi, false, None).unwrap();
        assert!(matches!(ch.recv_msg(None).unwrap(), Msg::Hello { .. }));
        ch.send_msg(
            &Msg::Offer {
                name: hi.name(),
                compute_granted: false,
            },
            None,
        )
        .unwrap();
        ch.send_msg(&Msg::Decision { allowed: true }, None).unwrap();
        assert!(matches!(
            ch.recv_msg(None).unwrap(),
            Msg::Decision { allowed: true }
        ));
        confirmed_tx.send(()).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        ch.send_msg(&Msg::Ready, None).unwrap();
    });
    let client = std::thread::spawn(move || Client::connect(ci, &p, yes()));
    confirmed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    revoker.clear_trust().unwrap();
    ready_tx.send(()).unwrap();
    let result = client.join().unwrap();
    remote.join().unwrap();
    assert!(
        result.is_err(),
        "late Ready restored a pin after persisted revocation"
    );
}

#[test]
fn review_revoke_during_host_approval_cannot_restore_grant() {
    let (hd, hi) = identity();
    let (_cd, ci) = identity();
    let revoker = Identity::load_or_create(hd.path()).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let s = host(
        hi.clone(),
        Arc::new(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            true
        }),
        empty(),
    );
    let p = peer(&hi, &s);
    let client = std::thread::spawn(move || Client::connect(ci, &p, yes()));
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    revoker.clear_trust().unwrap();
    release_tx.send(()).unwrap();
    assert!(
        client.join().unwrap().is_err(),
        "stale host dialog restored revoked grant"
    );
}

#[test]
fn review_final_waits_for_cancelled_preview_and_previews_fail_fast() {
    let (_hd, hi) = identity();
    let (_cd, ci) = identity();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let s = host(
        hi.clone(),
        yes(),
        Arc::new(move |_, o| {
            if o.beam_size <= 1 {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            n.fetch_add(1, Ordering::SeqCst);
            Ok(Transcript {
                text: format!("beam {}", o.beam_size),
                ..Transcript::default()
            })
        }),
    );
    let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
    let preview = c.clone();
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel = flag.clone();
    let task = std::thread::spawn(move || {
        preview.transcribe(
            &[0.1],
            &TranscribeOptions::fast(),
            Some(&|| cancel.load(Ordering::SeqCst)),
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    flag.store(true, Ordering::SeqCst);
    assert!(task.join().unwrap().is_err());
    let final_client = c.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let final_task = std::thread::spawn(move || {
        let r = final_client.transcribe(&[0.2], &TranscribeOptions::accurate(), None);
        done_tx.send(r).unwrap();
    });
    let pending = done_rx.recv_timeout(Duration::from_millis(2300));
    let start = Instant::now();
    let busy = c
        .transcribe(&[0.3], &TranscribeOptions::fast(), None)
        .unwrap_err();
    release_tx.send(()).unwrap();
    let result = match pending {
        Ok(r) => r,
        Err(_) => done_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
    };
    final_task.join().unwrap();
    assert!(busy.contains("server busy"));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(result.unwrap().text, "beam 5");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn review_legacy_trust_is_discarded_and_epochs_survive_reload() {
    use crate::identity::Role;
    let (d, _initial) = identity();
    let (_pd, p) = identity();
    std::fs::write(
        d.path().join("trusted.json"),
        serde_json::to_vec(&vec![p.id()]).unwrap(),
    )
    .unwrap();
    let i = Identity::load_or_create(d.path()).unwrap();
    let before = i.epoch().unwrap();
    for role in [Role::PinnedServer, Role::ComputeGrant] {
        assert!(!i.authorized(role, &p.id(), before).unwrap());
    }
    i.confirm(Role::PinnedServer, &p.id(), before, true)
        .unwrap();
    let revoker = Identity::load_or_create(d.path()).unwrap();
    revoker.clear_trust().unwrap();
    let reloaded = Identity::load_or_create(d.path()).unwrap();
    assert_eq!(reloaded.epoch().unwrap(), before + 1);
    for role in [Role::PinnedServer, Role::ComputeGrant] {
        assert!(i.confirm(role, &p.id(), before, true).is_err());
        assert!(!reloaded.authorized(role, &p.id(), before + 1).unwrap());
    }
    drop(i);
}

#[test]
fn review_final_queue_is_cancellable_and_revoke_stops_waiters() {
    for revoke in [false, true] {
        let (_hd, hi) = identity();
        let (_cd, ci) = identity();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let n = calls.clone();
        let s = host(
            hi.clone(),
            yes(),
            Arc::new(move |_, _| {
                n.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Ok(Transcript::default())
            }),
        );
        let c = Client::connect(ci, &peer(&hi, &s), yes()).unwrap();
        let preview = c.clone();
        let preview_task = std::thread::spawn(move || {
            preview.transcribe(&[0.1], &TranscribeOptions::fast(), None)
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancelled.clone();
        let final_task = std::thread::spawn(move || {
            c.transcribe(
                &[0.2],
                &TranscribeOptions::accurate(),
                Some(&|| flag.load(Ordering::SeqCst)),
            )
        });
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        if revoke {
            s.revoke_all().unwrap();
        } else {
            cancelled.store(true, Ordering::SeqCst);
        }
        assert!(final_task.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        // Closing the host interrupts queued handlers; the user callback remains bounded.
        drop(s);
        release_tx.send(()).unwrap();
        assert!(preview_task.join().unwrap().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn review_identity_lock_contention_waits_instead_of_rejecting_peer() {
    use fs2::FileExt;
    let (d, _i) = identity();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(d.path().join("identity.lock"))
        .unwrap();
    file.lock_exclusive().unwrap();
    let path = d.path().to_owned();
    let waiter = std::thread::spawn(move || Identity::load_or_create(&path));
    std::thread::sleep(Duration::from_millis(100));
    FileExt::unlock(&file).unwrap();
    assert!(
        waiter.join().unwrap().is_ok(),
        "ordinary identity lock contention rejected a peer"
    );
}
