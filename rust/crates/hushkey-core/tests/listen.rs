//! Continuous listening on real speech.
//!
//! Needs HUSHKEY_TEST_MODEL, HUSHKEY_TEST_SPEECH and HUSHKEY_TEST_VAD (the
//! Silero GGML model); skipped without them.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hushkey_core::{
    read_wav_16k, Engine, EngineConfig, Event, EventSink, ListenConfig, ListenSession, WavReplay,
};

fn fixtures() -> Option<(PathBuf, PathBuf, PathBuf)> {
    Some((
        std::env::var_os("HUSHKEY_TEST_MODEL")?.into(),
        std::env::var_os("HUSHKEY_TEST_SPEECH")?.into(),
        std::env::var_os("HUSHKEY_TEST_VAD")?.into(),
    ))
}

macro_rules! fixtures_or_skip {
    () => {
        match fixtures() {
            Some(f) => f,
            None => {
                eprintln!(
                    "skipped: set HUSHKEY_TEST_MODEL, HUSHKEY_TEST_SPEECH and HUSHKEY_TEST_VAD"
                );
                return;
            }
        }
    };
}

fn write_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

fn listen(model: PathBuf, vad: PathBuf, audio: &[f32], name: &str) -> Vec<Event> {
    let mut config = EngineConfig::new(model);
    config.language = Some("en".into());
    config.vad_model = Some(vad);
    let engine = Arc::new(Engine::new(config).unwrap());
    let dir = std::env::temp_dir().join(format!("hk-listen-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("input.wav");
    write_wav(&wav, audio);
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let mut session = ListenSession::start(
        engine,
        Box::new(WavReplay::new(&wav, false)),
        ListenConfig::default(),
        sink,
    )
    .unwrap();
    session.wait().unwrap(); // ends by itself when the file ran out
    std::fs::remove_dir_all(&dir).ok();
    rx.recv_timeout(Duration::from_secs(1))
        .into_iter()
        .chain(rx.try_iter())
        .collect()
}

#[test]
fn two_sentences_with_a_pause_become_separate_utterances() {
    let (model, speech, vad) = fixtures_or_skip!();
    let jfk = read_wav_16k(&speech).unwrap();
    let audio = [
        jfk.clone(),
        vec![0.0; 32_000],
        jfk.clone(),
        vec![0.0; 16_000],
    ]
    .concat();
    let events = listen(model, vad, &audio, "two");

    assert_eq!(events.last(), Some(&Event::Stopped), "{events:#?}");
    let utterances: Vec<(String, i64, i64)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Utterance {
                transcript,
                start_ms,
                end_ms,
            } => Some((transcript.text.to_lowercase(), *start_ms, *end_ms)),
            _ => None,
        })
        .collect();
    assert!(utterances.len() >= 2, "{utterances:#?}");
    let all: String = utterances
        .iter()
        .map(|u| u.0.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(all.matches("country").count(), 4, "{all}"); // twice per sentence
                                                            // In order, not overlapping, and nothing in the 2 s of silence.
    let gap = (
        jfk.len() as i64 * 1000 / 16_000 + 300,
        jfk.len() as i64 * 1000 / 16_000 + 1700,
    );
    for pair in utterances.windows(2) {
        assert!(pair[0].2 <= pair[1].1, "{utterances:#?}");
    }
    assert!(
        utterances.iter().all(|u| u.2 <= gap.0 || u.1 >= gap.1),
        "{utterances:#?}"
    );
    // Every utterance sits between a SpeechStart and its SpeechEnd, and
    // every SpeechStart gets its SpeechEnd.
    let (mut open, mut heard) = (false, false);
    for e in &events {
        match e {
            Event::SpeechStart { .. } => {
                assert!(!open, "{events:#?}");
                (open, heard) = (true, false);
            }
            Event::Utterance { .. } => {
                assert!(open && !heard, "{events:#?}");
                heard = true;
            }
            Event::SpeechEnd { .. } => {
                assert!(open, "{events:#?}");
                open = false;
            }
            _ => {}
        }
    }
    assert!(!open, "{events:#?}");
}

#[test]
fn silence_produces_no_utterance() {
    let (model, _, vad) = fixtures_or_skip!();
    let events = listen(model, vad, &vec![0.0; 48_000], "silence");
    assert_eq!(events, vec![Event::Stopped]);
}

#[test]
fn listening_needs_a_vad_model() {
    let (model, _, _) = fixtures_or_skip!();
    let engine = Arc::new(Engine::new(EngineConfig::new(model)).unwrap());
    let sink: EventSink = Arc::new(|_| {});
    let err = ListenSession::start(
        engine,
        Box::new(WavReplay::new("unused.wav", false)),
        ListenConfig::default(),
        sink,
    )
    .err()
    .expect("refused");
    assert!(err.to_string().contains("VAD"), "{err}");
}

/// An input that fails right after starting, like an unplugged microphone.
struct FailingInput;

impl hushkey_core::AudioInput for FailingInput {
    fn start(&mut self, mut sink: hushkey_core::AudioSink) -> hushkey_core::Result<()> {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            (sink.on_error)("device unplugged".into());
        });
        Ok(())
    }

    fn stop(&mut self) {}
}

