# hushkey core (Rust)

hushkey's speech recognition as a library: microphone capture, whisper.cpp
transcription and push-to-talk sessions with a live preview, usable from Rust
and, through a C ABI, from any language. The Python hushkey app in the
repository root is unaffected; see
[the design](../docs/superpowers/specs/2026-10-28-rust-core-design.md).

| Crate | What |
| --- | --- |
| `hushkey-core` | The library: `Engine`, `RecordSession`, audio inputs, model lookup |
| `hushkey-ffi` | The C ABI (`hushkey.dll` / `libhushkey.so` / `.dylib`, static lib) and [`include/hushkey.h`](include/hushkey.h) |
| `hushkey-cli` | `hushkey-cli transcribe file.wav`, `record`, `devices` |

## Use it from C

```c
#include "hushkey.h"

static void on_event(const HkEvent *e, void *user) {
    if (e->kind == HK_EVENT_KIND_PREVIEW) printf("… %s\n", e->text);
    if (e->kind == HK_EVENT_KIND_FINAL)   printf("%s\n", e->text);
}

hk_process_init();                        /* first, before other threads */
HkEngineConfig cfg = {.size = sizeof cfg};
hk_engine_config_init(&cfg);
cfg.model = "large-v3-turbo";            /* or a path to a ggml-*.bin */
HkEngine *engine = hk_engine_new(&cfg);  /* NULL: see hk_last_error() */

HkSession *s = hk_record_start(engine, NULL, on_event, NULL);  /* microphone */
/* ... the user talks; previews arrive every 2 s ... */
char *text = NULL;
hk_record_stop(s, &text);                 /* FINAL, then STOPPED */
hk_string_free(text);
hk_engine_free(engine);
```

Rules of the ABI (also in the header): every struct starts with `size`, so it
can grow without breaking callers; failures return NULL or -1 and set
`hk_last_error()`; returned strings are freed with `hk_string_free`; event
strings live only during the callback; callbacks run on a library thread and
may call `hk_record_stop`. Ignore event kinds you do not know.

## Models

`model` is a file path or a whisper model name. Names are looked up in the
Hugging Face cache (where the Python hushkey already put them) and in
`HUSHKEY_MODEL_DIR` (default: the OS cache folder + `hushkey/models`). Builds
with the `download` feature (the CLI has it) fetch a missing model there.
`vad_model = "silero"` adds whisper.cpp's Silero voice activity detection,
which masks pauses so whisper does not invent text in them.

## Build

Rust 1.85+, CMake and a C++ compiler (whisper.cpp is built from source), and
libclang for bindgen (`LIBCLANG_PATH` if it is not found; on Windows the
`libclang` Python wheel ships one). Linux needs `libasound2-dev`.

```sh
cargo build --release                       # CPU
cargo build --release --features vulkan     # + VULKAN_SDK
cargo build --release --features metal      # macOS
cargo build --release --features cuda       # + CUDA toolkit
```

On a Vulkan build, call `hk_process_init()` (Rust: `init_process()`) at
program start, before other threads: it sets
`VK_LOADER_LAYERS_DISABLE=~implicit~` unless the host set it or
`WHISPER_CPP_ALLOW_LAYERS=1`. Capture and overlay layers that inject
themselves into every Vulkan app stalled inference for minutes on an
RX 7600 XT (2 s for 11 s of speech with large-v3-turbo without them). It is a
separate call because changing the environment while other threads read it
is undefined behaviour on Linux and macOS; a host can also set the variable
itself. `HUSHKEY_WHISPER_LOG=1` shows whisper.cpp's own log on stderr.

On Windows, build the Vulkan variant with a short `--target-dir` (e.g.
`C:\hkt`): whisper.cpp's nested shader-generator build otherwise exceeds the
260-character path limit and fails with `C1083`.

## Test

```sh
export HUSHKEY_TEST_MODEL=/path/to/ggml-tiny.bin
export HUSHKEY_TEST_SPEECH=/path/to/jfk.wav   # whisper.cpp samples/jfk.wav
cargo test --workspace
```

Without the two variables the real-inference tests skip. CI runs everything
on Linux, Windows and macOS and compiles `crates/hushkey-ffi/tests/c/smoke.c`
against the header. After changing the C API, regenerate the header:
`HUSHKEY_BLESS=1 cargo test -p hushkey-ffi --test header`.
