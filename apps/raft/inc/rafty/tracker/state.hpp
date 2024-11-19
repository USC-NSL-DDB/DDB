#pragma once

#include <cstdint>
#include <string>

namespace rafty {
namespace tracker {
enum class StateType : uint8_t { Prob, Replicate };

inline std::string to_string(StateType const &type) {
  switch (type) {
  case StateType::Prob:
    return "Probe";
  case StateType::Replicate:
    return "Replicate";
  default:
    return "ERROR";
  }
}

inline std::ostream &operator<<(std::ostream &os, StateType const &type) {
  os << to_string(type);
  return os;
}
} // namespace tracker
} // namespace rafty
