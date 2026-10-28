/* A C host using include/hushkey.h: load a model, transcribe a file, record
 * from a WAV as if from a microphone, listen to it continuously. CI compiles
 * it against the built library. Usage: smoke <model> <speech.wav> <vad>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "hushkey.h"

static volatile int finals, stops, utterances;

static void on_event(const HkEvent *event, void *user_data) {
    (void)user_data;
    switch (event->kind) {
    case HK_EVENT_KIND_PREVIEW:
        printf("preview: %s\n", event->text);
        break;
    case HK_EVENT_KIND_FINAL:
        finals++;
        printf("final: %s\n", event->text);
        break;
    case HK_EVENT_KIND_SPEECH_START:
        printf("speech at %lld ms\n", (long long)event->start_ms);
        break;
    case HK_EVENT_KIND_UTTERANCE:
        utterances++;
        printf("utterance [%lld-%lld ms]: %s\n", (long long)event->start_ms,
               (long long)event->end_ms, event->text);
        break;
    case HK_EVENT_KIND_ERROR:
        printf("error: %s\n", event->text);
        break;
    case HK_EVENT_KIND_STOPPED:
        stops++;
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
    stops = 0;
    session = hk_listen_start(engine, &lis, on_event, NULL);
    if (!session)
        return fail("hk_listen_start");
    while (stops == 0)
        ; /* set from the library's thread */
    if (hk_listen_stop(session) != 0)
        return fail("hk_listen_stop");
    hk_engine_free(engine);

    if (!ok || finals != 1 || stops != 1 || utterances < 1) {
        fprintf(stderr, "unexpected result: ok=%d finals=%d stops=%d utterances=%d\n", ok,
                finals, stops, utterances);
        return 1;
    }
    puts("smoke ok");
    return 0;
}
