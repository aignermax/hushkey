//! hushkey's C ABI. The header is `include/hushkey.h`, generated from this
//! file by cbindgen; a test keeps the two in sync.
//!
//! Conventions:
//! - Every config and event struct starts with `size`. A caller sets it to
//!   `sizeof` the struct it was compiled with; the library reads only fields
//!   that fit, so structs can grow without breaking older callers.
//! - Functions that fail return NULL or a negative number; `hk_last_error`
//!   then describes the failure (per thread).
//! - Strings in are UTF-8 and NUL-terminated. Strings the library returns are
//!   freed with `hk_string_free`. Strings inside an event are valid only
//!   during the callback.
//! - Callbacks run on a library thread, one event at a time, in order.
//!   Calling `hk_record_stop` or `hk_listen_stop` from inside a callback is
//!   allowed.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::{offset_of, size_of};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use hushkey_core::{
    endpoint, model, read_wav_16k, AudioInput, Engine, EngineConfig, Event, EventSink,
    ListenConfig, ListenSession, Microphone, RecordConfig, RecordSession, TranscribeOptions,
    WavReplay, VAD_FRAME,
};

/// Version of this ABI. Additions keep the number; incompatible changes
/// raise it.
pub const HK_ABI_VERSION: u32 = 1;

/// Settings for `hk_engine_new`. Initialize with `hk_engine_config_init`.
#[repr(C)]
pub struct HkEngineConfig {
    /// `sizeof(HkEngineConfig)` as the caller knows it.
    pub size: usize,
    /// A GGML model file, or a model name (`"tiny"`, `"small"`,
    /// `"large-v3-turbo"`, ...) looked up in the Hugging Face cache.
    pub model: *const c_char,
    /// Run on the GPU when the library was built with a GPU backend.
    pub use_gpu: bool,
    /// Which GPU, as the backend numbers them.
    pub gpu_device: i32,
    /// CPU threads; 0 picks one per core, up to 8.
    pub threads: u32,
    /// Default language (`"de"`, `"en"`, ...); NULL detects it.
    pub language: *const c_char,
    /// Silero VAD model file, or `"silero"` for the cached one; NULL for
    /// none. With it, pauses are masked before transcription.
    pub vad_model: *const c_char,
}

/// Settings for `hk_record_start`. Initialize with `hk_record_config_init`.
#[repr(C)]
pub struct HkRecordConfig {
    /// `sizeof(HkRecordConfig)` as the caller knows it.
    pub size: usize,
    /// Emit `HK_EVENT_KIND_PREVIEW` events while recording.
    pub preview: bool,
    /// Milliseconds between previews.
    pub preview_interval_ms: u32,
    /// How much of the recording's end a preview covers, in milliseconds.
    pub preview_window_ms: u32,
    /// Overrides the engine's language; `"auto"` forces detection.
    pub language: *const c_char,
    /// Vocabulary or style hint passed to whisper.
    pub prompt: *const c_char,
    /// Input device name; NULL for the default microphone.
    pub input_device: *const c_char,
    /// Record from this WAV file instead of a microphone.
    pub input_wav: *const c_char,
    /// With `input_wav`: play it at real-time speed like a microphone.
    pub input_realtime: bool,
}

/// Settings for `hk_listen_start`. Initialize with `hk_listen_config_init`.
#[repr(C)]
pub struct HkListenConfig {
    /// `sizeof(HkListenConfig)` as the caller knows it.
    pub size: usize,
    /// A pause this long (ms) ends an utterance. Shorter reacts faster to
    /// commands; longer lets people pause mid-sentence.
    pub end_silence_ms: u32,
    /// Speech must last this long (ms) to count (filters clicks, breaths).
    pub min_speech_ms: u32,
    /// An utterance is cut after this long (ms) even without a pause.
    pub max_utterance_ms: u32,
    /// Speech probability threshold, between 0 and 1.
    pub vad_threshold: f32,
    /// Emit `HK_EVENT_KIND_PREVIEW` events while an utterance is spoken.
    pub preview: bool,
    /// Milliseconds between previews.
    pub preview_interval_ms: u32,
    /// Overrides the engine's language; `"auto"` forces detection.
    pub language: *const c_char,
    /// Vocabulary or style hint passed to whisper (command words, names).
    pub prompt: *const c_char,
    /// Input device name; NULL for the default microphone.
    pub input_device: *const c_char,
    /// Listen to this WAV file instead of a microphone; the session ends by
    /// itself when the file does.
    pub input_wav: *const c_char,
    /// With `input_wav`: play it at real-time speed like a microphone.
    pub input_realtime: bool,
}

