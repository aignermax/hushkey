/* A C host using include/hushkey.h: load a model, transcribe a file, record
 * from a WAV as if from a microphone. CI compiles it against the built
 * library. Usage: smoke <model> <speech.wav>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "hushkey.h"

static int finals, stops;

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
    if (argc != 3) {
        fprintf(stderr, "usage: %s <model> <speech.wav>\n", argv[0]);
        return 2;
    }
    if (hk_abi_version() != HK_ABI_VERSION) {
        fprintf(stderr, "ABI mismatch\n");
        return 1;
    }

    HkEngineConfig cfg = {.size = sizeof cfg};
    hk_engine_config_init(&cfg);
    cfg.model = argv[1];
    cfg.language = "en";
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
    hk_engine_free(engine);

    if (!ok || finals != 1 || stops != 1) {
        fprintf(stderr, "unexpected result: ok=%d finals=%d stops=%d\n", ok, finals, stops);
        return 1;
    }
    puts("smoke ok");
    return 0;
}
