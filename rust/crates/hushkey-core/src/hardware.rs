//! First-run model sizing using the same device registry as whisper.cpp.

use std::cmp::Reverse;
use std::ffi::CStr;
use std::thread::available_parallelism;

use whisper_rs::whisper_rs_sys as sys;

/// A local model and the device settings to pass to [`crate::EngineConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareRecommendation {
    pub model: &'static str,
    pub use_gpu: bool,
    pub gpu_device: i32,
    /// The detected capabilities and the choice made from them, for diagnostics.
    pub evidence: String,
}

/// Inspect this build's registered devices without loading or fetching a model.
/// Passing `false` bypasses the native GPU registry entirely.
pub fn recommend_hardware(allow_gpu: bool) -> HardwareRecommendation {
    let threads = available_parallelism().map_or(1, |n| n.get());
    let simd = cpu_simd();
    if !allow_gpu {
        return cpu_choice(threads, simd, "GPU disabled");
    }
    if !cfg!(any(feature = "cuda", feature = "metal", feature = "vulkan")) {
        return cpu_choice(threads, simd, "CPU-only build");
    }
    choose(&registered_gpus(), threads, simd)
}

fn cpu_simd() -> Option<&'static str> {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    if std::is_x86_feature_detected!("avx2") {
        return Some("AVX2");
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        return Some("NEON");
    }
    None
}

fn cpu_choice(threads: usize, simd: Option<&str>, reason: &str) -> HardwareRecommendation {
    let model = if threads >= 4 && simd.is_some() {
        "base"
    } else {
        "tiny"
    };
    let simd = simd.unwrap_or("no AVX2/NEON detected");
    HardwareRecommendation {
        model,
        use_gpu: false,
        gpu_device: 0,
        evidence: format!(
            "{reason}. Selected {model} on CPU ({threads} available threads, {simd})"
        ),
    }
}

struct Gpu {
    backend: String,
    description: String,
    integrated: bool,
    free: u64,
    total: u64,
}

fn registered_gpus() -> Vec<Gpu> {
    crate::acceleration::install_log_hook();
    // SAFETY: ggml owns these registry devices and NUL-terminated device strings
    // for the life of the process. Only query indexes within dev_count().
    unsafe {
        (0..sys::ggml_backend_dev_count())
            .filter_map(|index| {
                let device = sys::ggml_backend_dev_get(index);
                let kind = sys::ggml_backend_dev_type(device);
                // whisper_backend_init_gpu counts both GPU types and skips CPU/ACCEL.
                // Keep even unusable GPUs here so later indexes match gpu_device.
                let integrated = match kind {
                    sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU => false,
                    sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU => true,
                    _ => return None,
                };
                let (mut free, mut total) = (0, 0);
                sys::ggml_backend_dev_memory(device, &mut free, &mut total);
                let label = |ptr: *const std::ffi::c_char| {
                    if ptr.is_null() {
                        return String::new();
                    }
                    CStr::from_ptr(ptr).to_string_lossy().into_owned()
                };
                Some(Gpu {
                    backend: label(sys::ggml_backend_dev_name(device)),
                    description: label(sys::ggml_backend_dev_description(device)),
                    integrated,
                    free: free as u64,
                    total: total as u64,
                })
            })
            .collect()
    }
}

const MIB: u64 = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GpuModel {
    Base,
    Small,
    Turbo,
}

impl Gpu {
    fn model(&self) -> Option<GpuModel> {
        // Older Metal devices can omit both working-set figures. Start small
        // there. A known capacity with zero free is an exhausted device.
        if self.free == 0 && self.total == 0 && self.backend == "Metal" {
            return Some(GpuModel::Base);
        }
        // Native Metal/Vulkan subtract unsigned usage from budget. Treat an
        // underflow as exhaustion rather than mistaking it for spare memory.
        if self.free > self.total {
            return None;
        }
        // Reserve headroom for inference state and decoder scratch. An iGPU
        // can advertise most of system RAM despite modest compute throughput.
        match self.free / MIB {
            4096.. if !self.integrated => Some(GpuModel::Turbo),
            2048.. => Some(GpuModel::Small),
            768.. => Some(GpuModel::Base),
            _ => None,
        }
    }

    fn evidence(&self, index: usize) -> String {
        let memory = if self.total == 0 {
            "memory unreported".to_owned()
        } else if self.free > self.total {
            "inconsistent memory budget".to_owned()
        } else {
            format!("{} / {} MiB available", self.free / MIB, self.total / MIB)
        };
        let kind = if self.integrated {
            "integrated GPU"
        } else {
            "GPU"
        };
        format!(
            "{kind} {index} {} ({}, {memory})",
            self.backend, self.description
        )
    }
}

