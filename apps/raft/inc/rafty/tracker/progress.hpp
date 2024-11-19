#pragma once

#include <cstdint>
#include <map>
#include <sstream>
#include <string>

#include "rafty/tracker/state.hpp"

namespace rafty {
namespace tracker {
class Progress {
private:
  uint64_t match;
  uint64_t next;
  uint64_t sent_commit_idx;
  StateType state;
  bool recent_active;
  bool flow_paused;

public:
  Progress();
  Progress(uint64_t match, uint64_t next);
  ~Progress() = default;

  // getters
  uint64_t get_match() const;
  uint64_t get_next() const;
  uint64_t get_sent_commit_idx() const;
  StateType get_state() const;
  bool is_recent_active() const;
  bool is_paused() const;

  // setters
  void set_match(const uint64_t match);
  void set_recent_active(bool active);

  void reset(StateType state);
  void become_probe();
  void become_replicate();
  void sent_entries(int entries);
  bool can_bump_commit(uint64_t idx);
  void sent_commit(uint64_t commit);
  bool maybe_update(uint64_t n);
  bool maybe_decrease_to(uint64_t reject, uint64_t match_hint);

  inline std::string to_string() const {
    std::stringstream ss;
    ss << this->state << " match=" << this->match << " next=" << this->next;
    if (!this->recent_active) {
      ss << " inactive";
    } else {
      ss << " active";
    }
    return ss.str();
  }
};

inline std::ostream &operator<<(std::ostream &os, Progress const &p) {
  os << p.to_string();
  return os;
}

using ProgressMap = std::map<uint64_t, Progress>;

inline std::string to_string(ProgressMap const &pmap) {
  std::stringstream ss;
  ss << "Progress Map = {" << std::endl;
  for (auto const &[idx, progress] : pmap) {
    ss << idx << ": " << progress.to_string() << ", " << std::endl;
  }
  ss << "}";
  return ss.str();
}

inline std::ostream &operator<<(std::ostream &os, ProgressMap const &pmap) {
  os << to_string(pmap);
  return os;
}
} // namespace tracker
} // namespace rafty

#include "rafty/impl/progress.ipp"