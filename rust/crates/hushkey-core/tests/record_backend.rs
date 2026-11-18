use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use hushkey_core::{
    AudioInput, AudioSink, Error, Event, RecordConfig, RecordSession, Result, TranscribeOptions,
    Transcriber, Transcript,
};

struct Input;
impl AudioInput for Input {
    fn start(&mut self, mut sink: AudioSink) -> Result<()> {
        (sink.on_samples)(&vec![0.25; 16_000]);
        Ok(())
    }
    fn stop(&mut self) {}
}

struct Backend {
    fail: bool,
    calls: Mutex<Vec<(usize, u32)>>,
}
impl Transcriber for Backend {
    fn transcribe_cancellable(
        &self,
        audio: &[f32],
        options: &TranscribeOptions,
        abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcript> {
        if abort.is_some_and(|f| f()) {
            return Err(Error::Cancelled);
        }
        self.calls
            .lock()
            .unwrap()
            .push((audio.len(), options.beam_size));
        if self.fail {
            return Err(Error::Transcribe("server disconnected".into()));
        }
        assert!(audio.iter().all(|&s| s == 0.25));
        Ok(Transcript {
            text: "Recognized on server".into(),
            ..Default::default()
        })
    }
}

#[test]
fn capture_uses_selected_backend_for_preview_and_final() {
    let backend = Arc::new(Backend {
        fail: false,
        calls: Mutex::new(Vec::new()),
    });
    let (tx, rx) = mpsc::channel();
    let mut session = RecordSession::start(
        backend.clone(),
        Box::new(Input),
        RecordConfig {
            preview_interval: Duration::from_millis(10),
            ..Default::default()
        },
        Arc::new(move |e| {
            tx.send(e).unwrap();
        }),
    )
    .unwrap();
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Event::Preview { .. }
    ));
    assert_eq!(
        session.stop().unwrap().unwrap().text,
        "Recognized on server"
    );
    assert_eq!(
        *backend.calls.lock().unwrap(),
        vec![(16_000, 1), (16_000, 5)]
    );
    assert!(matches!(rx.recv().unwrap(), Event::Final { .. }));
    assert_eq!(rx.recv().unwrap(), Event::Stopped);
}

#[test]
fn failed_final_is_an_error_instead_of_successful_empty_dictation() {
    let backend = Arc::new(Backend {
        fail: true,
        calls: Mutex::new(Vec::new()),
    });
    let (tx, rx) = mpsc::channel();
    let mut session = RecordSession::start(
        backend,
        Box::new(Input),
        RecordConfig {
            preview: false,
            ..Default::default()
        },
        Arc::new(move |e| {
            tx.send(e).unwrap();
        }),
    )
    .unwrap();
    assert!(session
        .stop()
        .unwrap_err()
        .to_string()
        .contains("server disconnected"));
    assert!(matches!(rx.recv().unwrap(), Event::Error { .. }));
    assert_eq!(rx.recv().unwrap(), Event::Stopped);
}

#[test]
fn cancelling_a_tap_does_not_send_audio_for_final_inference() {
    let backend = Arc::new(Backend {
        fail: false,
        calls: Mutex::new(Vec::new()),
    });
    let mut session = RecordSession::start(
        backend.clone(),
        Box::new(Input),
        RecordConfig {
            preview: false,
            ..Default::default()
        },
        Arc::new(|_| {}),
    )
    .unwrap();
    assert!(session.cancel().unwrap().is_none());
    assert!(backend.calls.lock().unwrap().is_empty());
}

#[test]
fn a_capture_limit_bounds_memory_and_reports_the_cutoff() {
    let backend = Arc::new(Backend {
        fail: false,
        calls: Mutex::new(Vec::new()),
    });
    let (tx, rx) = mpsc::channel();
    let mut session = RecordSession::start(
        backend.clone(),
        Box::new(Input),
        RecordConfig {
            preview: false,
            max_duration: Some(Duration::from_millis(500)),
            ..Default::default()
        },
        Arc::new(move |e| {
            tx.send(e).unwrap();
        }),
    )
    .unwrap();
    assert_eq!(
        session.stop().unwrap().unwrap().text,
        "Recognized on server"
    );
    assert_eq!(*backend.calls.lock().unwrap(), vec![(8_000, 5)]);
    assert!(rx
        .try_iter()
        .any(|e| matches!(e, Event::Error { message } if message.contains("limit"))));
}
