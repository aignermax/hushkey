# hushkey core (Rust)

hushkey's speech recognition as a library: microphone capture, whisper.cpp
transcription, push-to-talk sessions with a live preview, and continuous
listening that reports every utterance, recognizes voice commands and hands
everything else to a "brain" (Claudio, or an OpenAI-compatible server),
usable from Rust and, through a C ABI, from any language. The Python hushkey app in the
repository root is unaffected; see
[the design](../docs/superpowers/specs/2026-10-28-rust-core-design.md).

| Crate | What |
| --- | --- |
| `hushkey-core` | The library: `Engine`, `RecordSession`, `ListenSession`, `CommandSet`, the `Brain` trait, audio inputs, model lookup |
| `hushkey-brain` | Brains: `Claudio` (`/v1/chat`), `OpenAiChat` (`/v1/chat/completions`) |
| `hushkey-ffi` | The C ABI (`hushkey.dll` / `libhushkey.so` / `.dylib`, static lib) and [`include/hushkey.h`](include/hushkey.h) |
| `hushkey-cli` | `hushkey-cli transcribe file.wav`, `record`, `listen`, `devices` |

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

Listening continuously instead of push-to-talk: load the engine with
`cfg.vad_model = "silero"` (or a path), then

```c
HkListenConfig lis = {.size = sizeof lis};
hk_listen_config_init(&lis);
lis.end_silence_ms = 400;      /* pause that ends an utterance */
lis.prompt = "weiter, zurück, Kanal";  /* words whisper should expect */
HkSession *s = hk_listen_start(engine, &lis, on_event, NULL);
/* HK_EVENT_KIND_SPEECH_START, PREVIEW..., UTTERANCE per sentence ... */
hk_listen_stop(s);             /* the last utterance is finished first */
```

### Voice commands and a brain

Hands busy at a plug board: say "Kanal 6" and the test UI switches. A
command set maps phrases to names; `{n}` is a number (digits, or German or
English words), `{word}` one word, `{text}` the rest:

```c
lis.commands_json =
    "{\"commands\": ["
    "  {\"name\": \"next\",    \"phrases\": [\"weiter\", \"next\"]},"
    "  {\"name\": \"back\",    \"phrases\": [\"zurück\", \"back\"]},"
    "  {\"name\": \"channel\", \"phrases\": [\"kanal {n}\", \"channel {n}\"]}]}";
/* "Kanal sechs" -> HK_EVENT_KIND_COMMAND: name "channel", args {"6"} */
```

Matching tolerates small slips ("kanall 7") but prefers no command to a
doubtful one; the command words become whisper's prompt, which makes short
words reliable. Utterances that are no command go to the brain, if one is
set, and its answer arrives as `HK_EVENT_KIND_REPLY`:

```c
lis.brain = HK_BRAIN_KIND_CLAUDIO;                 /* or HK_BRAIN_KIND_OPENAI */
lis.brain_url = "https://192.168.1.112:8766";      /* Claudio on the Thor */
lis.brain_ca_cert = "server.pem";                  /* its self-signed certificate */
```

Brains connect through the operating system's TLS, so certificates in the
system store work too. Nothing leaves the machine without a `brain_url`.

From a terminal:

```sh
hushkey-cli -l de listen --commands lab.json \
    --claudio https://192.168.1.112:8766 --ca-cert server.pem
# [   3.65 -    5.02] Kanal 6
#   → command channel(6) [1.00]
# [  14.50 -   16.70] Wie spät ist es eigentlich?
#   ← Ich habe leider keinen Zugriff auf die aktuelle Uhrzeit ...
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
