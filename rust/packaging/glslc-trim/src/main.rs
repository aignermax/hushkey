//! `glslc` for hushkey's release builds: whisper.cpp's Vulkan shaders
//! without the ones for quantized weights.
//!
//! whisper.cpp builds every shader for every weight type ggml knows, ~54 MB
//! of SPIR-V embedded in the program. hushkey's models are f16, which use
//! the f32/f16 shaders (~5 MB); the rest (q4_0 … q6_k, iq*, mxfp4, bf16) is
//! dead weight. ggml-vulkan creates a pipeline only when an operation needs
//! it, so a shader that is never used is never read.
//!
//! The release workflow puts this program in the real glslc's place (that
//! one moves to `real-glslc`, named in `HUSHKEY_REAL_GLSLC`), so CMake finds
//! it however it looks. whisper.cpp's shader generator
//! then runs it once per shader with `-o <dir>/<shader name>.spv`: shaders
//! named for a quantized type get a SPIR-V header and nothing else (the
//! generator needs a non-empty file to define the symbol), everything else
//! goes to the real glslc unchanged. hushkey-core runs quantized models on
//! the CPU, so these placeholders are never compiled into a pipeline.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitCode};

/// The weight types whose shaders are left out (whisper.cpp's
/// `type_names` without f32 and f16, plus q8_1, the activations' format
/// for quantized matrix products).
const DROPPED: &[&str] = &[
    "q4_0", "q4_1", "q5_0", "q5_1", "q8_0", "q8_1", "q2_k", "q3_k", "q4_k", "q5_k", "q6_k",
    "iq1_s", "iq1_m", "iq2_xxs", "iq2_xs", "iq2_s", "iq3_xxs", "iq3_s", "iq4_xs", "iq4_nl",
    "mxfp4", "bf16",
];

/// A SPIR-V module header with no instructions: magic, version 1.0,
/// generator 0, id bound 1, schema 0.
const PLACEHOLDER: [u32; 5] = [0x0723_0203, 0x0001_0000, 0, 1, 0];

/// Whether `name` (a shader's name, `matmul_q4_k_f32_aligned`) has one of
/// the dropped types as a whole `_`-separated part.
fn dropped(name: &str) -> bool {
    DROPPED.iter().any(|t| {
        name.match_indices(t).any(|(at, _)| {
            let end = at + t.len();
            (at == 0 || name.as_bytes()[at - 1] == b'_')
                && (end == name.len() || name.as_bytes()[end] == b'_')
        })
    })
}

/// The output file of a glslc call (`-o <file>`), unless it writes to
/// stdout.
fn output(args: &[OsString]) -> Option<&Path> {
    let at = args.iter().position(|a| a == "-o")?;
    let out = Path::new(args.get(at + 1)?);
    (out != Path::new("-")).then_some(out)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(out) = output(&args) {
        let name = out.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if dropped(name) {
            let bytes: Vec<u8> = PLACEHOLDER.iter().flat_map(|w| w.to_le_bytes()).collect();
            return match std::fs::write(out, bytes) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("glslc-trim: {}: {e}", out.display());
                    ExitCode::FAILURE
                }
            };
        }
    }
    let Some(real) = std::env::var_os("HUSHKEY_REAL_GLSLC") else {
        eprintln!("glslc-trim: HUSHKEY_REAL_GLSLC (the real glslc) is not set");
        return ExitCode::FAILURE;
    };
    match Command::new(&real).args(&args).status() {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(code.clamp(0, 255) as u8),
            None => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("glslc-trim: {}: {e}", Path::new(&real).display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantized_shaders_are_dropped() {
        for name in [
            "matmul_q4_k_f32_aligned_cm1",
            "mul_mat_vec_iq2_xxs_f16_f32_subgroup",
            "dequant_q8_0",
            "get_rows_mxfp4",
            "cpy_f32_q5_1",
            "quantize_q8_1_x4_subgroup",
            "matmul_bf16_aligned",
            "iq4_nl",
        ] {
            assert!(dropped(name), "{name}");
        }
    }

    #[test]
    fn float_shaders_are_kept() {
        for name in [
            "matmul_f16_f32_aligned_cm1",
            "flash_attn_f32_f16_f16acc_cm2",
            "soft_max_f32",
            "rope_neox_f16",
            "conv2d_f32",
            // A type's name inside a longer part is not that type.
            "xq4_0",
            "q4_0x",
        ] {
            assert!(!dropped(name), "{name}");
        }
    }

    #[test]
    fn only_a_file_output_is_considered() {
        let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            output(&args(&[
                "-fshader-stage=compute",
                "in.comp",
                "-o",
                "dir/dequant_q4_0.spv"
            ])),
            Some(Path::new("dir/dequant_q4_0.spv"))
        );
        // CMake's extension probes write to stdout.
        assert_eq!(output(&args(&["-o", "-", "test.comp"])), None);
        assert_eq!(output(&args(&["--version"])), None);
    }
}