fn choose(gpus: &[Gpu], threads: usize, simd: Option<&str>) -> HardwareRecommendation {
    let candidate = gpus
        .iter()
        .enumerate()
        .filter_map(|(index, gpu)| {
            // For equal models prefer a discrete device, then more available
            // memory. Preserve whisper's first device on an exact tie.
            Some((gpu.model()?, !gpu.integrated, gpu.free, Reverse(index)))
        })
        .max();
    if let Some((tier, _, _, Reverse(index))) = candidate {
        let model = match tier {
            GpuModel::Base => "base",
            GpuModel::Small => "small",
            GpuModel::Turbo => "large-v3-turbo",
        };
        return HardwareRecommendation {
            model,
            use_gpu: true,
            gpu_device: index as i32,
            evidence: format!("Selected {model} on {}", gpus[index].evidence(index)),
        };
    }
    if gpus.is_empty() {
        return cpu_choice(threads, simd, "No compatible GPU registered");
    }
    let devices: Vec<_> = gpus
        .iter()
        .enumerate()
        .map(|(i, gpu)| gpu.evidence(i))
        .collect();
    cpu_choice(
        threads,
        simd,
        &format!("Insufficient GPU memory: {}", devices.join(", ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(free_mib: u64, total_mib: u64) -> Gpu {
        Gpu {
            backend: "Vulkan".into(),
            description: "test adapter".into(),
            integrated: false,
            free: free_mib * MIB,
            total: total_mib * MIB,
        }
    }

    #[test]
    fn cpu_uses_available_threads_and_simd() {
        for (threads, simd, expected) in [
            (4, Some("NEON"), "base"),
            (3, Some("AVX2"), "tiny"),
            (8, None, "tiny"),
        ] {
            let choice = choose(&[], threads, simd);
            assert_eq!(choice.model, expected);
            assert!(!choice.use_gpu);
            assert!(choice.evidence.contains("No compatible GPU"));
        }
        let cpu = recommend_hardware(false);
        assert!(!cpu.use_gpu);
        assert!(cpu.evidence.contains("GPU disabled"));
    }

    #[test]
    fn sizes_model_from_current_budget() {
        for (free, expected) in [
            (767, None),
            (768, Some("base")),
            (2047, Some("base")),
            (2048, Some("small")),
            (4095, Some("small")),
            (4096, Some("large-v3-turbo")),
        ] {
            let choice = choose(&[gpu(free, 8192)], 4, Some("AVX2"));
            assert_eq!(choice.use_gpu.then_some(choice.model), expected);
        }
    }

    #[test]
    fn preserves_whisper_gpu_indexes_and_prefers_suitable_adapters() {
        let integrated = Gpu {
            integrated: true,
            ..gpu(16384, 16384)
        };
        assert_eq!(
            choose(std::slice::from_ref(&integrated), 8, None).model,
            "small"
        );
        let adapters = [
            gpu(0, 8192),
            integrated,
            gpu(4096, 6144),
            gpu(8192, 8192),
            gpu(8192, 8192),
        ];
        let choice = choose(&adapters, 8, None);
        assert_eq!(choice.gpu_device, 3);
        assert_eq!(choice.model, "large-v3-turbo");
        assert!(choice.evidence.contains("GPU 3"));
    }

    #[test]
    fn distinguishes_unknown_metal_memory_from_exhaustion() {
        let metal = |free, total| Gpu {
            backend: "Metal".into(),
            ..gpu(free, total)
        };
        let unknown = choose(&[metal(0, 0)], 8, None);
        assert!(unknown.use_gpu);
        assert_eq!(unknown.model, "base");
        for device in [metal(0, 8192), metal(8193, 8192), gpu(0, 0)] {
            assert!(!choose(&[device], 8, None).use_gpu);
        }
        assert_eq!(
            choose(&[metal(8192, 8192)], 8, None).model,
            "large-v3-turbo"
        );
    }

    #[cfg(not(any(feature = "vulkan", feature = "cuda", feature = "metal")))]
    #[test]
    fn cpu_build_recommends_cpu_even_when_gpu_allowed() {
        let choice = recommend_hardware(true);
        assert!(!choice.use_gpu);
        assert!(choice.evidence.contains("CPU-only build"));
    }

    #[cfg(any(feature = "vulkan", feature = "cuda", feature = "metal"))]
    #[test]
    #[ignore = "requires a GPU build with a usable local adapter"]
    fn probe_local_gpu() {
        let choice = recommend_hardware(true);
        eprintln!("{}", choice.evidence);
        assert!(choice.use_gpu);
    }
}
