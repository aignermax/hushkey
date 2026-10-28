/* A C host using include/hushkey.h: load a model, transcribe a file, record
 * from a WAV as if from a microphone, listen to it continuously. CI compiles
 * it against the built library. Usage: smoke <model> <speech.wav> <vad>
 */
#if !defined(_WIN32)
#define _POSIX_C_SOURCE 199309L /* nanosleep */
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "hushkey.h"

/* Counters written on the library's thread, read on the main thread. */
#ifdef _MSC_VER
#include <windows.h>
typedef volatile LONG counter;
#define BUMP(c) InterlockedIncrement(&(c))
#define READ(c) InterlockedCompareExchange(&(c), 0, 0)
#define RESET(c) InterlockedExchange(&(c), 0)
static void sleep_ms(int ms) { Sleep((DWORD)ms); }
#else
#include <time.h>
typedef int counter;
#define BUMP(c) __atomic_add_fetch(&(c), 1, __ATOMIC_SEQ_CST)
#define READ(c) __atomic_load_n(&(c), __ATOMIC_SEQ_CST)
#define RESET(c) __atomic_store_n(&(c), 0, __ATOMIC_SEQ_CST)
static void sleep_ms(int ms) {
    struct timespec t = {ms / 1000, (long)(ms % 1000) * 1000000L};
    nanosleep(&t, NULL);
}
#endif

static counter finals, stops, utterances;

static void on_event(const HkEvent *event, void *user_data) {
    (void)user_data;
    switch (event->kind) {
    case HK_EVENT_KIND_PREVIEW:
        printf("preview: %s\n", event->text);
        break;
    case HK_EVENT_KIND_FINAL:
        BUMP(finals);
        printf("final: %s\n", event->text);
        break;
    case HK_EVENT_KIND_SPEECH_START:
        printf("speech at %lld ms\n", (long long)event->start_ms);
        break;
    case HK_EVENT_KIND_UTTERANCE:
        BUMP(utterances);
        printf("utterance [%lld-%lld ms]: %s\n", (long long)event->start_ms,
               (long long)event->end_ms, event->text);
        break;
    case HK_EVENT_KIND_ERROR:
        printf("error: %s\n", event->text);
        break;
    case HK_EVENT_KIND_STOPPED:
        BUMP(stops);
        break;
    default:
        break; /* kinds added by newer versions */
    }
}

static int fail(const char *what) {
    const char *reason = hk_last_error();
    fprintf(stderr, "%s failed: %s\n", what, reason ? reason : "(no reason)");
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <model> <speech.wav> <vad-model>\n", argv[0]);
        return 2;
    }
    hk_process_init();
    if (hk_abi_version() != HK_ABI_VERSION) {
        fprintf(stderr, "ABI mismatch\n");
        return 1;
    }

    HkEngineConfig cfg = {.size = sizeof cfg};
    hk_engine_config_init(&cfg);
    cfg.model = argv[1];
    cfg.language = "en";
    cfg.vad_model = argv[3];
    HkEngine *engine = hk_engine_new(&cfg);
    if (!engine)
        return fail("hk_engine_new");

    char *text = hk_transcribe_file(engine, argv[2], NULL);
    if (!text)
        return fail("hk_transcribe_file");
    printf("file: %s\n", text);
    int ok = strstr(text, "country") != NULL;
    hk_string_free(text);

    HkRecordConfig rec = {.size = sizeof rec};
    hk_record_config_init(&rec);
    rec.input_wav = argv[2];
    rec.input_realtime = false;
    HkSession *session = hk_record_start(engine, &rec, on_event, NULL);
    if (!session)
        return fail("hk_record_start");
    char *final_text = NULL;
    if (hk_record_stop(session, &final_text) != 0)
        return fail("hk_record_stop");
    /* Stopped at once: the replay may not have delivered everything, but
     * the session must still finish cleanly with exactly one final. */
    hk_string_free(final_text);

    /* Listening to a file ends by itself when the file does. */
    HkListenConfig lis = {.size = sizeof lis};
    hk_listen_config_init(&lis);
    lis.input_wav = argv[2];
    lis.input_realtime = false;
    RESET(stops);
    session = hk_listen_start(engine, &lis, on_event, NULL);
    if (!session)
        return fail("hk_listen_start");
    for (int waited = 0; READ(stops) == 0; waited += 100) {
        if (waited > 120000) {
            fprintf(stderr, "listening never stopped\n");
            return 1;
        }
        sleep_ms(100);
    }
    if (hk_listen_stop(session) != 0)
        return fail("hk_listen_stop");
    hk_engine_free(engine);

    if (!ok || READ(finals) != 1 || READ(stops) != 1 || READ(utterances) < 1) {
        fprintf(stderr, "unexpected result: ok=%d finals=%ld stops=%ld utterances=%ld\n", ok,
                (long)READ(finals), (long)READ(stops), (long)READ(utterances));
        return 1;
    }
    puts("smoke ok");
    return 0;
}
