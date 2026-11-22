//! Resolve the saved local preference on the loader thread. Only an automatic
//! choice can change models after a Whisper initialization failure.

use std::path::Path;

use hushkey_core::{model, recommend_hardware, Engine, EngineConfig, Error, Result};

use super::gpu_crash;
use crate::{config::AUTO_MODEL, log};

pub(super) fn load(
    preference: &str,
    allow_gpu: bool,
    loading: impl Fn(&'static str),
) -> Result<(&str, Engine)> {
    if preference != AUTO_MODEL {
        let config = prepare(preference, allow_gpu, 0)?;
        return Ok((preference, open(config)?));
    }

    let choice = recommend_hardware(allow_gpu);
    log::line(&format!(
        "automatic model {}: {}",
        choice.model, choice.evidence
    ));
    loading(choice.model);
    let mut config = prepare(choice.model, choice.use_gpu, choice.gpu_device)?;
    // When the GPU cannot take it, the fallback below is a model chosen for
    // the CPU, not this one loaded on the CPU first.
    config.cpu_fallback = false;
    let whisper_file = config.model.clone();
    let first = match open(config) {
        Ok(engine) if !choice.use_gpu || engine.on_gpu() => return Ok((choice.model, engine)),
        Ok(engine) => {
            log::line(&format!("GPU choice ran on {:?}", engine.acceleration()));
            Ok(engine)
        }
        Err(e) if whisper_init_failed(&e, &whisper_file) => {
            log::line(&format!("automatic model {} failed: {e}", choice.model));
            Err(e)
        }
        Err(e) => return Err(e),
    };
    let Some(fallback) = cpu_retry(choice.model, choice.use_gpu, first.is_ok(), || {
        recommend_hardware(false).model
    }) else {
        return first.map(|engine| (choice.model, engine));
    };
    // Release the first context and its buffers before allocating another.
    drop(first);
    log::line(&format!(
        "automatic model falling back to {fallback} on CPU"
    ));
    loading(fallback);
    Ok((fallback, open(prepare(fallback, false, 0)?)?))
}

/// Load an engine; on a GPU, also build its pipelines. Both can abort the
/// process, so a GPU load is marked until it ends (see [`gpu_crash`]).
fn open(config: EngineConfig) -> Result<Engine> {
    let marked = config
        .cpu_reason()
        .is_none()
        .then(|| gpu_crash::marker().hold());
    let engine = Engine::new(config)?;
    if engine.on_gpu() {
        super::warm_up(&engine);
        if let Some(marked) = marked {
            marked.succeeded();
        }
    }
    Ok(engine)
}

fn cpu_retry(
    model: &str,
    requested_gpu: bool,
    initialized: bool,
    recommended_cpu_model: impl FnOnce() -> &'static str,
) -> Option<&'static str> {
    if requested_gpu {
        let cpu_model = recommended_cpu_model();
        // Whisper can initialize successfully while choosing the CPU itself.
        return (cpu_model != model || !initialized).then_some(cpu_model);
    }
    (model != "tiny").then_some("tiny")
}

fn prepare(model: &str, use_gpu: bool, gpu_device: i32) -> Result<EngineConfig> {
    let mut config = EngineConfig::new(model::ensure_whisper_model(model)?);
    config.use_gpu = use_gpu;
    config.gpu_device = gpu_device;
    config.vad_model = Some(model::ensure_vad_model()?);
    Ok(config)
}

/// A broken VAD file is shared by every model, so switching models cannot help.
fn whisper_init_failed(error: &Error, whisper_file: &Path) -> bool {
    matches!(error, Error::ModelLoad { path, .. } if path == whisper_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_fallback_reuses_a_working_cpu_model_or_retries_once() {
        assert_eq!(cpu_retry("base", true, true, || "base"), None);
        assert_eq!(cpu_retry("base", true, false, || "base"), Some("base"));
        assert_eq!(cpu_retry("small", true, true, || "base"), Some("base"));
    }

    #[test]
    fn failed_cpu_choice_can_step_down_to_tiny() {
        assert_eq!(
            cpu_retry("base", false, false, || unreachable!()),
            Some("tiny")
        );
        assert_eq!(cpu_retry("tiny", false, false, || unreachable!()), None);
    }

    #[test]
    fn manual_model_failure_preserves_the_requested_name() {
        let invalid_name = "invalid model name!";
        let error = load(invalid_name, true, |_| panic!("manual model changed"))
            .err()
            .expect("invalid manual model must fail");
        assert!(error.to_string().contains(invalid_name));
    }

    #[test]
    fn only_whisper_initialization_errors_can_trigger_another_model_load() {
        let whisper = Path::new("ggml-large-v3-turbo.bin");
        let load = |path: &Path| Error::ModelLoad {
            path: path.into(),
            reason: "failed".into(),
        };
        assert!(whisper_init_failed(&load(whisper), whisper));
        assert!(!whisper_init_failed(
            &load(Path::new("silero.bin")),
            whisper
        ));
        assert!(!whisper_init_failed(
            &Error::ModelNotFound("download failed".into()),
            whisper
        ));
    }
}