#[test]
fn stopping_from_the_error_callback_does_not_deadlock() {
    let (model, _, vad) = fixtures_or_skip!();
    let mut config = EngineConfig::new(model);
    config.vad_model = Some(vad);
    let engine = Arc::new(Engine::new(config).unwrap());
    let slot: Arc<std::sync::Mutex<Option<ListenSession>>> = Arc::default();
    let (tx, rx) = mpsc::channel();
    let in_callback = slot.clone();
    let sink: EventSink = Arc::new(move |e| {
        if matches!(e, Event::Error { .. }) {
            if let Some(session) = in_callback.lock().unwrap().as_mut() {
                session.stop().unwrap(); // the host's natural reaction
            }
        }
        let _ = tx.send(e);
    });
    let session = ListenSession::start(
        engine,
        Box::new(FailingInput),
        ListenConfig::default(),
        sink,
    )
    .unwrap();
    *slot.lock().unwrap() = Some(session);
    let mut events = Vec::new();
    while let Ok(e) = rx.recv_timeout(Duration::from_secs(30)) {
        let done = e == Event::Stopped;
        events.push(e);
        if done {
            break;
        }
    }
    assert_eq!(
        events,
        vec![
            Event::Error {
                message: "device unplugged".into()
            },
            Event::Stopped
        ]
    );
    drop(slot.lock().unwrap().take());
}

/// Answers by repeating; records what it was asked.
struct EchoBrain(Arc<std::sync::Mutex<Vec<String>>>);

impl hushkey_core::Brain for EchoBrain {
    fn reply(&mut self, utterance: &str) -> Result<hushkey_core::Reply, String> {
        self.0.lock().unwrap().push(utterance.to_string());
        std::thread::sleep(Duration::from_millis(300)); // thinking takes a while
        Ok(hushkey_core::Reply {
            text: format!("echo: {utterance}"),
            details: None,
        })
    }
}

#[test]
fn spoken_commands_become_command_events_and_the_rest_goes_to_the_brain() {
    let (model, _, vad) = fixtures_or_skip!();
    let mut config = EngineConfig::new(model);
    config.language = Some("de".into());
    config.vad_model = Some(vad);
    let engine = Arc::new(Engine::new(config).unwrap());
    let commands = hushkey_core::CommandSet::from_json(
        r#"{"commands": [
            {"name": "next", "phrases": ["weiter"]},
            {"name": "back", "phrases": ["zurück"]},
            {"name": "channel", "phrases": ["kanal {n}"]}]}"#,
    )
    .unwrap();
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listen = ListenConfig {
        commands: Some(Arc::new(commands)),
        brain: Some(Box::new(EchoBrain(asked.clone()))),
        ..ListenConfig::default()
    };
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let wav = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/commands-de.wav");
    let mut session =
        ListenSession::start(engine, Box::new(WavReplay::new(&wav, false)), listen, sink).unwrap();
    session.wait().unwrap();
    let events: Vec<Event> = rx.try_iter().collect();

    let commands: Vec<(String, Vec<String>)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Command { command, .. } => Some((command.name.clone(), command.args.clone())),
            _ => None,
        })
        .collect();
    let expected: Vec<(String, Vec<String>)> = vec![
        ("next".into(), vec![]),
        ("channel".into(), vec!["6".into()]),
        ("channel".into(), vec!["21".into()]),
        ("back".into(), vec![]),
    ];
    assert_eq!(commands, expected, "{events:#?}");
    // Only the question went to the brain, and its answer came before Stopped.
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "{:?}",
        asked.lock().unwrap()
    );
    let reply = events
        .iter()
        .position(|e| matches!(e, Event::Reply { text, .. } if text.to_lowercase().starts_with("echo: wie spät")));
    assert!(reply.is_some(), "{events:#?}");
    assert_eq!(events.last(), Some(&Event::Stopped));
    // Every command follows the utterance it came from.
    for (i, e) in events.iter().enumerate() {
        if let Event::Command { text, .. } = e {
            assert!(
                matches!(&events[i - 1], Event::Utterance { transcript, .. } if &transcript.text == text)
            );
        }
    }
}

/// Blocks on its first question until the test opens the gate; counts
/// what it was asked.
struct GatedBrain {
    asked: Arc<std::sync::atomic::AtomicUsize>,
    gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl hushkey_core::Brain for GatedBrain {
    fn reply(&mut self, _utterance: &str) -> Result<hushkey_core::Reply, String> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (open, signal) = &*self.gate;
        let _open = signal
            .wait_while(open.lock().unwrap(), |open| !*open)
            .unwrap();
        Ok(hushkey_core::Reply {
            text: "ok".into(),
            details: None,
        })
    }
}

#[test]
fn after_a_stop_queued_utterances_no_longer_reach_the_brain() {
    let (model, _, vad) = fixtures_or_skip!();
    let mut config = EngineConfig::new(model);
    config.language = Some("de".into());
    config.vad_model = Some(vad);
    let engine = Arc::new(Engine::new(config).unwrap());
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let listen = ListenConfig {
        brain: Some(Box::new(GatedBrain {
            asked: asked.clone(),
            gate: gate.clone(),
        })),
        ..ListenConfig::default()
    };
    let (tx, rx) = mpsc::channel();
    let sink: EventSink = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let wav = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/commands-de.wav");
    // Without commands all five utterances go to the brain; it holds on to
    // the first, so the other four queue up whatever the machine's speed.
    let mut session =
        ListenSession::start(engine, Box::new(WavReplay::new(&wav, false)), listen, sink).unwrap();
    let mut utterances = 0;
    while utterances < 5 {
        let event = rx
            .recv_timeout(Duration::from_secs(120))
            .expect("utterances");
        if matches!(event, Event::Utterance { .. }) {
            utterances += 1;
        }
    }
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    session.stop().unwrap();
    *gate.0.lock().unwrap() = true; // the request in flight finishes
    gate.1.notify_all();
    std::thread::sleep(Duration::from_secs(1));
    let asked = asked.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        asked, 1,
        "the brain was asked {asked} times; the queue left the machine"
    );
}
