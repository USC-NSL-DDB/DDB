#pragma once

#include <functional>
#include <set>
#include <unordered_map>

#include "rafty/tracker/progress.hpp"

namespace rafty {
namespace tracker {
enum class VoteType : uint8_t { VoteGranted, VoteDenied, VotePending };

struct VoteResult {
  uint64_t granted;
  uint64_t rejected;
  VoteType type;
};

class ProgressTracker {
private:
  uint64_t quorum;
  ProgressMap p_map;
  std::unordered_map<uint64_t, bool> votes;
  std::set<uint64_t> voters; // represent voters as a set of ids

public:
  ProgressTracker() = delete;
  ProgressTracker(const std::set<uint64_t> &voters);
  ProgressTracker(std::set<uint64_t> &&voters);
  ~ProgressTracker() = default;

  ProgressTracker(const ProgressTracker &&other);
  ProgressTracker &operator=(const ProgressTracker &&other);

  inline const std::unordered_map<uint64_t, bool> &get_votes() const {
    return votes;
  }

  inline const ProgressMap &get_p_map() const { return p_map; }

  inline Progress &operator[](uint64_t id) { return get_progress_at(id); }

  inline std::set<uint64_t> get_ids() const {
    std::set<uint64_t> ids;
    for (auto const &[id, _] : p_map) {
      ids.insert(id);
    }
    return ids;
  }

  inline Progress &get_progress_at(uint64_t id) {
    auto pr = p_map.find(id);
    if (pr == p_map.end()) {
      throw std::runtime_error("peer not found in p_map");
    }
    return pr->second;
  }

  inline std::optional<std::reference_wrapper<Progress>>
  get_progress_if_exist(uint64_t id) {
    auto pr = p_map.find(id);
    if (pr == p_map.end()) {
      return std::nullopt;
    }
    return pr->second;
  }

  inline bool progress_exists(uint64_t id) const { return p_map.contains(id); }

  inline void reset_votes() { votes.clear(); }

  inline void record_vote(uint64_t id, bool vote) {
    // remove duplicates here
    if (!votes.contains(id)) {
      votes[id] = vote;
    }
  }

  inline VoteResult tally_votes() const {
    uint64_t granted = 0;
    uint64_t rejected = 0;
    for (auto const &[id, vote] : votes) {
      if (vote) {
        granted++;
      } else {
        rejected++;
      }
    }
    if (granted >= get_quorum()) {
      return {granted, rejected, VoteType::VoteGranted};
    } else if (rejected >= get_quorum()) {
      return {granted, rejected, VoteType::VoteDenied};
    }
    return {granted, rejected, VoteType::VotePending};
  }

  inline void apply(std::function<void(uint64_t, Progress &)> func) {
    for (auto &[id, progress] : p_map) {
      func(id, progress);
    }
  }

  inline void visit(std::function<void(uint64_t, const Progress &)> func) {
    apply([&func](uint64_t id, Progress &progress) { func(id, progress); });
  }

  // TODO: verify correctness.
  inline uint64_t get_committed() {
    auto n = static_cast<uint64_t>(this->p_map.size());

    if (n == 0) {
      // If no nodes are present, return the maximum possible index
      return std::numeric_limits<uint64_t>::max();
    }

    // Use std::array for small cluster sizes to avoid heap allocation
    std::array<uint64_t, 7> stk = {0}; // Initialize all elements to zero
    std::vector<uint64_t> arr;

    if (n <= stk.size()) {
      // Use std::array for up to 7 elements
      arr.assign(stk.begin(), stk.begin() + n);
    } else {
      // Allocate on the heap for larger clusters and initialize to zero
      arr.resize(n, 0);
    }

    // Collect all acknowledged indices
    auto i = n - 1;
    for (const auto &[id, pr] : this->p_map) {
      arr[i--] = pr.get_match();
    }

    // Sort the indices to find the majority acknowledged index
    std::sort(arr.begin(), arr.end());

    // Calculate the position for the majority quorum
    size_t pos = n - (n / 2 + 1);

    // Return the index that is acknowledged by a majority of nodes
    return arr[pos];
  }

  inline bool is_singleton() const { return voters.size() == 1; }

  inline uint64_t get_quorum() const { return quorum; }
};

} // namespace tracker
} // namespace rafty

#include "rafty/impl/tracker.ipp"
