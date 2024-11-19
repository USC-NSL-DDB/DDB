#pragma once

#include <cstdint>
#include <map>
#include <string>

namespace rafty {
struct Config {
  uint64_t id;
  std::string addr;
  // maps id to addr
  std::map<uint64_t, std::string> peer_addrs;
};
} // namespace rafty
