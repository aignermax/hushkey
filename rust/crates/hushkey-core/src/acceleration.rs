//! The backend selected by whisper.cpp for a model's inference state.

use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::Write;
use std::sync::OnceLock;

use whisper_rs::whisper_rs_sys as sys;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acceleration {
    Gpu {
        backend: String,
        device: String,
    },
    /// CPU was explicitly selected.
    Cpu,
    /// The selected GPU was absent or failed to initialize.
    CpuFallback,
    /// This build omits the GPU shaders for quantized weights.
    QuantizedCpu,
    /// This build has no GPU backend.
    CpuBuild,
    /// The native library did not identify a backend.
    Unknown,
}

// whisper.cpp exposes no state-backend getter. It does report the selection
// and any initialization failure synchronously during whisper_init_state.
// Scope collection to that state on that thread. Silero also logs a CPU
// initialization and independent engines may be loading on other threads.
thread_local! {
    static OBSERVED: Cell<Option<Acceleration>> = const { Cell::new(None) };
}

static VERBOSE: OnceLock<bool> = OnceLock::new();

pub(crate) fn install_log_hook() {
    VERBOSE.get_or_init(|| {
        // SAFETY: installed once before this crate calls whisper; no borrowed
        // user data. whisper_log_set installs the same callback for ggml.
        unsafe { sys::whisper_log_set(Some(log_message), std::ptr::null_mut()) };
        std::env::var_os("HUSHKEY_WHISPER_LOG").is_some()
    });
}

unsafe extern "C" fn log_message(_: sys::ggml_log_level, text: *const c_char, _: *mut c_void) {
    if text.is_null() {
        return;
    }
    // SAFETY: whisper supplies a NUL-terminated string for the callback.
    let text = unsafe { CStr::from_ptr(text) };
    if VERBOSE.get() == Some(&true) {
        let _ = std::io::stderr().lock().write_all(text.to_bytes());
    }
    let Some(message) = text.to_bytes().strip_prefix(b"whisper_backend_init_gpu: ") else {
        return;
    };
    let Ok(message) = std::str::from_utf8(message) else {
        return;
    };
    let _ = OBSERVED.try_with(|slot| {
        if let Some(previous) = slot.take() {
            slot.set(Some(observe(message.trim_end(), previous)));
        }
    });
}

fn observe(message: &str, previous: Acceleration) -> Acceleration {
    if message == "no GPU found" || message.starts_with("failed to initialize ") {
        return Acceleration::CpuFallback;
    }
    if let Some(backend) = message
        .strip_prefix("using ")
        .and_then(|s| s.strip_suffix(" backend"))
    {
        // whisper reports 'using' just before initialization. A subsequent
        // failure replaces this candidate, even when creating the state succeeds.
        return Acceleration::Gpu {
            backend: backend.to_owned(),
            device: String::new(),
        };
    }
    previous
}

pub(crate) fn observe_state<T>(create: impl FnOnce() -> T) -> (T, Acceleration) {
    OBSERVED.set(Some(Acceleration::Unknown));
    let result = create();
    let mut backend = OBSERVED.take().unwrap_or(Acceleration::Unknown);
    if let Acceleration::Gpu { backend, device } = &mut backend {
        if let Ok(name) = CString::new(backend.as_str()) {
            // SAFETY: ggml's registry owns the device and its description.
            // Resolve precisely the backend that this state initialized.
            unsafe {
                let dev = sys::ggml_backend_dev_by_name(name.as_ptr());
                if !dev.is_null() {
                    let description = sys::ggml_backend_dev_description(dev);
                    if !description.is_null() {
                        *device = CStr::from_ptr(description).to_string_lossy().into_owned();
                    }
                }
            }
        }
    }
    (result, backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_initialization_overrides_selected_gpu() {
        let selected = observe("using Vulkan1 backend", Acceleration::Unknown);
        assert!(matches!(&selected, Acceleration::Gpu { backend, .. } if backend == "Vulkan1"));
        assert_eq!(
            observe("failed to initialize Vulkan1 backend", selected),
            Acceleration::CpuFallback
        );
        assert_eq!(
            observe("device 0: Vulkan0 (type: 1)", Acceleration::Unknown),
            Acceleration::Unknown
        );
    }
}
