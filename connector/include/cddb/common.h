#pragma once

#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
  uint32_t comm_ip;
  pid_t pid;
  /* readable ip */
  char ipv4_str[16]; /* enough for "xxx.xxx.xxx.xxx\0" */
  bool initialized;
} DDBMetadata;

/* Global metadata instance */
extern DDBMetadata ddb_meta;

/* Initialize the global metadata with default values */
static inline void init_default_ddb_meta() {
  ddb_meta.comm_ip = 0;
  ddb_meta.pid = 0;
  ddb_meta.ipv4_str[0] = '\0';
  ddb_meta.initialized = false;
}

/* Get pointer to global metadata */
static inline DDBMetadata *get_global_ddb_meta() { return &ddb_meta; }

/* Get a copy of global metadata */
static inline DDBMetadata get_ddb_meta() { return ddb_meta; }

/* Initialize global metadata with new values */
static inline void init_ddb_meta(const DDBMetadata *new_meta) {
  if (new_meta != NULL) {
    ddb_meta.comm_ip = new_meta->comm_ip;
    ddb_meta.pid = new_meta->pid;
    strncpy(ddb_meta.ipv4_str, new_meta->ipv4_str,
            sizeof(ddb_meta.ipv4_str) - 1);
    ddb_meta.ipv4_str[sizeof(ddb_meta.ipv4_str) - 1] = '\0';
    ddb_meta.initialized = true;
  }
}

/* Check if metadata is initialized */
static inline bool Initialized() { return ddb_meta.initialized; }

#ifdef __cplusplus
}
#endif