/// What an event reports. Newer library versions may add kinds; ignore any
/// you do not know.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HkEventKind {
    /// `text`: the recording so far, transcribed quickly.
    Preview = 1,
    /// `text`, `language`, `end_ms`: the whole recording, after stop.
    Final = 2,
    /// Listening: someone started speaking at `start_ms`.
    SpeechStart = 3,
    /// Listening: `text`, `language`, `start_ms`, `end_ms` of one finished
    /// utterance.
    Utterance = 4,
    /// `text`: what went wrong.
    Error = 7,
    /// The session is over; nothing follows.
    Stopped = 8,
}

/// One event. Pointers are valid only during the callback.
#[repr(C)]
pub struct HkEvent {
    /// `sizeof(HkEvent)` of the library.
    pub size: usize,
    pub kind: HkEventKind,
    /// Transcribed text or an error message; NULL when the kind has none.
    pub text: *const c_char,
    /// The detected language (ISO code), or NULL.
    pub language: *const c_char,
    /// Start of the reported audio, in milliseconds from the session start.
    pub start_ms: i64,
    /// End of the reported audio, in milliseconds from the session start.
    pub end_ms: i64,
}

/// Receives events. `event` and everything it points to are valid only
/// during the call.
pub type HkEventCallback =
    Option<unsafe extern "C" fn(event: *const HkEvent, user_data: *mut c_void)>;

/// A loaded model. Thread-safe.
pub struct HkEngine {
    engine: Arc<Engine>,
}

/// A recording or listening session in progress.
pub enum HkSession {
    Record(RecordSession),
    Listen(ListenSession),
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(message: &str) {
    let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(message));
}

/// Run `f`, turning errors and panics into `fallback` plus `hk_last_error`.
fn guard<T>(fallback: T, f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => value,
        Ok(Err(message)) => {
            set_error(&message);
            fallback
        }
        Err(_) => {
            set_error("internal error in hushkey");
            fallback
        }
    }
}

/// A C string argument; NULL is `None`.
unsafe fn opt_str(p: *const c_char, what: &str) -> Result<Option<String>, String> {
    if p.is_null() {
        return Ok(None);
    }
    CStr::from_ptr(p)
        .to_str()
        .map(|s| Some(s.to_string()))
        .map_err(|_| format!("{what} is not valid UTF-8"))
}

fn into_c_string(s: &str) -> *mut c_char {
    CString::new(s.replace('\0', " "))
        .map(CString::into_raw)
        .unwrap_or(ptr::null_mut())
}

/// The size of a struct field, without touching memory.
const fn field_size<S, F>(_: fn(&S) -> &F) -> usize {
    size_of::<F>()
}

/// Read `$field` of `*$cfg` if the caller's struct is large enough to hold
/// it, else `$default`.
macro_rules! read_field {
    ($cfg:expr, $ty:ty, $field:ident, $default:expr) => {{
        let end = offset_of!($ty, $field) + field_size(|c: &$ty| &c.$field);
        if (*$cfg).size >= end {
            ptr::addr_of!((*$cfg).$field).read_unaligned()
        } else {
            $default
        }
    }};
}

/// [`read_field!`] for a `bool` field: reads the byte and tests it, since a
/// host (C# marshaling, uninitialized memory) may store values other than
/// 0 and 1, which are not valid Rust `bool`s.
macro_rules! read_bool {
    ($cfg:expr, $ty:ty, $field:ident, $default:expr) => {{
        let end = offset_of!($ty, $field) + field_size(|c: &$ty| &c.$field);
        if (*$cfg).size >= end {
            ptr::addr_of!((*$cfg).$field).cast::<u8>().read_unaligned() != 0
        } else {
            $default
        }
    }};
}

