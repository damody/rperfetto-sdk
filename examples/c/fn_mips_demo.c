/*
 * Automatic function entry/exit tracing with -finstrument-functions, for
 * per-function MIPS from the system trace (no perf counters in the app, no
 * root). Build: examples/c/build.sh. Run:
 *
 *   fn_mips_demo --system [seconds]   events into the running traced session
 *   fn_mips_demo [out.pftrace]        in-process (slices only, no MIPS)
 *
 * Record with configs/function_mips.cfg, then run sql/function_mips.sql.
 * Every function in this file is instrumented by the compiler; nothing here
 * calls the tracing API except main().
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "rperfetto_sdk.h"

void rpf_instrument_enable(RpfCategory cat);
void rpf_instrument_disable(void);

/* noinline + external linkage: one slice per call, named via dladdr. */
#define WORK __attribute__((noinline))

static volatile uint64_t g_sink;

WORK uint64_t c_alu_work(uint64_t n) {
  uint64_t x = 0x9e3779b97f4a7c15ull;
  for (uint64_t i = 0; i < n; i++) {
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x += i;
  }
  g_sink = x;
  return x;
}

WORK uint32_t c_pointer_chase(const uint32_t* next, size_t steps) {
  uint32_t p = 0;
  for (size_t i = 0; i < steps; i++) p = next[p];
  g_sink = p;
  return p;
}

WORK double c_fp_work(uint32_t n) {
  volatile double v = 1.2345;
  for (uint32_t i = 0; i < n; i++) v = __builtin_sqrt(v * 1.000001 + i) / 1.0000003;
  return v;
}

WORK void c_sleepy(void) {
  c_alu_work(200000);
  usleep(4000);
}

WORK void c_frame(const uint32_t* next) {
  c_alu_work(2000000);
  c_pointer_chase(next, 300000);
  c_fp_work(200000);
  c_sleepy();
}

/* Sattolo's algorithm: one random cycle through all slots. */
WORK uint32_t* c_make_chain(size_t len) {
  uint32_t* v = malloc(len * sizeof(uint32_t));
  for (size_t i = 0; i < len; i++) v[i] = (uint32_t)i;
  uint64_t s = 0x2545F4914F6CDD1Dull;
  for (size_t i = len - 1; i > 0; i--) {
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    size_t j = (size_t)(s % i);
    uint32_t t = v[i];
    v[i] = v[j];
    v[j] = t;
  }
  return v;
}

int main(int argc, char** argv) {
  uint32_t* next = c_make_chain(16u << 20);
  RpfInitArgs init;
  memset(&init, 0, sizeof(init));
  init.process_name = "fn_mips_demo";
  init.producer_name = "rperfetto_fn_mips_c";
  rpf_set_thread_name("c-fn-main");

  int system = argc > 1 && !strcmp(argv[1], "--system");
  init.backends = system ? RPF_BACKEND_SYSTEM : RPF_BACKEND_IN_PROCESS;
  if (!rpf_init(&init)) {
    fprintf(stderr, "rpf_init failed\n");
    return 1;
  }
  RpfSession* s = system ? NULL : rpf_session_start(NULL);
  rpf_instrument_enable(NULL); /* default "fn" category */

  if (system) {
    int secs = argc > 2 ? atoi(argv[2]) : 10;
    time_t end = time(NULL) + secs;
    int n = 0;
    while (time(NULL) < end) {
      c_frame(next);
      n++;
    }
    fprintf(stderr, "%d frames\n", n);
  } else {
    for (int i = 0; i < 5; i++) c_frame(next);
  }

  rpf_instrument_disable();
  if (s) {
    const char* out = argc > 1 ? argv[1] : "fn_mips_demo.pftrace";
    rpf_session_stop(s);
    rpf_session_write_file(s, out);
    rpf_session_destroy(s);
    fprintf(stderr, "wrote %s\n", out);
  }
  rpf_shutdown();
  free(next);
  return 0;
}
