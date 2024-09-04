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


extern DDBMetadata ddb_meta;


static inline DDBMetadata* get_global_ddb_meta() {
  return &ddb_meta;
}

static inline DDBMetadata get_ddb_meta() {
  return *get_global_ddb_meta();
}

static inline void init_ddb_meta(const DDBMetadata* new_meta) {
  if (new_meta != NULL) {
    *get_global_ddb_meta() = *new_meta;
  }
}

static inline void update_ddb_meta(uint32_t comm_ip, uint16_t comm_port, const char* host) {
  DDBMetadata* meta = get_global_ddb_meta();
  meta->comm_ip = comm_ip;
  meta->comm_port = comm_port;
  if (host != NULL) {
    strncpy(meta->host, host, sizeof(meta->host) - 1);
    meta->host[sizeof(meta->host) - 1] = '\0';
  }
}

#ifdef __cplusplus
}
#endif

#ifdef DEFINE_DDB_META
DDBMetadata ddb_meta = {0, 0};
#endif