/// Write `$value` to `$field` of `*$cfg` if the caller's struct holds it.
macro_rules! write_field {
    ($cfg:expr, $ty:ty, $field:ident, $value:expr) => {{
        let end = offset_of!($ty, $field) + field_size(|c: &$ty| &c.$field);
        if (*$cfg).size >= end {
            ptr::addr_of_mut!((*$cfg).$field).write_unaligned($value);
        }
    }};
}

/// Prepare the process for hushkey. Call it once at program start, before
/// other threads run and before the first `hk_engine_new`. On a Vulkan build
/// it sets `VK_LOADER_LAYERS_DISABLE=~implicit~` (unless already set, or
/// `WHISPER_CPP_ALLOW_LAYERS=1`): capture and overlay layers that inject
/// themselves into every Vulkan program stalled inference for minutes on an
/// AMD GPU. Hosts that cannot call it early can set the variable in their
/// environment instead. Safe to skip on CPU, Metal and CUDA builds.
///
/// # Safety
/// No other thread may read or write the environment during the call.
#[no_mangle]
pub unsafe extern "C" fn hk_process_init() {
    hushkey_core::init_process();
}

/// The ABI version the library implements (`HK_ABI_VERSION`).
#[no_mangle]
pub extern "C" fn hk_abi_version() -> u32 {
    HK_ABI_VERSION
}

/// The library version, e.g. `"0.1.0"`. Static; do not free.
#[no_mangle]
pub extern "C" fn hk_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// What the last failing call on this thread reported, or NULL. Valid until
/// the next hushkey call on this thread.
#[no_mangle]
pub extern "C" fn hk_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
}

/// Free a string returned by the library. NULL is ignored.
///
/// # Safety
/// `s` must come from this library and not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn hk_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Fill `cfg` with defaults. Set `cfg->size = sizeof *cfg` first.
///
/// # Safety
/// `cfg` must point to a writable struct of at least `cfg->size` bytes.
#[no_mangle]
pub unsafe extern "C" fn hk_engine_config_init(cfg: *mut HkEngineConfig) {
    if cfg.is_null() {
        return;
    }
    write_field!(cfg, HkEngineConfig, model, ptr::null());
    write_field!(cfg, HkEngineConfig, use_gpu, true);
    write_field!(cfg, HkEngineConfig, gpu_device, 0);
    write_field!(cfg, HkEngineConfig, threads, 0);
    write_field!(cfg, HkEngineConfig, language, ptr::null());
    write_field!(cfg, HkEngineConfig, vad_model, ptr::null());
}

/// Load a model. Returns NULL on failure (see `hk_last_error`).
///
/// # Safety
/// `cfg` must point to an initialized `HkEngineConfig`.
#[no_mangle]
pub unsafe extern "C" fn hk_engine_new(cfg: *const HkEngineConfig) -> *mut HkEngine {
    guard(ptr::null_mut(), || {
        if cfg.is_null() {
            return Err("cfg is NULL".into());
        }
        let model_arg = opt_str(
            read_field!(cfg, HkEngineConfig, model, ptr::null()),
            "model",
        )?
        .ok_or("model is required")?;
        let mut config = EngineConfig::new(resolve_model(&model_arg)?);
        config.use_gpu = read_bool!(cfg, HkEngineConfig, use_gpu, true);
        config.gpu_device = read_field!(cfg, HkEngineConfig, gpu_device, 0);
        config.threads = read_field!(cfg, HkEngineConfig, threads, 0) as usize;
        config.language = opt_str(
            read_field!(cfg, HkEngineConfig, language, ptr::null()),
            "language",
        )?;
        config.vad_model = match opt_str(
            read_field!(cfg, HkEngineConfig, vad_model, ptr::null()),
            "vad_model",
        )? {
            None => None,
            Some(name) if name == "silero" => Some(resolve_vad()?),
            Some(path) => Some(PathBuf::from(path)),
        };
        let engine = Engine::new(config).map_err(|e| e.to_string())?;
        Ok(Box::into_raw(Box::new(HkEngine {
            engine: Arc::new(engine),
        })))
    })
}

