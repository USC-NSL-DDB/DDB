#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <sched.h>
#include <unistd.h>

#include "ddb/common.h"

/// @brief  Added magic number for testing DDBTraceMeta 
#define T_META_MATIC 12345ULL
// constexpr static uint64_t tMetaMagic = 12345;

typedef struct {
  uint32_t caller_comm_ip;
  pid_t pid;
} __attribute__((packed)) DDBCallerMeta;

typedef struct {
  uintptr_t rip;
  uintptr_t rsp;
  uintptr_t rbp;
} __attribute__((packed)) DDBCallerContext;

/// @brief  Added data structure for backtrace
typedef struct {
  uint64_t magic;
  DDBCallerMeta meta;
  DDBCallerContext ctx;
} __attribute__((packed)) DDBTraceMeta;

static inline __attribute__((always_inline)) void get_context(DDBCallerContext* ctx) { 
  void *rsp;
  void *rbp;

  // Fetch the current stack pointer (RSP)
  asm volatile ("mov %%rsp, %0" : "=r" (rsp));

  // Fetch the current base pointer (RBP)
  asm volatile ("mov %%rbp, %0" : "=r" (rbp));

  ctx->rsp = (uintptr_t) rsp;
  ctx->rip = (uintptr_t) __builtin_return_address(0); // Approximation to get RIP
  ctx->rbp = (uintptr_t) rbp;
}

static inline __attribute__((always_inline)) void __get_caller_meta(DDBCallerMeta* meta) {
  meta->caller_comm_ip = ddb_meta.comm_ip;
  meta->pid = getpid();
}

static inline __attribute__((always_inline)) void get_trace_meta(DDBTraceMeta* trace_meta) {
  trace_meta->magic = T_META_MATIC;
  __get_caller_meta(&trace_meta->meta);
  get_context(&trace_meta->ctx);
}

#ifdef __cplusplus
}
#endif
