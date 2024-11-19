#pragma once

#include <iostream>

#include <string>
#include <cstdint>
#include <sched.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <functional>

#include "ddb/common.hpp"

/// @brief  Added magic number for testing DDBTraceMeta 
#define T_META_MATIC 12345ULL
// constexpr static uint64_t tMetaMagic = 12345;

// typedef struct {
//   uint32_t caller_comm_ip;
//   pid_t pid;
// } __attribute__((packed)) DDBCallerMeta;

// typedef struct {
//   uintptr_t rip;
//   uintptr_t rsp;
//   uintptr_t rbp;
// } __attribute__((packed)) DDBCallerContext;

// /// @brief  Added data structure for backtrace
// typedef struct {
//   uint64_t magic;
//   DDBCallerMeta meta;
//   DDBCallerContext ctx;
// } __attribute__((packed)) DDBTraceMeta;

namespace DDB {
struct DDBCallerMeta {
  uint32_t caller_comm_ip = 0;
  pid_t pid = 0;
  pid_t tid = 0;
};

struct DDBLocalMeta {
  uint32_t local_comm_ip = 0;
  pid_t pid = 0;
  pid_t tid = 0;
};

struct DDBCallerContext {
  uintptr_t pc = 0;  // Program Counter
  uintptr_t sp = 0;  // Stack Pointer
  uintptr_t fp = 0;  // Frame Pointer
  #ifdef __aarch64__
  uintptr_t lr = 0;  // Link Register (only on ARM64)
  #endif
};

/// @brief  Added data structure for backtrace
struct DDBTraceMeta {
  uint64_t magic = 0;
  DDBCallerMeta meta;
  DDBCallerContext ctx;
  // DDBLocalMeta local_meta;
};

static __attribute__((noinline)) uintptr_t get_pc() {
  // essentially return the return address of this function
  // to get the PC (program counter) at the caller position.
  // NOTE: noinline should be enforced to create a stack frame here.
  return reinterpret_cast<uintptr_t>(__builtin_return_address(0));
}

static inline __attribute((always_inline)) uintptr_t get_sp() {
  void* sp;
#if defined(__x86_64__)
  asm volatile("mov %%rsp, %0" : "=r" (sp));
#elif defined(__aarch64__)
  asm volatile("mov %0, sp" : "=r" (sp));
#else
  #error "Unsupported architecture"
#endif
  return reinterpret_cast<uintptr_t>(sp);
}

static inline __attribute((always_inline)) uintptr_t get_fp() {
  return reinterpret_cast<uintptr_t>(__builtin_frame_address(0));
}

static inline __attribute__((always_inline)) void get_context(DDBCallerContext* ctx) { 
  ctx->sp = get_sp();
  ctx->pc = get_pc();
  ctx->fp = get_fp();

#ifdef __aarch64__
  // Grab link register at ARM64, not sure if this is useful...
  void *lr;
  asm volatile ("mov %0, x30" : "=r" (lr));
  ctx->lr = (uintptr_t)lr;
#endif
  // std::cout << "rsp = " << _rsp << ", rip = " << _rip << ", rbp = " << _rbp << std::endl;
  // std::cout << "sp = " << ctx->sp << ", pc = " << ctx->pc << ", fp = " << ctx->fp << std::endl;
}

static inline __attribute__((always_inline)) void __get_caller_meta(DDBCallerMeta* meta) {
  meta->caller_comm_ip = ddb_meta.comm_ip;
  meta->pid = getpid();
  meta->tid = syscall(SYS_gettid);
}

static inline __attribute__((always_inline)) void get_trace_meta(DDBTraceMeta* trace_meta) {
  trace_meta->magic = T_META_MATIC;
  __get_caller_meta(&trace_meta->meta);
  get_context(&trace_meta->ctx);
}

namespace Backtrace {
  template<typename RT = void, class RPCCallable>
  __attribute__((noinline))
  static RT extraction(std::function<DDBTraceMeta()> extractor, RPCCallable&& rpc_callable) {
    DDBTraceMeta meta;
    if (extractor) {
      meta = extractor();
    }
    if constexpr (!std::is_void_v<RT>) {
      return rpc_callable();
    } else {
      rpc_callable();
    }
  }
} // namespace Backtrace
} // namespace DDB