#[cfg(feature = "download")]
fn resolve_model(name: &str) -> Result<PathBuf, String> {
    model::ensure_whisper_model(name).map_err(|e| e.to_string())
}

#[cfg(not(feature = "download"))]
fn resolve_model(name: &str) -> Result<PathBuf, String> {
    model::resolve_whisper_model(name).map_err(|e| e.to_string())
}

#[cfg(feature = "download")]
fn resolve_vad() -> Result<PathBuf, String> {
    model::ensure_vad_model().map_err(|e| e.to_string())
}

#[cfg(not(feature = "download"))]
fn resolve_vad() -> Result<PathBuf, String> {
    model::resolve_vad_model().map_err(|e| e.to_string())
}

/// Release a model. Sessions still running keep it loaded until they end.
///
/// # Safety
/// `engine` must come from `hk_engine_new` and not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn hk_engine_free(engine: *mut HkEngine) {
    if !engine.is_null() {
        drop(Box::from_raw(engine));
    }
}

/// Transcribe a WAV file (any rate, channels, format). Returns the text, to
/// be freed with `hk_string_free`, or NULL on failure.
///
/// # Safety
/// `engine` must be valid; `path` and `language` NUL-terminated or NULL.
#[no_mangle]
pub unsafe extern "C" fn hk_transcribe_file(
    engine: *mut HkEngine,
    path: *const c_char,
    language: *const c_char,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        let engine = engine.as_ref().ok_or("engine is NULL")?;
        let path = opt_str(path, "path")?.ok_or("path is required")?;
        let audio = read_wav_16k(path.as_ref()).map_err(|e| e.to_string())?;
        transcribe(engine, &audio, language)
    })
}

/// Transcribe 16 kHz mono float samples in [-1, 1]. Returns the text, to be
/// freed with `hk_string_free`, or NULL on failure.
///
/// # Safety
/// `engine` must be valid; `samples` must hold `n_samples` floats.
#[no_mangle]
pub unsafe extern "C" fn hk_transcribe_pcm(
    engine: *mut HkEngine,
    samples: *const f32,
    n_samples: usize,
    language: *const c_char,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        let engine = engine.as_ref().ok_or("engine is NULL")?;
        let audio = if n_samples == 0 {
            &[][..]
        } else if samples.is_null() {
            return Err("samples is NULL".into());
        } else {
            std::slice::from_raw_parts(samples, n_samples)
        };
        transcribe(engine, audio, language)
    })
}

unsafe fn transcribe(
    engine: &HkEngine,
    audio: &[f32],
    language: *const c_char,
) -> Result<*mut c_char, String> {
    let options = TranscribeOptions {
        language: opt_str(language, "language")?,
        ..TranscribeOptions::accurate()
    };
    let transcript = engine
        .engine
        .transcribe(audio, &options)
        .map_err(|e| e.to_string())?;
    Ok(into_c_string(&transcript.text))
}

/// Fill `cfg` with defaults. Set `cfg->size = sizeof *cfg` first.
///
/// # Safety
/// `cfg` must point to a writable struct of at least `cfg->size` bytes.
#[no_mangle]
pub unsafe extern "C" fn hk_record_config_init(cfg: *mut HkRecordConfig) {
    if cfg.is_null() {
        return;
    }
    let defaults = RecordConfig::default();
    write_field!(cfg, HkRecordConfig, preview, defaults.preview);
    write_field!(
        cfg,
        HkRecordConfig,
        preview_interval_ms,
        defaults.preview_interval.as_millis() as u32
    );
    write_field!(
        cfg,
        HkRecordConfig,
        preview_window_ms,
        defaults.preview_window.as_millis() as u32
    );
    write_field!(cfg, HkRecordConfig, language, ptr::null());
    write_field!(cfg, HkRecordConfig, prompt, ptr::null());
    write_field!(cfg, HkRecordConfig, input_device, ptr::null());
    write_field!(cfg, HkRecordConfig, input_wav, ptr::null());
    write_field!(cfg, HkRecordConfig, input_realtime, true);
}

