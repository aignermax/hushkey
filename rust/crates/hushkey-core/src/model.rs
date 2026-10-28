//! Finding GGML models: a path, or a name in the Hugging Face cache.
//!
//! The Python hushkey downloads its whisper.cpp models through
//! huggingface_hub, so they already sit in the HF cache; a name like
//! `large-v3-turbo` resolves there without a second download.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Repository and file of a named whisper model. Names are letters,
/// digits, `.`, `-` and `_` only, so a name can never point outside the
/// model folders.
pub fn whisper_model_file(name: &str) -> Result<(&'static str, String)> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !valid {
        return Err(Error::ModelNotFound(format!(
            "{name} (neither a file nor a model name)"
        )));
    }
    Ok(("ggerganov/whisper.cpp", format!("ggml-{name}.bin")))
}

/// Repository and file of the Silero voice activity model.
pub const VAD_MODEL: (&str, &str) = ("ggml-org/whisper-vad", "ggml-silero-v5.1.2.bin");

/// A path to an existing file, or a whisper model name (`tiny`, `small`,
/// `large-v3-turbo`, ...) found in the Hugging Face cache or hushkey's own
/// model folder.
pub fn resolve_whisper_model(name_or_path: &str) -> Result<PathBuf> {
    let path = Path::new(name_or_path);
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let (repo, file) = whisper_model_file(name_or_path)?;
    find_cached(repo, &file).ok_or_else(|| Error::ModelNotFound(name_or_path.to_string()))
}

/// The VAD model, from the cache.
pub fn resolve_vad_model() -> Result<PathBuf> {
    find_cached(VAD_MODEL.0, VAD_MODEL.1)
        .ok_or_else(|| Error::ModelNotFound(VAD_MODEL.1.to_string()))
}

/// hushkey's own model folder, used for downloads.
pub fn model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HUSHKEY_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("hushkey")
        .join("models")
}

fn hf_hub_cache() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HF_HUB_CACHE") {
        return Some(PathBuf::from(dir));
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return Some(PathBuf::from(home).join("hub"));
    }
    dirs::home_dir().map(|h| h.join(".cache").join("huggingface").join("hub"))
}

fn find_cached(repo: &str, file: &str) -> Option<PathBuf> {
    let own = model_dir().join(file);
    if own.is_file() {
        return Some(own);
    }
    let snapshots = hf_hub_cache()?
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(snapshots)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(file))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

/// Download `file` from `repo` into [`model_dir`] unless it is already cached.
#[cfg(feature = "download")]
pub fn ensure(repo: &str, file: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::time::Duration;
    if let Some(found) = find_cached(repo, file) {
        return Ok(found);
    }
    let dir = model_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(file);
    // One partial file per download: two processes fetching the same model
    // at once must not interleave their writes into one file.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let partial = dir.join(format!("{file}.{}-{stamp}.part", std::process::id()));
    let failed =
        |reason: String| Error::ModelNotFound(format!("{file}: download failed: {reason}"));
    // No overall limit (a 1.6 GB model takes long on a slow line), but a
    // connection that stops delivering is given up on.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into();
    let url = format!("https://huggingface.co/{repo}/resolve/main/{file}");
    let response = agent.get(&url).call().map_err(|e| failed(e.to_string()))?;
    let expected: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let result = (|| -> Result<()> {
        let mut body = response.into_body().into_reader();
        let mut out = std::fs::File::create(&partial)?;
        let written = std::io::copy(&mut body, &mut out)?;
        out.flush()?;
        if expected.is_some_and(|n| n != written) {
            return Err(failed(format!(
                "got {written} of {} bytes",
                expected.unwrap_or(0)
            )));
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    if target.is_file() {
        // Another process finished the same download first.
        let _ = std::fs::remove_file(&partial);
    } else {
        std::fs::rename(&partial, &target)?;
    }
    Ok(target)
}

/// [`resolve_whisper_model`], downloading a named model when it is missing.
#[cfg(feature = "download")]
pub fn ensure_whisper_model(name_or_path: &str) -> Result<PathBuf> {
    if Path::new(name_or_path).is_file() {
        return Ok(PathBuf::from(name_or_path));
    }
    let (repo, file) = whisper_model_file(name_or_path)?;
    ensure(repo, &file)
}

/// [`resolve_vad_model`], downloading it when it is missing.
#[cfg(feature = "download")]
pub fn ensure_vad_model() -> Result<PathBuf> {
    ensure(VAD_MODEL.0, VAD_MODEL.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_path_wins_over_a_name() {
        let file = std::env::temp_dir().join(format!("hk-model-{}.bin", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve_whisper_model(file.to_str().unwrap()).unwrap(), file);
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn a_name_cannot_point_outside_the_model_folders() {
        for name in ["../../x", "a/b", "a\\b", "..", ".hidden", "", "c:x"] {
            assert!(whisper_model_file(name).is_err(), "{name:?}");
        }
        assert_eq!(
            whisper_model_file("large-v3-turbo").unwrap().1,
            "ggml-large-v3-turbo.bin"
        );
    }

    #[test]
    fn an_unknown_name_is_reported_by_name() {
        let err = resolve_whisper_model("no-such-model-xyz").unwrap_err();
        assert!(err.to_string().contains("no-such-model-xyz"));
    }
}
