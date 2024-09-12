#pragma once

#include <string>
#include <cstdint>

#include <unistd.h>

namespace DDB {
struct DDBMetadata{
  uint32_t comm_ip;
  pid_t pid;
  // readable ip 
  std::string ipv4_str;
};

extern DDBMetadata ddb_meta;

static inline DDBMetadata* get_global_ddb_meta() {
  return &ddb_meta;
}

static inline DDBMetadata get_ddb_meta() {
  return *get_global_ddb_meta();
}

static inline void init_ddb_meta(const DDBMetadata& new_meta) {
  *get_global_ddb_meta() = new_meta;
}

#ifdef DEFINE_DDB_META
DDBMetadata ddb_meta = {};
#endif
}