/// Start recording. Events go to `callback` (may be NULL) with `user_data`.
/// Returns NULL on failure.
///
/// # Safety
/// `engine` must be valid; `cfg` initialized or NULL for defaults;
/// `user_data` must stay valid until the `HK_EVENT_KIND_STOPPED` event.
#[no_mangle]
pub unsafe extern "C" fn hk_record_start(
    engine: *mut HkEngine,
    cfg: *const HkRecordConfig,
    callback: HkEventCallback,
    user_data: *mut c_void,
) -> *mut HkSession {
    guard(ptr::null_mut(), || {
        let engine = engine.as_ref().ok_or("engine is NULL")?;
        let mut config = RecordConfig::default();
        let mut input: Box<dyn AudioInput> = Box::new(Microphone::new(None));
        if !cfg.is_null() {
            let defaults = RecordConfig::default();
            config.preview = read_bool!(cfg, HkRecordConfig, preview, defaults.preview);
            let interval = read_field!(cfg, HkRecordConfig, preview_interval_ms, 0);
            if interval > 0 {
                config.preview_interval = Duration::from_millis(interval as u64);
            }
            let window = read_field!(cfg, HkRecordConfig, preview_window_ms, 0);
            if window > 0 {
                config.preview_window = Duration::from_millis(window as u64);
            }
            config.options.language = opt_str(
                read_field!(cfg, HkRecordConfig, language, ptr::null()),
                "language",
            )?;
            config.options.prompt = opt_str(
                read_field!(cfg, HkRecordConfig, prompt, ptr::null()),
                "prompt",
            )?;
            let device = opt_str(
                read_field!(cfg, HkRecordConfig, input_device, ptr::null()),
                "input_device",
            )?;
            let wav = opt_str(
                read_field!(cfg, HkRecordConfig, input_wav, ptr::null()),
                "input_wav",
            )?;
            let realtime = read_bool!(cfg, HkRecordConfig, input_realtime, true);
            input = match wav {
                Some(path) => Box::new(WavReplay::new(path, realtime)),
                None => Box::new(Microphone::new(device)),
            };
        }
        let sink = event_sink(callback, user_data);
        let record = RecordSession::start(engine.engine.clone(), input, config, sink)
            .map_err(|e| e.to_string())?;
        Ok(Box::into_raw(Box::new(HkSession::Record(record))))
    })
}

/// Stop recording, transcribe it, and free the session. Emits
/// `HK_EVENT_KIND_FINAL` and `HK_EVENT_KIND_STOPPED` before returning. If
/// `text_out` is not NULL it receives the final text (free it with
/// `hk_string_free`), or NULL.
///
/// Called from inside the session's callback, it returns at once; the final
/// events follow after the callback returns, and `*text_out` is NULL.
/// Returns 0, or -1 on failure.
///
/// # Safety
/// `session` must come from `hk_record_start` and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn hk_record_stop(
    session: *mut HkSession,
    text_out: *mut *mut c_char,
) -> c_int {
    if !text_out.is_null() {
        *text_out = ptr::null_mut();
    }
    guard(-1, || {
        if session.is_null() {
            return Err("session is NULL".into());
        }
        // Check the kind before taking ownership: on a mix-up the session
        // must stay intact for the right stop function.
        if !matches!(&*session, HkSession::Record(_)) {
            return Err("not a recording session; use hk_listen_stop".into());
        }
        let mut session = Box::from_raw(session);
        let HkSession::Record(record) = &mut *session else {
            unreachable!()
        };
        let transcript = record.stop().map_err(|e| e.to_string())?;
        if let (Some(t), false) = (transcript, text_out.is_null()) {
            *text_out = into_c_string(&t.text);
        }
        Ok(0)
    })
}

