#pragma once

#include <cstdint>

namespace rafty {
namespace tracker {
// TODO: use voters
inline ProgressTracker::ProgressTracker(const std::set<uint64_t> &voters)
    : voters(voters) {
  this->quorum = static_cast<uint64_t>((this->voters.size() / 2) + 1);
  for (const auto &id : this->voters) {
    this->p_map[id] = Progress();
  }
}

inline ProgressTracker::ProgressTracker(std::set<uint64_t> &&voters)
    : voters(std::move(voters)) {
  this->quorum = static_cast<uint64_t>((this->voters.size() / 2) + 1);
  for (const auto &id : this->voters) {
    this->p_map[id] = Progress();
  }
}

inline ProgressTracker::ProgressTracker(const ProgressTracker &&other)
    : quorum(other.quorum), p_map(std::move(other.p_map)),
      votes(std::move(other.votes)), voters(std::move(other.voters)) {}

inline ProgressTracker &
ProgressTracker::operator=(const ProgressTracker &&other) {
  if (this != &other) {
    this->quorum = other.quorum;
    this->p_map = std::move(other.p_map);
    this->votes = std::move(other.votes);
    this->voters = std::move(other.voters);
  }
  return *this;
}
} // namespace tracker
} // namespace rafty
