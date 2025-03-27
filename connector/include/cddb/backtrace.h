#pragma once

#include <sched.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Added magic number for testing DDBTraceMeta */
#define T_META_MATIC 12345ULL

/* Caller metadata structure */
typedef struct {
  uint32_t caller_comm_ip;
  pid_t pid;
  pid_t tid;
} DDBCallerMeta;

/* Local metadata structure */
typedef struct {
  uint32_t local_comm_ip;
  pid_t pid;
  pid_t tid;
} DDBLocalMeta;

/* Caller context structure */
typedef struct {
  uintptr_t pc;  /* Program Counter */
  uintptr_t sp;  /* Stack Pointer */
  uintptr_t fp;  /* Frame Pointer */
#ifdef __aarch64__
  uintptr_t lr;  /* Link Register (only on ARM64) */
#endif
} DDBCallerContext;

/* Trace metadata structure */
typedef struct {
  uint64_t magic;
  DDBCallerMeta meta;
  DDBCallerContext ctx;
  /* DDBLocalMeta local_meta; */
} DDBTraceMeta;

/* External global variable for communication IP */
extern uint32_t ddb_meta_comm_ip;

/* Check if trace metadata is valid */
static inline int ddb_trace_meta_valid(DDBTraceMeta* trace) {
  return trace->magic == T_META_MATIC;
}

/* Get program counter */
static __attribute__((noinline)) uintptr_t ddb_get_pc(void) {
  /* essentially return the return address of this function
     to get the PC (program counter) at the caller position.
     NOTE: noinline should be enforced to create a stack frame here. */
  return (uintptr_t)__builtin_return_address(0);
}

/* Get stack pointer */
static inline __attribute((always_inline)) uintptr_t ddb_get_sp(void) {
  void* sp;
#if defined(__x86_64__)
  asm volatile("mov %%rsp, %0" : "=r"(sp));
#elif defined(__aarch64__)
  asm volatile("mov %0, sp" : "=r"(sp));
#else
#error "Unsupported architecture"
#endif
  return (uintptr_t)sp;
}

/* Get frame pointer */
static inline __attribute((always_inline)) uintptr_t ddb_get_fp(void) {
  return (uintptr_t)__builtin_frame_address(0);
}

/* Get caller context */
static inline __attribute__((always_inline)) void ddb_get_context(
    DDBCallerContext* ctx) {
  ctx->sp = ddb_get_sp();
  ctx->pc = ddb_get_pc();
  ctx->fp = ddb_get_fp();

#ifdef __aarch64__
  /* Grab link register at ARM64, not sure if this is useful... */
  void* lr;
  asm volatile("mov %0, x30" : "=r"(lr));
  ctx->lr = (uintptr_t)lr;
#endif
}

/* Get caller metadata */
static inline __attribute__((always_inline)) void ddb_get_caller_meta(
    DDBCallerMeta* meta) {
  meta->caller_comm_ip = ddb_meta_comm_ip;
  meta->pid = getpid();
  meta->tid = syscall(SYS_gettid);
}

/* Get trace metadata */
static inline __attribute__((always_inline)) void ddb_get_trace_meta(
    DDBTraceMeta* trace_meta) {
  trace_meta->magic = T_META_MATIC;
  ddb_get_caller_meta(&trace_meta->meta);
  ddb_get_context(&trace_meta->ctx);
}

/* Backtrace extraction function - simple version without templates */
__attribute__((noinline)) static void __attribute__((unused)) ddb_backtrace_extraction(
    DDBTraceMeta* (*extractor)(void), void (*callback)(void)) {
  DDBTraceMeta meta = {0};
  asm volatile("" : "+m"(meta));  /* Force compiler to assume meta is modified */
  
  if (extractor) {
    meta = *extractor();
  }
  
  if (!ddb_trace_meta_valid(&meta)) {
    printf("WARN: Magic doesn't match\n");
  }

  callback();
}

/* Version that returns a value through a void* parameter */
__attribute__((noinline)) static void __attribute__((unused)) ddb_backtrace_extraction_with_return(
    DDBTraceMeta* (*extractor)(void), void* (*callback)(void), void* result) {
  DDBTraceMeta meta = { .magic = T_META_MATIC, .meta = {0}, .ctx = {0} };
  asm volatile("" : "+m"(meta));  /* Force compiler to assume meta is modified */
  
  if (extractor) {
    meta = *extractor();
  }
  
  if (!ddb_trace_meta_valid(&meta)) {
    printf("WARN: Magic doesn't match\n");
  }

  if (result) {
    *(void**)result = callback();
  }
}

#ifdef __cplusplus
}
#endif