/// Fill `cfg` with defaults. Set `cfg->size = sizeof *cfg` first.
///
/// # Safety
/// `cfg` must point to a writable struct of at least `cfg->size` bytes.
#[no_mangle]
pub unsafe extern "C" fn hk_listen_config_init(cfg: *mut HkListenConfig) {
    if cfg.is_null() {
        return;
    }
    let d = ListenConfig::default();
    let to_ms = |frames: usize| (frames * VAD_FRAME * 1000 / 16_000) as u32;
    write_field!(
        cfg,
        HkListenConfig,
        end_silence_ms,
        to_ms(d.endpoint.end_silence_frames)
    );
    write_field!(
        cfg,
        HkListenConfig,
        min_speech_ms,
        to_ms(d.endpoint.min_speech_frames)
    );
    write_field!(
        cfg,
        HkListenConfig,
        max_utterance_ms,
        to_ms(d.endpoint.max_utterance_frames)
    );
    write_field!(cfg, HkListenConfig, vad_threshold, d.endpoint.threshold);
    write_field!(cfg, HkListenConfig, preview, d.preview);
    write_field!(
        cfg,
        HkListenConfig,
        preview_interval_ms,
        d.preview_interval.as_millis() as u32
    );
    write_field!(cfg, HkListenConfig, language, ptr::null());
    write_field!(cfg, HkListenConfig, prompt, ptr::null());
    write_field!(cfg, HkListenConfig, input_device, ptr::null());
    write_field!(cfg, HkListenConfig, input_wav, ptr::null());
    write_field!(cfg, HkListenConfig, input_realtime, true);
}

/// Start listening continuously: each utterance arrives as
/// `HK_EVENT_KIND_UTTERANCE`. The engine needs a VAD model
/// (`HkEngineConfig.vad_model`). Returns NULL on failure.
///
/// # Safety
/// `engine` must be valid; `cfg` initialized or NULL for defaults;
/// `user_data` must stay valid until the `HK_EVENT_KIND_STOPPED` event.
#[no_mangle]
pub unsafe extern "C" fn hk_listen_start(
    engine: *mut HkEngine,
    cfg: *const HkListenConfig,
    callback: HkEventCallback,
    user_data: *mut c_void,
) -> *mut HkSession {
    guard(ptr::null_mut(), || {
        let engine = engine.as_ref().ok_or("engine is NULL")?;
        let mut config = ListenConfig::default();
        let mut input: Box<dyn AudioInput> = Box::new(Microphone::new(None));
        if !cfg.is_null() {
            let d = ListenConfig::default();
            let frames_or = |ms: u32, default: usize| {
                if ms > 0 {
                    endpoint::frames(ms)
                } else {
                    default
                }
            };
            let e = &mut config.endpoint;
            e.end_silence_frames = frames_or(
                read_field!(cfg, HkListenConfig, end_silence_ms, 0),
                d.endpoint.end_silence_frames,
            );
            e.min_speech_frames = frames_or(
                read_field!(cfg, HkListenConfig, min_speech_ms, 0),
                d.endpoint.min_speech_frames,
            );
            e.max_utterance_frames = frames_or(
                read_field!(cfg, HkListenConfig, max_utterance_ms, 0),
                d.endpoint.max_utterance_frames,
            );
            let threshold = read_field!(cfg, HkListenConfig, vad_threshold, d.endpoint.threshold);
            if !(threshold > 0.0 && threshold < 1.0) {
                return Err(format!(
                    "vad_threshold must be between 0 and 1, not {threshold}"
                ));
            }
            e.threshold = threshold;
            config.preview = read_bool!(cfg, HkListenConfig, preview, d.preview);
            let interval = read_field!(cfg, HkListenConfig, preview_interval_ms, 0);
            if interval > 0 {
                config.preview_interval = Duration::from_millis(interval as u64);
            }
            config.options.language = opt_str(
                read_field!(cfg, HkListenConfig, language, ptr::null()),
                "language",
            )?;
            config.options.prompt = opt_str(
                read_field!(cfg, HkListenConfig, prompt, ptr::null()),
                "prompt",
            )?;
            let device = opt_str(
                read_field!(cfg, HkListenConfig, input_device, ptr::null()),
                "input_device",
            )?;
            let wav = opt_str(
                read_field!(cfg, HkListenConfig, input_wav, ptr::null()),
                "input_wav",
            )?;
            let realtime = read_bool!(cfg, HkListenConfig, input_realtime, true);
            input = match wav {
                Some(path) => Box::new(WavReplay::new(path, realtime)),
                None => Box::new(Microphone::new(device)),
            };
        }
        let sink = event_sink(callback, user_data);
        let listen = ListenSession::start(engine.engine.clone(), input, config, sink)
            .map_err(|e| e.to_string())?;
        Ok(Box::into_raw(Box::new(HkSession::Listen(listen))))
    })
}

