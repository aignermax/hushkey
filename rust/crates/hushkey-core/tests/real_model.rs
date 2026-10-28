//! Real whisper.cpp inference on public speech.
//!
//! Needs HUSHKEY_TEST_MODEL (a GGML file, ggml-tiny.bin is enough) and
//! HUSHKEY_TEST_SPEECH (whisper.cpp's samples/jfk.wav); skipped without them.
//! CI downloads both.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hushkey_core::{
    read_wav_16k, Engine, EngineConfig, Event, RecordConfig, RecordSession, TranscribeOptions,
    WavReplay,
};

fn fixtures() -> Option<(PathBuf, PathBuf)> {
    let model = std::env::var_os("HUSHKEY_TEST_MODEL")?;
    let speech = std::env::var_os("HUSHKEY_TEST_SPEECH")?;
    Some((PathBuf::from(model), PathBuf::from(speech)))
}

macro_rules! fixtures_or_skip {
    () => {
        match fixtures() {
            Some(f) => f,
            None => {
                eprintln!("skipped: set HUSHKEY_TEST_MODEL and HUSHKEY_TEST_SPEECH");
                return;
            }
        }
    };
}

fn engine(model: PathBuf) -> Arc<Engine> {
    let mut config = EngineConfig::new(model);
    config.language = Some("en".into());
    Arc::new(Engine::new(config).expect("model loads"))
}

#[test]
fn transcribes_public_speech_repeatedly() {
    let (model, speech) = fixtures_or_skip!();
    let engine = engine(model);
    let audio = read_wav_16k(&speech).unwrap();
    for _ in 0..2 {
        let t = engine
            .transcribe(&audio, &TranscribeOptions::accurate())
            .unwrap();
        assert!(t.text.to_lowercase().contains("country"), "{:?}", t.text);
        assert_eq!(t.language.as_deref(), Some("en"));
        let duration_ms = audio.len() as i64 * 1000 / 16_000;
        assert!(t
            .segments
            .iter()
            .all(|s| 0 <= s.start_ms && s.start_ms <= s.end_ms && s.end_ms <= duration_ms));
    }
}

#[test]
fn silence_yields_no_text() {
    let (model, _) = fixtures_or_skip!();
    let engine = engine(model);
    let t = engine
        .transcribe(&vec![0.0; 16_000], &TranscribeOptions::accurate())
        .unwrap();
    assert_eq!(t.text, "");
    assert!(t.segments.is_empty());
}

fn record(
    engine: Arc<Engine>,
    speech: PathBuf,
    config: RecordConfig,
) -> (Vec<Event>, Option<String>) {
    let (tx, rx) = mpsc::channel();
    let sink: hushkey_core::EventSink = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    let mut session =
        RecordSession::start(engine, Box::new(WavReplay::new(speech, true)), config, sink).unwrap();
    std::thread::sleep(Duration::from_secs(12)); // jfk.wav is 11 s
    let transcript = session.stop().unwrap();
    (rx.try_iter().collect(), transcript.map(|t| t.text))
}

#[test]
fn a_recording_previews_then_finalizes_then_stops() {
    let (model, speech) = fixtures_or_skip!();
    let config = RecordConfig {
        preview_interval: Duration::from_secs(1),
        ..RecordConfig::default()
    };
    let (events, text) = record(engine(model), speech, config);
    let final_text = text.expect("a final transcript");
    assert!(
        final_text.to_lowercase().contains("country"),
        "{final_text:?}"
    );
    let previews = events
        .iter()
        .take_while(|e| matches!(e, Event::Preview { .. }))
        .count();
    assert!(previews >= 1, "{events:?}");
    assert!(
        matches!(&events[previews], Event::Final { transcript } if transcript.text == final_text)
    );
    assert_eq!(events.get(previews + 1), Some(&Event::Stopped));
    assert_eq!(events.len(), previews + 2);
}

#[test]
fn stopping_from_inside_the_callback_does_not_deadlock() {
    let (model, speech) = fixtures_or_skip!();
    let slot: Arc<Mutex<Option<RecordSession>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::channel();
    let in_callback = slot.clone();
    let sink: hushkey_core::EventSink = Arc::new(move |e| {
        if matches!(e, Event::Preview { .. }) {
            if let Some(session) = in_callback.lock().unwrap().as_mut() {
                assert!(
                    session.stop().unwrap().is_none(),
                    "a stop from the callback completes later"
                );
            }
        }
        let _ = tx.send(e);
    });
    let config = RecordConfig {
        preview_interval: Duration::from_millis(500),
        ..RecordConfig::default()
    };
    let session = RecordSession::start(
        engine(model),
        Box::new(WavReplay::new(speech, true)),
        config,
        sink,
    )
    .unwrap();
    *slot.lock().unwrap() = Some(session);
    let mut saw_final = false;
    loop {
        match rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the session stops")
        {
            Event::Final { .. } => saw_final = true,
            Event::Stopped => break,
            _ => {}
        }
    }
    assert!(saw_final);
    drop(slot.lock().unwrap().take()); // dropping after the stop is fine
}
