#pragma once

#include <cstdint>
#include <functional>
#include <future>
#include <memory>
#include <mutex>
#include <span>
#include <string>
#include <unordered_map>
#include <vector>

#include <grpcpp/grpcpp.h>

#include "common/common.hpp"
#include "common/config.hpp"
#include "common/logger.hpp"
#include "rafty/raft_service.hpp"
#include "rafty/tracker/progress.hpp"
#include "rafty/tracker/tracker.hpp"

#include "common/utils/net_intercepter.hpp"
#include "common/utils/thread_pool.hpp"

#include "toolings/msg_queue.hpp"

#include "raft.grpc.pb.h"

using namespace toolings;

namespace rafty {
// Remove: student impl
using RaftServiceStub = std::unique_ptr<raftpb::RaftService::Stub>;
using grpc::Server;

using TickFn = std::function<void()>;

enum class RaftState : uint8_t {
  LEADER = 0,
  FOLLOWER = 1,
  CANDIDATE = 2,
};

inline std::string to_string(RaftState state) {
  switch (state) {
  case RaftState::LEADER:
    return "LEADER";
  case RaftState::FOLLOWER:
    return "FOLLOWER";
  case RaftState::CANDIDATE:
    return "CANDIDATE";
  default:
    return "UNKNOWN";
  }
}

class Raft {
  // funcs
public:
  Raft(const Config &config, MessageQueue<ApplyResult> &ready);
  ~Raft();

  // WARN: do not modify the signature
  void start_server();
  void stop_server();
  void connect_peers();

  // WARN: do not modify this function
  bool is_dead() const;
  // WARN: do not modify the signature
  void kill();

  // TODO: implement `run`
  void run();

  // WARN: do not modify the signature
  // TODO: implement `propose` and `get_state`
  ProposalResult propose(const std::string &data);
  State get_state() const;

  // lab3: sycn propose
  ProposalResult propose_sync(const std::string &data);

private:
  std::unique_ptr<grpc::ClientContext> create_context(uint64_t to) const;
  void apply(const ApplyResult &result);

  // properties
protected:
  mutable std::mutex mtx;

private:
  // WARN: do not modify the declaration
  uint64_t id;
  std::string listening_addr;
  std::map<uint64_t, std::string> peer_addrs;

  std::atomic<bool> dead;
  MessageQueue<ApplyResult> &ready_queue;

public:
  // Remove: test purpose
  void say_hello();
  void add_ready(const std::string &data);

// Remove: student implementation
private: // vars
  utils::ThreadPool tpool; 
  
  // configuration needed variables
  std::unordered_map<uint64_t, RaftServiceStub> peers_;
  RaftService service_;
  std::unique_ptr<Server> server_;

  // raft state variables
  // persistent state
  uint64_t current_term;
  std::optional<uint64_t> voted_for;
  std::vector<raftpb::Entry> logs;

  // volatile state on all servers
  uint64_t commit_index;
  uint64_t last_applied;

  // volatile state on leaders
  tracker::ProgressTracker tracker;

  // not required from paper
  RaftState state;
  std::optional<uint64_t> leader_id;
  std::optional<uint64_t> leader_transferee;

  // internally managed
  uint64_t heartbeat_elapsed;
  uint64_t election_elapsed;

  constexpr static uint64_t election_timeout = 300;
  constexpr static uint64_t heartbeat_timeout = 30;
  uint64_t randomized_elec_timeout;

  TickFn tick_fn = nullptr;

  // lab3
  std::mutex prop_mtx;
  std::unordered_map<uint64_t, std::shared_ptr<std::promise<bool>>> prop_promises;

  std::unique_ptr<rafty::utils::logger> logger;

  friend class RaftService;

private: // funcs
  void receive_hello(const std::string &msg);
  void reset(uint64_t term);
  void abort_leader_transfer();
  void reset_rand_elec_timout();
  void tick();
  void tick_election();
  void tick_heartbeat();
  bool have_past_elec_timeout() const;
  void campaign();
  void send_heartbeat(uint64_t to, bool empty_entry = false);
  void bcast_heartbeat(bool empty_entry = false);

  void become_leader();
  void become_follower(uint64_t term,
                       std::optional<uint64_t> leader_id = std::nullopt);
  void become_candidate();
  void resolve_vote_result(const tracker::VoteResult &v_r);
  // attempt to progress the replicated index for a peer (or self), and commit if possible
  void progress_index(uint64_t index, uint64_t id);
  void progress_index(uint64_t index, tracker::Progress &pr);

  // send and handle request_vote rpc and its response
  void send_request_vote_rpc(uint64_t candidate_id, uint64_t target_id,
                             uint64_t sender_term, const EntryID &last_id);

  struct SendAppendEntriesArgs {
    uint64_t sender_term;
    uint64_t leader_id;
    uint64_t prev_log_idx;
    uint64_t prev_log_term;
    std::vector<raftpb::Entry> entries;
    uint64_t leader_commit;
  };

  // send and handle append_entries rpc and its response
  void send_append_entries_rpc(uint64_t target_id, tracker::Progress &pr,
                               const SendAppendEntriesArgs &args);

  uint64_t last_index() const;
  void commit_to(uint64_t idx);
  bool maybe_commit();
  bool match_term(const EntryID &at) const;
  uint64_t get_term(uint64_t at) const;
  EntryID get_last_entry_id() const;
  tracker::VoteResult poll(uint64_t id, bool granted);
  uint64_t find_conflict(std::span<const raftpb::Entry> entries) const;
  // getters
  uint64_t get_committed() const;

  // helpers
  EntryID get_entry_id(const raftpb::Entry &entry) const;
};

} // namespace rafty

#include "rafty/impl/raft.ipp"
