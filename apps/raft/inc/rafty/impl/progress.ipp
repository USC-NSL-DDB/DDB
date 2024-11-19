#pragma once

namespace rafty {
namespace tracker {
inline Progress::Progress()
    : match(0), next(0), sent_commit_idx(0), state(StateType::Prob),
      recent_active(false), flow_paused(false) {}

inline Progress::Progress(uint64_t match, uint64_t next)
    : match(match), next(next) {}

inline uint64_t Progress::get_match() const { return this->match; }
inline uint64_t Progress::get_next() const { return this->next; }
inline uint64_t Progress::get_sent_commit_idx() const {
  return this->sent_commit_idx;
}
inline StateType Progress::get_state() const { return this->state; }
inline bool Progress::is_recent_active() const { return this->recent_active; }
inline bool Progress::is_paused() const {
  switch (this->state) {
  case StateType::Prob:
  case StateType::Replicate:
    return this->flow_paused;
  default:
    throw std::runtime_error("unexpected state");
  }
}
inline void Progress::set_match(const uint64_t match) { this->match = match; }
inline void Progress::set_recent_active(bool active) {
  this->recent_active = active;
}

inline void Progress::reset(StateType state) {
  this->flow_paused = false;
  this->state = state;
}

inline void Progress::become_probe() {
  this->reset(StateType::Prob);
  this->next = this->match + 1;
  this->sent_commit_idx = std::min(this->sent_commit_idx, this->next - 1);
}

inline void Progress::become_replicate() {
  this->reset(StateType::Replicate);
  this->next = this->match + 1;
}

inline void Progress::sent_entries(int entries) {
  switch (this->state) {
  case StateType::Prob:
    if (entries > 0) {
      this->flow_paused = true;
    }
    break;
  case StateType::Replicate:
    if (entries > 0) {
      this->next += static_cast<uint64_t>(entries);
      // adjust inflights? do we need to keep this data structure?
    }
    break;
  default:
    break;
  }
}

inline bool Progress::can_bump_commit(uint64_t idx) {
  return idx > this->sent_commit_idx && this->sent_commit_idx < this->next - 1;
}

inline void Progress::sent_commit(uint64_t commit) {
  this->sent_commit_idx = commit;
}

inline bool Progress::maybe_update(uint64_t n) {
  if (n <= this->match) {
    return false;
  }
  this->match = n;
  this->next = std::max(this->next, n + 1);
  this->flow_paused = false;
  return true;
}

inline bool Progress::maybe_decrease_to(uint64_t reject, uint64_t match_hint) {
  if (this->state == StateType::Replicate) {
    if (reject <= this->match) {
      return false;
    }
    this->next = this->match + 1;
    this->sent_commit_idx = std::min(this->sent_commit_idx, this->next - 1);
    return true;
  }

  // The rejection must be stale if "rejected" does not match next - 1. This
  // is because non-replicating followers are probed one entry at a time.
  // The check is a best effort assuming message reordering is rare.
  if (this->next - 1 != reject) {
    return false;
  }

  this->next = std::max(std::min(reject, match_hint + 1), this->match + 1);

  this->sent_commit_idx = std::min(this->sent_commit_idx, this->next - 1);
  this->flow_paused = false;
  return true;
}

} // namespace tracker
} // namespace rafty
