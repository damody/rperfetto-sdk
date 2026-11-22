/*
 * GCC/Clang -finstrument-functions hooks that turn every instrumented
 * function's entry/exit into rperfetto-sdk function slices.
 *
 * Compile the code you want to measure with -finstrument-functions. Every
 * function in this file is no_instrument_function, so it may be compiled with
 * or without the flag. Link with -rdynamic (Android: -Wl,--export-dynamic) so
 * dladdr() can name functions of the executable; functions without a dynamic
 * symbol (e.g. static ones) are named "<module>+0x<offset>", which
 * addr2line / llvm-symbolizer can resolve offline.
 *
 * Hooks stay silent until rpf_instrument_enable() is called.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <dlfcn.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "rperfetto_sdk.h"

#define NO_INSTR __attribute__((no_instrument_function))
#define MAX_DEPTH 1024
#define CACHE_SIZE 1024 /* power of two */

static RpfCategory g_cat;
static volatile int g_enabled;

/* Per-thread: which stack levels emitted a begin, so exits stay paired even
 * if hooks are enabled/disabled while functions are on the stack. */
static __thread uint16_t t_depth;
static __thread uint8_t t_emitted[MAX_DEPTH];
static __thread int t_in_hook;

struct name_entry {
  void* fn;
  const char* name;
};
static struct name_entry g_cache[CACHE_SIZE];
static pthread_mutex_t g_cache_lock = PTHREAD_MUTEX_INITIALIZER;

NO_INSTR static const char* resolve_name(void* fn) {
  size_t slot = ((uintptr_t)fn >> 2) & (CACHE_SIZE - 1);
  pthread_mutex_lock(&g_cache_lock);
  for (size_t i = 0; i < CACHE_SIZE; i++) {
    struct name_entry* e = &g_cache[(slot + i) & (CACHE_SIZE - 1)];
    if (e->fn == fn) {
      pthread_mutex_unlock(&g_cache_lock);
      return e->name;
    }
    if (e->fn == NULL) break;
  }
  pthread_mutex_unlock(&g_cache_lock);

  const char* name = NULL;
  Dl_info info;
  if (dladdr(fn, &info) && info.dli_sname && info.dli_saddr == fn) {
    name = info.dli_sname; /* owned by the loader, lives as long as the module */
  } else {
    char buf[256];
    const char* mod = "?";
    uintptr_t off = (uintptr_t)fn;
    if (dladdr(fn, &info) && info.dli_fname) {
      mod = strrchr(info.dli_fname, '/') ? strrchr(info.dli_fname, '/') + 1 : info.dli_fname;
      off -= (uintptr_t)info.dli_fbase;
    }
    snprintf(buf, sizeof(buf), "%s+0x%lx", mod, (unsigned long)off);
    name = strdup(buf); /* leaked on purpose: one per distinct function */
  }

  pthread_mutex_lock(&g_cache_lock);
  for (size_t i = 0; i < CACHE_SIZE; i++) {
    struct name_entry* e = &g_cache[(slot + i) & (CACHE_SIZE - 1)];
    if (e->fn == fn) break;
    if (e->fn == NULL) {
      e->fn = fn;
      e->name = name;
      break;
    }
  }
  pthread_mutex_unlock(&g_cache_lock);
  return name;
}

/* cat may be NULL for the default "fn" category. */
NO_INSTR void rpf_instrument_enable(RpfCategory cat) {
  g_cat = cat;
  __atomic_store_n(&g_enabled, 1, __ATOMIC_RELEASE);
}

NO_INSTR void rpf_instrument_disable(void) {
  __atomic_store_n(&g_enabled, 0, __ATOMIC_RELEASE);
}

NO_INSTR void __cyg_profile_func_enter(void* fn, void* call_site) {
  (void)call_site;
  uint16_t d = t_depth++;
  if (d >= MAX_DEPTH) return;
  t_emitted[d] = 0;
  if (!__atomic_load_n(&g_enabled, __ATOMIC_ACQUIRE) || t_in_hook) return;
  t_in_hook = 1;
  rpf_func_begin(g_cat, resolve_name(fn));
  t_in_hook = 0;
  t_emitted[d] = 1;
}

NO_INSTR void __cyg_profile_func_exit(void* fn, void* call_site) {
  (void)fn;
  (void)call_site;
  if (t_depth == 0) return;
  uint16_t d = --t_depth;
  if (d >= MAX_DEPTH || !t_emitted[d]) return;
  t_in_hook = 1;
  rpf_func_end();
  t_in_hook = 0;
}
