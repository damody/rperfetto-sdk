/*
 * C example for rperfetto-sdk.
 *
 *   c_demo [out.pftrace]        in-process session, writes a .pftrace file
 *   c_demo --system [seconds]   emit into the running traced session
 *   c_demo --atrace [seconds]   emit to ftrace trace_marker (ATRACE format)
 */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "rperfetto_sdk.h"

static RpfCategory g_cat_game;
static RpfCategory g_cat_audio;

static void busy_ms(int ms) {
  struct timespec start, now;
  clock_gettime(CLOCK_MONOTONIC, &start);
  do {
    clock_gettime(CLOCK_MONOTONIC, &now);
  } while ((now.tv_sec - start.tv_sec) * 1000 +
               (now.tv_nsec - start.tv_nsec) / 1000000 < ms);
}

static void* audio_thread(void* arg) {
  int frames = *(int*)arg;
  rpf_set_thread_name("c-audio");
  for (int i = 0; i < frames; i++) {
    RpfArg args[1];
    args[0].name = "buffer";
    args[0].kind = RPF_ARG_TYPE_INT;
    args[0].value.i = i;
    rpf_slice_begin(g_cat_audio, 0, "mix", args, 1);
    busy_ms(4);
    rpf_slice_end(g_cat_audio, 0);
    busy_ms(2);
  }
  return NULL;
}

static void run(int frames) {
  uint64_t gpu_track = rpf_track_named("GPU queue", 0);
  uint64_t mem_track = rpf_track_counter("heap", 0, "bytes");
  uint64_t load_track = rpf_track_counter("cpu_load", 0, NULL);
  pthread_t audio;
  pthread_create(&audio, NULL, audio_thread, &frames);

  for (int i = 0; i < frames; i++) {
    RpfArg args[3];
    args[0].name = "index";
    args[0].kind = RPF_ARG_TYPE_UINT;
    args[0].value.u = (uint64_t)i;
    args[1].name = "level";
    args[1].kind = RPF_ARG_TYPE_STRING;
    args[1].value.s = "castle";
    args[2].name = "vsync";
    args[2].kind = RPF_ARG_TYPE_BOOL;
    args[2].value.b = (i % 2) == 0;
    rpf_slice_begin(g_cat_game, 0, "c_frame", args, 3);

    rpf_slice_begin(g_cat_game, 0, "c_physics", NULL, 0);
    busy_ms(2);
    rpf_slice_end(g_cat_game, 0);

    rpf_slice_begin(g_cat_game, gpu_track, "gpu_draw", NULL, 0);
    busy_ms(3);
    rpf_instant(g_cat_game, 0, "c_present", NULL, 0);
    rpf_slice_end(g_cat_game, gpu_track);

    rpf_counter_i64(g_cat_game, mem_track, 1024 * (i + 1));
    rpf_counter_f64(g_cat_game, load_track, 0.25 * i);
    rpf_slice_end(g_cat_game, 0);
    busy_ms(3);
  }
  pthread_join(audio, NULL);
}

int main(int argc, char** argv) {
  RpfInitArgs init;
  memset(&init, 0, sizeof(init));
  init.process_name = "c_demo";
  init.producer_name = "rperfetto_c_demo";

  g_cat_game = rpf_category_register("c_game");
  g_cat_audio = rpf_category_register("c_audio");
  rpf_set_thread_name("c-main");

  if (argc > 1 && (!strcmp(argv[1], "--system") || !strcmp(argv[1], "--atrace"))) {
    int secs = argc > 2 ? atoi(argv[2]) : 10;
    init.backends = !strcmp(argv[1], "--system") ? RPF_BACKEND_SYSTEM : RPF_BACKEND_ATRACE;
    if (!rpf_init(&init)) {
      fprintf(stderr, "rpf_init failed\n");
      return 1;
    }
    time_t end = time(NULL) + secs;
    while (time(NULL) < end) run(10);
    rpf_shutdown();
    return 0;
  }

  const char* out = argc > 1 ? argv[1] : "c_demo.pftrace";
  init.backends = RPF_BACKEND_IN_PROCESS;
  if (!rpf_init(&init)) {
    fprintf(stderr, "rpf_init failed\n");
    return 1;
  }
  RpfSession* s = rpf_session_start(NULL);
  if (!s) {
    fprintf(stderr, "rpf_session_start failed\n");
    return 1;
  }
  run(5);
  rpf_session_stop(s);
  if (!rpf_session_write_file(s, out)) {
    fprintf(stderr, "write failed\n");
    return 1;
  }
  rpf_session_destroy(s);
  rpf_shutdown();
  fprintf(stderr, "wrote %s\n", out);
  return 0;
}
