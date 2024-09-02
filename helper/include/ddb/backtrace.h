#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <stdint.h>
#include <sched.h>

/// @brief  Added magic number for testing RPCRewqProcletCallDebugMeta
#define T_META_MATIC = 12345ULL;
// constexpr static uint64_t tMetaMagic = 12345;

/// @brief  Added data structure for backtrace
typedef struct {
  uint64_t magic;
  uint32_t caller_comm_ip;
  uintptr_t rip;
  uintptr_t rsp;
  uintptr_t rbp;
  pid_t pid;
} __attribute__((packed)) RPCReqProcletCallDebugMeta;



#ifdef __cplusplus
}
#endif
