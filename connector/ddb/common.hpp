#pragma once

#include <string>
#include <cstdint>

#include <unistd.h>

namespace DDB {
struct DDBMetadata{
  uint32_t comm_ip = 0;
  pid_t pid = 0;
  // readable ip 
  std::string ipv4_str;
  bool initialized = false;
};

// extern DDBMetadata ddb_meta;
// #ifdef DEFINE_DDB_META
inline DDBMetadata ddb_meta = {};
// #endif

static inline DDBMetadata* get_global_ddb_meta() {
  return &ddb_meta;
}

static inline DDBMetadata get_ddb_meta() {
  return *get_global_ddb_meta();
}

static inline void init_ddb_meta(const DDBMetadata& new_meta) {
  *get_global_ddb_meta() = new_meta;
  get_global_ddb_meta()->initialized = true;
}

static inline bool Initialized() {
  return get_ddb_meta().initialized;
}
}