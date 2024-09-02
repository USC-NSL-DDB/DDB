#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <netdb.h> // NI_MAXHOST
#include <string.h> // strncpy

typedef struct {
  uint32_t comm_ip;
  uint16_t comm_port;
  // readable hostname
  char host[NI_MAXHOST];
} DDBMetadata;

// extern DDBMetadata ddb_meta;

static DDBMetadata* get_global_ddb_meta() {
  static DDBMetadata ddb_meta = {0, 0};
  return &ddb_meta;
}

static DDBMetadata get_ddb_meta() {
  return *get_global_ddb_meta();
}

static void init_ddb_meta(const DDBMetadata* new_meta) {
  if (new_meta != NULL) {
    *get_global_ddb_meta() = *new_meta;
  }
}

static void update_ddb_meta(uint32_t comm_ip, uint16_t comm_port, const char* host) {
  DDBMetadata* meta = get_global_ddb_meta();
  meta->comm_ip = comm_ip;
  meta->comm_port = comm_port;
  strncpy(meta->host, host, sizeof(meta->host) - 1);
  meta->host[sizeof(meta->host) - 1] = '\0';
}

// /// @brief  Added magic number for testing RPCRewqProcletCallDebugMeta
// constexpr static uint64_t tMetaMagic = 12345;

// /// @brief  Added data structure for backtrace
// typedef struct {
//   uint64_t magic;
//   uint32_t caller_comm_ip;
//   uintptr_t rip;
//   uintptr_t rsp;
//   uintptr_t rbp;
//   pid_t pid;
// } __attribute__((packed)) RPCReqProcletCallDebugMeta;


#ifdef __cplusplus
}
#endif
