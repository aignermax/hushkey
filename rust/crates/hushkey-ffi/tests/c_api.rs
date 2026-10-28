//! The C functions, called the way a C host calls them.
//!
//! Needs HUSHKEY_TEST_MODEL and HUSHKEY_TEST_SPEECH (see hushkey-core's
//! real_model tests); the tests that do not load a model always run.

use std::ffi::{c_void, CStr, CString};
use std::mem::size_of;
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use std::time::Duration;

use hushkey::*;

fn fixtures() -> Option<(CString, CString)> {
    let model = std::env::var("HUSHKEY_TEST_MODEL").ok()?;
    let speech = std::env::var("HUSHKEY_TEST_SPEECH").ok()?;
    Some((CString::new(model).unwrap(), CString::new(speech).unwrap()))
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

fn last_error() -> String {
    let p = hk_last_error();
    assert!(!p.is_null(), "a failure sets hk_last_error");
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

unsafe fn engine(model: &CString) -> *mut HkEngine {
    let mut cfg: HkEngineConfig = std::mem::zeroed();
    cfg.size = size_of::<HkEngineConfig>();
    hk_engine_config_init(&mut cfg);
    cfg.model = model.as_ptr();
    let language = c"en";
    cfg.language = language.as_ptr();
    let engine = hk_engine_new(&cfg);
    assert!(!engine.is_null(), "{}", last_error());
    engine
}

unsafe fn take(s: *mut std::ffi::c_char) -> String {
    assert!(!s.is_null(), "{}", last_error());
    let text = CStr::from_ptr(s).to_string_lossy().into_owned();
    hk_string_free(s);
    text
}

#[test]
fn versions_are_reported() {
    assert_eq!(hk_abi_version(), HK_ABI_VERSION);
    let version = unsafe { CStr::from_ptr(hk_version()) }.to_str().unwrap();
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_missing_model_fails_with_a_reason() {
    unsafe {
        let mut cfg: HkEngineConfig = std::mem::zeroed();
        cfg.size = size_of::<HkEngineConfig>();
        hk_engine_config_init(&mut cfg);
        let missing = c"no-such-model-xyz";
        cfg.model = missing.as_ptr();
        assert!(hk_engine_new(&cfg).is_null());
        assert!(last_error().contains("no-such-model-xyz"));
        assert!(hk_engine_new(ptr::null()).is_null());
        assert!(last_error().contains("NULL"));
    }
}

#[test]
fn an_old_caller_with_a_smaller_struct_gets_defaults() {
    unsafe {
        // A caller compiled when the struct ended after `model`.
        let mut cfg: HkEngineConfig = std::mem::zeroed();
        cfg.size = std::mem::offset_of!(HkEngineConfig, use_gpu);
        cfg.use_gpu = false;
        cfg.threads = 77;
        hk_engine_config_init(&mut cfg);
        // init must not write beyond the caller's size
        assert!(!cfg.use_gpu);
        assert_eq!(cfg.threads, 77);
    }
}

#[test]
fn transcribes_a_file_and_samples() {
    let (model, speech) = fixtures_or_skip!();
    unsafe {
        let engine = engine(&model);
        let text = take(hk_transcribe_file(engine, speech.as_ptr(), ptr::null()));
        assert!(text.to_lowercase().contains("country"), "{text:?}");
        let silence = vec![0.0f32; 16_000];
        assert_eq!(
            take(hk_transcribe_pcm(
                engine,
                silence.as_ptr(),
                silence.len(),
                ptr::null()
            )),
            ""
        );
        assert!(hk_transcribe_file(engine, c"missing.wav".as_ptr(), ptr::null()).is_null());
        assert!(last_error().contains("missing.wav"));
        hk_engine_free(engine);
    }
}

unsafe extern "C" fn collect(event: *const HkEvent, user: *mut c_void) {
    let events = &*(user as *const Mutex<Vec<(HkEventKind, String)>>);
    let event = &*event;
    let text = if event.text.is_null() {
        String::new()
    } else {
        CStr::from_ptr(event.text).to_string_lossy().into_owned()
    };
    events.lock().unwrap().push((event.kind, text));
}

#[test]
fn records_from_a_wav_and_reports_events_in_order() {
    let (model, speech) = fixtures_or_skip!();
    unsafe {
        let engine = engine(&model);
        let mut cfg: HkRecordConfig = std::mem::zeroed();
        cfg.size = size_of::<HkRecordConfig>();
        hk_record_config_init(&mut cfg);
        cfg.input_wav = speech.as_ptr();
        cfg.input_realtime = false;
        let events: Box<Mutex<Vec<(HkEventKind, String)>>> = Box::default();
        let user = &*events as *const _ as *mut c_void;
        let session = hk_record_start(engine, &cfg, Some(collect), user);
        assert!(!session.is_null(), "{}", last_error());
        std::thread::sleep(Duration::from_millis(500));
        let mut text = ptr::null_mut();
        assert_eq!(hk_record_stop(session, &mut text), 0, "{}", last_error());
        let text = take(text);
        assert!(text.to_lowercase().contains("country"), "{text:?}");
        let events = events.lock().unwrap();
        let kinds: Vec<HkEventKind> = events.iter().map(|e| e.0).collect();
        assert_eq!(
            &kinds[kinds.len() - 2..],
            &[HkEventKind::Final, HkEventKind::Stopped]
        );
        assert_eq!(events[events.len() - 2].1, text);
        hk_engine_free(engine);
    }
}

#[test]
fn listens_to_a_wav_until_it_ends_and_reports_utterances() {
    let (model, speech) = fixtures_or_skip!();
    let Ok(vad) = std::env::var("HUSHKEY_TEST_VAD") else {
        eprintln!("skipped: set HUSHKEY_TEST_VAD");
        return;
    };
    unsafe {
        let mut cfg: HkEngineConfig = std::mem::zeroed();
        cfg.size = size_of::<HkEngineConfig>();
        hk_engine_config_init(&mut cfg);
        cfg.model = model.as_ptr();
        cfg.language = c"en".as_ptr();
        let vad = CString::new(vad).unwrap();
        cfg.vad_model = vad.as_ptr();
        let engine = hk_engine_new(&cfg);
        assert!(!engine.is_null(), "{}", last_error());

        let mut listen: HkListenConfig = std::mem::zeroed();
        listen.size = size_of::<HkListenConfig>();
        hk_listen_config_init(&mut listen);
        assert_eq!(listen.end_silence_ms, 512); // 16 frames of 32 ms
        listen.input_wav = speech.as_ptr();
        listen.input_realtime = false;
        let events: Box<Mutex<Vec<(HkEventKind, String)>>> = Box::default();
        let user = &*events as *const _ as *mut c_void;
        let session = hk_listen_start(engine, &listen, Some(collect), user);
        assert!(!session.is_null(), "{}", last_error());
        // The file ends the session by itself; wait for STOPPED, then free.
        for _ in 0..300 {
            if events.lock().unwrap().last().map(|e| e.0) == Some(HkEventKind::Stopped) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(hk_listen_stop(session), 0, "{}", last_error());
        let events = events.lock().unwrap();
        let text: String = events
            .iter()
            .filter(|e| e.0 == HkEventKind::Utterance)
            .map(|e| e.1.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text.matches("country").count(), 2, "{events:?}");
        assert!(events.iter().any(|e| e.0 == HkEventKind::SpeechStart));
        assert_eq!(events.last().map(|e| e.0), Some(HkEventKind::Stopped));

        // A listening session is not a recording: the mix-up is refused and
        // leaves the session intact for the right stop function.
        let session = hk_listen_start(engine, &listen, None, ptr::null_mut());
        assert_eq!(hk_record_stop(session, ptr::null_mut()), -1);
        assert!(last_error().contains("hk_listen_stop"));
        assert_eq!(hk_listen_stop(session), 0, "{}", last_error());
        hk_engine_free(engine);
    }
}

#[test]
fn listening_without_a_vad_model_is_refused() {
    let (model, _) = fixtures_or_skip!();
    unsafe {
        let engine = engine(&model);
        assert!(hk_listen_start(engine, ptr::null(), None, ptr::null_mut()).is_null());
        assert!(last_error().contains("VAD"));
        hk_engine_free(engine);
    }
}

/// A tiny OpenAI-compatible chat server answering every request with
/// "it is late".
fn chat_server() -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim_end().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert!(String::from_utf8_lossy(&body).contains("test-model"));
            let reply = r#"{"choices":[{"message":{"role":"assistant","content":"it is late"}}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    url
}

unsafe extern "C" fn collect_commands(event: *const HkEvent, user: *mut c_void) {
    let seen = &*(user as *const Mutex<Vec<String>>);
    let e = &*event;
    let text = |p: *const std::ffi::c_char| {
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    let line = match e.kind {
        HkEventKind::Command => {
            let args: Vec<String> = (0..e.n_args).map(|i| text(*e.args.add(i))).collect();
            format!(
                "command {}({}) {:.1}",
                text(e.name),
                args.join(","),
                e.confidence
            )
        }
        HkEventKind::Reply => format!("reply {} <- {}", text(e.text), text(e.utterance)),
        HkEventKind::Error => format!("error {}", text(e.text)),
        HkEventKind::Stopped => "stopped".into(),
        _ => return,
    };
    seen.lock().unwrap().push(line);
}

#[test]
fn commands_and_an_openai_brain_through_the_c_api() {
    let (model, _) = fixtures_or_skip!();
    let Ok(vad) = std::env::var("HUSHKEY_TEST_VAD") else {
        eprintln!("skipped: set HUSHKEY_TEST_VAD");
        return;
    };
    let wav = CString::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../hushkey-core/tests/data/commands-de.wav")
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let url = CString::new(chat_server()).unwrap();
    unsafe {
        let mut cfg: HkEngineConfig = std::mem::zeroed();
        cfg.size = size_of::<HkEngineConfig>();
        hk_engine_config_init(&mut cfg);
        cfg.model = model.as_ptr();
        cfg.language = c"de".as_ptr();
        let vad = CString::new(vad).unwrap();
        cfg.vad_model = vad.as_ptr();
        let engine = hk_engine_new(&cfg);
        assert!(!engine.is_null(), "{}", last_error());

        let mut lis: HkListenConfig = std::mem::zeroed();
        lis.size = size_of::<HkListenConfig>();
        hk_listen_config_init(&mut lis);
        assert_eq!(lis.brain, HkBrainKind::None);
        lis.input_wav = wav.as_ptr();
        lis.input_realtime = false;
        let commands = cr#"{"commands":[{"name":"next","phrases":["weiter"]},{"name":"back","phrases":["zurück"]},{"name":"channel","phrases":["kanal {n}"]}]}"#;
        lis.commands_json = commands.as_ptr();
        lis.brain = HkBrainKind::Openai;
        lis.brain_url = url.as_ptr();
        lis.brain_model = c"test-model".as_ptr();
        let seen: Box<Mutex<Vec<String>>> = Box::default();
        let user = &*seen as *const _ as *mut c_void;
        let session = hk_listen_start(engine, &lis, Some(collect_commands), user);
        assert!(!session.is_null(), "{}", last_error());
        for _ in 0..600 {
            if seen.lock().unwrap().last().map(String::as_str) == Some("stopped") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(hk_listen_stop(session), 0, "{}", last_error());
        let seen = seen.lock().unwrap();
        assert_eq!(
            &seen[..4],
            [
                "command next() 1.0",
                "command channel(6) 1.0",
                "command channel(21) 1.0",
                "command back() 1.0"
            ],
            "{seen:?}"
        );
        assert!(
            seen[4].starts_with("reply it is late <- ") && seen[4].to_lowercase().contains("spät"),
            "{seen:?}"
        );
        assert_eq!(seen.last().map(String::as_str), Some("stopped"));

        // A bad command set or brain is refused up front, with the reason.
        lis.commands_json = c"{\"commands\": [{\"name\": \"x\", \"phrases\": [\"{n}\"]}]}".as_ptr();
        assert!(hk_listen_start(engine, &lis, None, ptr::null_mut()).is_null());
        assert!(last_error().contains("no words"), "{}", last_error());
        lis.commands_json = ptr::null();
        lis.brain_model = ptr::null();
        assert!(hk_listen_start(engine, &lis, None, ptr::null_mut()).is_null());
        assert!(last_error().contains("brain_model"), "{}", last_error());
        hk_engine_free(engine);
    }
}
