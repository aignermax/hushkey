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
    // Every utterance was announced by a SpeechStart.
    let mut open = false;
    for e in &events {
        match e {
            Event::SpeechStart { .. } => {
                assert!(!open, "{events:#?}");
                open = true;
            }
            Event::Utterance { .. } => {
                assert!(open, "{events:#?}");
                open = false;
            }
            _ => {}
        }
    }
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
