#pragma once

#include <cstdint>
#include <string>

namespace rafty {
struct ProposalResult {
  uint64_t index;
  uint64_t term;
  bool is_leader;
};

struct ApplyResult {
  bool valid;
  std::string data;
  uint64_t index;
};

struct State {
  uint64_t term;
  bool is_leader;
};

struct EntryID {
  uint64_t term;
  uint64_t index;
};

} // namespace rafty
