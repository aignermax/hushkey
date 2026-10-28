//! A loaded whisper.cpp model and the transcription call.

use std::path::PathBuf;
use std::sync::{Mutex, Once};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::audio::{rms, SAMPLE_RATE};
use crate::error::{Error, Result};

/// Below this level a recording is treated as "nothing was said": a muted
/// or unplugged microphone. Matches the Python hushkey.
const SILENT_RMS: f32 = 0.001;
/// whisper.cpp rejects very short input; pad to at least this.
const MIN_SAMPLES: usize = SAMPLE_RATE as usize + SAMPLE_RATE as usize / 10;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// A GGML whisper model file (see [`crate::model`] to resolve names).
    pub model: PathBuf,
    /// Run on a GPU when the build has a GPU backend.
    pub use_gpu: bool,
    /// Which GPU, as the backend numbers them.
    pub gpu_device: i32,
    /// CPU threads; 0 picks one per core, up to 8.
    pub threads: usize,
    /// Default language (`"de"`, `"en"`, ...); `None` detects it per call.
    pub language: Option<String>,
    /// Silero VAD model: silence is masked before transcription, which keeps
    /// whisper from inventing text in pauses. `None` transcribes everything.
    pub vad_model: Option<PathBuf>,
}

impl EngineConfig {
    pub fn new(model: impl Into<PathBuf>) -> Self {
        EngineConfig {
            model: model.into(),
            use_gpu: true,
            gpu_device: 0,
            threads: 0,
            language: None,
            vad_model: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TranscribeOptions {
    /// Overrides the engine's language; `Some("auto")` forces detection.
    pub language: Option<String>,
    /// Text whisper treats as preceding the audio: vocabulary, spelling,
    /// punctuation style.
    pub prompt: Option<String>,
    /// Beam search width; 0 or 1 decodes greedily (faster, for previews).
    pub beam_size: u32,
}

impl TranscribeOptions {
    /// Accurate settings for a finished recording.
    pub fn accurate() -> Self {
        TranscribeOptions {
            beam_size: 5,
            ..Default::default()
        }
    }

    /// Fast settings for a live preview.
    pub fn fast() -> Self {
        TranscribeOptions {
            beam_size: 1,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Segment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<Segment>,
    /// The language whisper decoded in, as an ISO code.
    pub language: Option<String>,
}

/// A loaded model. Thread-safe: calls from several threads are serialized.
pub struct Engine {
    _ctx: WhisperContext,
    state: Mutex<WhisperState>,
    vad: Option<Mutex<WhisperVadContext>>,
    config: EngineConfig,
}

static PROCESS_SETUP: Once = Once::new();

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Engine> {
        PROCESS_SETUP.call_once(|| {
            // whisper.cpp prints its model and backend details to stderr; a
            // library must not write to the host's console unless asked to
            // (HUSHKEY_WHISPER_LOG=1, for diagnosing a backend).
            if std::env::var_os("HUSHKEY_WHISPER_LOG").is_none() {
                whisper_rs::install_logging_hooks();
            }
            // Capture/overlay layers injected into every Vulkan app stalled
            // inference for minutes on an RX 7600 XT (0.05 s without them).
            // The Vulkan loader reads this when whisper.cpp first creates its
            // instance, so it must be set before the first model loads. A host
            // that sets VK_LOADER_LAYERS_DISABLE itself, or opts out with
            // WHISPER_CPP_ALLOW_LAYERS=1 like the Python hushkey, is respected.
            #[cfg(feature = "vulkan")]
            if std::env::var_os("VK_LOADER_LAYERS_DISABLE").is_none()
                && std::env::var("WHISPER_CPP_ALLOW_LAYERS").as_deref() != Ok("1")
            {
                std::env::set_var("VK_LOADER_LAYERS_DISABLE", "~implicit~");
            }
        });
        let model = config.model.to_str().ok_or_else(|| {
            Error::InvalidArgument(format!(
                "model path is not UTF-8: {}",
                config.model.display()
            ))
        })?;
        if !config.model.is_file() {
            return Err(Error::ModelNotFound(config.model.display().to_string()));
        }
        let mut params = WhisperContextParameters::default();
        params.use_gpu(config.use_gpu).gpu_device(config.gpu_device);
        let load_error = |reason: String| Error::ModelLoad {
            path: config.model.clone(),
            reason,
        };
        let ctx = WhisperContext::new_with_params(model, params)
            .map_err(|e| load_error(e.to_string()))?;
        let state = ctx.create_state().map_err(|e| load_error(e.to_string()))?;
        let vad = match &config.vad_model {
            None => None,
            Some(path) => {
                let path_str = path.to_str().ok_or_else(|| {
                    Error::InvalidArgument(format!(
                        "VAD model path is not UTF-8: {}",
                        path.display()
                    ))
                })?;
                if !path.is_file() {
                    return Err(Error::ModelNotFound(path.display().to_string()));
                }
                let mut vad_params = WhisperVadContextParams::new();
                vad_params.set_n_threads(threads(config.threads) as i32);
                // Silero is tiny; the CPU is faster than a GPU round trip.
                vad_params.set_use_gpu(false);
                let ctx =
                    WhisperVadContext::new(path_str, vad_params).map_err(|e| Error::ModelLoad {
                        path: path.clone(),
                        reason: e.to_string(),
                    })?;
                Some(Mutex::new(ctx))
            }
        };
        Ok(Engine {
            _ctx: ctx,
            state: Mutex::new(state),
            vad,
            config,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Whether a VAD model is loaded (needed for continuous listening).
    pub fn has_vad(&self) -> bool {
        self.vad.is_some()
    }

    /// Speech regions in `audio` as sample ranges, or `None` without a VAD
    /// model.
    pub fn speech_regions(&self, audio: &[f32]) -> Result<Option<Vec<(usize, usize)>>> {
        let Some(vad) = &self.vad else {
            return Ok(None);
        };
        if audio.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let mut vad = vad.lock().unwrap_or_else(|p| p.into_inner());
        let segments = vad
            .segments_from_samples(WhisperVadParams::default(), audio)
            .map_err(|e| Error::Transcribe(format!("voice activity detection: {e}")))?;
        let per_cs = SAMPLE_RATE as f32 / 100.0; // timestamps are in centiseconds
        Ok(Some(
            segments
                .map(|s| {
                    let start = ((s.start * per_cs) as usize).min(audio.len());
                    let end = ((s.end * per_cs) as usize).min(audio.len());
                    (start, end.max(start))
                })
                .collect(),
        ))
    }

    /// Transcribe 16 kHz mono audio.
    pub fn transcribe(&self, audio: &[f32], options: &TranscribeOptions) -> Result<Transcript> {
        if audio.is_empty() || rms(audio) < SILENT_RMS {
            return Ok(Transcript::default());
        }
        let mut input = match self.speech_regions(audio)? {
            None => audio.to_vec(),
            Some(regions) if regions.is_empty() => return Ok(Transcript::default()),
            Some(regions) => {
                // Mask rather than cut: timestamps stay on the recording's
                // timeline. Trailing silence is dropped, since decoding a
                // silent tail is where whisper invents text.
                let mut masked = vec![0.0f32; regions.last().map(|r| r.1).unwrap_or(0)];
                for &(start, end) in &regions {
                    masked[start..end].copy_from_slice(&audio[start..end]);
                }
                masked
            }
        };
        if input.len() < MIN_SAMPLES {
            input.resize(MIN_SAMPLES, 0.0);
        }

        let strategy = if options.beam_size > 1 {
            SamplingStrategy::BeamSearch {
                beam_size: options.beam_size as i32,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        let language = options
            .language
            .as_deref()
            .or(self.config.language.as_deref())
            .unwrap_or("auto");
        params.set_language(Some(language));
        params.set_n_threads(threads(self.config.threads) as i32);
        params.set_no_context(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        if let Some(prompt) = options.prompt.as_deref().filter(|p| !p.is_empty()) {
            params.set_initial_prompt(prompt);
        }

        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state
            .full(params, &input)
            .map_err(|e| Error::Transcribe(e.to_string()))?;
        let duration_ms = (audio.len() as i64 * 1000) / SAMPLE_RATE as i64;
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|e| Error::Transcribe(e.to_string()))?;
            let start_ms = segment.start_timestamp() * 10;
            if start_ms >= duration_ms {
                continue; // decoded from the padding
            }
            segments.push(Segment {
                start_ms,
                end_ms: (segment.end_timestamp() * 10).min(duration_ms),
                text: text.trim().to_string(),
            });
        }
        segments.retain(|s| !s.text.is_empty());
        let text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let language =
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        Ok(Transcript {
            text,
            segments,
            language,
        })
    }
}

fn threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4)
}