/// Stop listening and free the session. An utterance in progress is
/// transcribed and reported first, then `HK_EVENT_KIND_STOPPED`. Called from
/// inside the session's callback, it returns at once and the stop completes
/// after the callback. Also frees a session that already ended by itself
/// (its file ran out). Returns 0, or -1 on failure.
///
/// # Safety
/// `session` must come from `hk_listen_start` and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn hk_listen_stop(session: *mut HkSession) -> c_int {
    guard(-1, || {
        if session.is_null() {
            return Err("session is NULL".into());
        }
        if !matches!(&*session, HkSession::Listen(_)) {
            return Err("not a listening session; use hk_record_stop".into());
        }
        let mut session = Box::from_raw(session);
        let HkSession::Listen(listen) = &mut *session else {
            unreachable!()
        };
        match listen.stop() {
            Ok(()) | Err(hushkey_core::Error::Stopped) => Ok(0),
            Err(e) => Err(e.to_string()),
        }
    })
}

struct UserData(*mut c_void);
// The host promises user_data may be used from the library's thread.
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

fn event_sink(callback: HkEventCallback, user_data: *mut c_void) -> EventSink {
    let user = UserData(user_data);
    Arc::new(move |event: Event| {
        let Some(callback) = callback else { return };
        let user = &user;
        let empty = (None::<String>, None::<String>, 0i64, 0i64);
        let (kind, (text, language, start_ms, end_ms)) = match event {
            Event::Preview { text } => (HkEventKind::Preview, (Some(text), None, 0, 0)),
            Event::Final { transcript } => {
                let end = transcript.segments.last().map_or(0, |s| s.end_ms);
                (
                    HkEventKind::Final,
                    (Some(transcript.text), transcript.language, 0, end),
                )
            }
            Event::SpeechStart { at_ms } => (HkEventKind::SpeechStart, (None, None, at_ms, at_ms)),
            Event::Utterance {
                transcript,
                start_ms,
                end_ms,
            } => (
                HkEventKind::Utterance,
                (Some(transcript.text), transcript.language, start_ms, end_ms),
            ),
            Event::Error { message } => (HkEventKind::Error, (Some(message), None, 0, 0)),
            Event::Stopped => (HkEventKind::Stopped, empty),
        };
        let text = text.map(|t| CString::new(t.replace('\0', " ")).unwrap_or_default());
        let language = language.map(|l| CString::new(l.replace('\0', " ")).unwrap_or_default());
        let event = HkEvent {
            size: size_of::<HkEvent>(),
            kind,
            text: text.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            language: language.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            start_ms,
            end_ms,
        };
        unsafe { callback(&event, user.0) };
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bool_byte_other_than_0_or_1_reads_as_true() {
        for (byte, expected) in [(0u8, false), (1, true), (2, true), (0xFF, true)] {
            let mut cfg: HkRecordConfig = unsafe { std::mem::zeroed() };
            cfg.size = size_of::<HkRecordConfig>();
            unsafe {
                ptr::addr_of_mut!(cfg.preview).cast::<u8>().write(byte);
                assert_eq!(
                    read_bool!(
                        &cfg as *const HkRecordConfig,
                        HkRecordConfig,
                        preview,
                        false
                    ),
                    expected
                );
            }
        }
    }

    #[test]
    fn a_bool_beyond_the_callers_size_gets_the_default() {
        let mut cfg: HkRecordConfig = unsafe { std::mem::zeroed() };
        cfg.size = offset_of!(HkRecordConfig, input_realtime);
        unsafe {
            assert!(read_bool!(
                &cfg as *const HkRecordConfig,
                HkRecordConfig,
                input_realtime,
                true
            ));
        }
    }
}
