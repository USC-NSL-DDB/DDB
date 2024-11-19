#include <cstdint>
#include <iostream>
#include <memory>
#include <stdexcept>

#include "common/common.hpp"
#include "common/utils/rand_gen.hpp"
#include "rafty/raft.hpp"

namespace rafty {
using grpc::ServerBuilder;
using grpc::ServerContext;
using grpc::experimental::ClientInterceptorFactoryInterface;
using grpc::experimental::CreateCustomChannelWithInterceptors;

Raft::Raft(const Config &config, MessageQueue<ApplyResult> &ready)
    : id(config.id), listening_addr(config.addr), peer_addrs(config.peer_addrs),
      dead(false), ready_queue(ready), service_(this), current_term(0),
      voted_for(std::nullopt), commit_index(0), last_applied(0),
      tracker(std::set<uint64_t>()), state(RaftState::FOLLOWER),
      leader_id(std::nullopt), leader_transferee(std::nullopt),
      heartbeat_elapsed(0), election_elapsed(0),
      logger(utils::logger::get_logger(id)) {
  std::set<uint64_t> ids;
  for (const auto &peer_addr : this->peer_addrs) {
    ids.insert(peer_addr.first);
  }
  // add self id (counting self as a voter is a common practice)
  ids.insert(this->id);
  this->tracker = tracker::ProgressTracker(std::move(ids));
  this->logs.emplace_back(raftpb::Entry()); // placeholder for index 0
  this->become_follower(this->current_term);
}

Raft::~Raft() { this->stop_server(); }

void Raft::run() {
  std::thread([this] {
    while (!this->dead.load()) {
      this->tick();
      std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
  }).detach();
}

void Raft::say_hello() {
  raftpb::HelloRequest request;
  request.set_name("rafty");

  raftpb::HelloReply reply;

  grpc::ClientContext context;
  auto status = peers_[2]->SayHello(&context, request, &reply);

  if (status.ok()) {
    logger->debug("Raft {} received the return msg from: {}", id,
                 reply.message());
  } else {
    logger->error("Raft {} failed to receive msg from: {}", id,
                  status.error_message());
  }
}

void Raft::receive_hello(const std::string &msg) {
  logger->debug("Raft {} received msg from: {}", id, msg);
}

ProposalResult Raft::propose(const std::string &data) {
  if (this->state != RaftState::LEADER) {
    return {.index = 0, .term = this->current_term, .is_leader = false};
  }
  std::lock_guard<std::mutex> lock(this->mtx);

  auto index = this->last_index() + 1;
  auto term = this->current_term;
  auto new_entry = raftpb::Entry();
  new_entry.set_term(term);
  new_entry.set_data(data);
  new_entry.set_index(index);

  this->logs.emplace_back(new_entry);
  this->progress_index(index, this->id);
  this->logger->debug("{} [term {}, index {}] received a proposal with data: {}",
                     this->id, term, index, data);

  return {.index = index,
          .term = term,
          .is_leader = (this->state == RaftState::LEADER)};
}

ProposalResult Raft::propose_sync(const std::string &data) {
  if (this->state != RaftState::LEADER) {
    return {.index = 0, .term = this->current_term, .is_leader = false};
  }
  // std::lock_guard<std::mutex> lock(this->mtx);
  this->mtx.lock();

  auto index = this->last_index() + 1;
  auto term = this->current_term;
  auto new_entry = raftpb::Entry();
  new_entry.set_term(term);
  new_entry.set_data(data);
  new_entry.set_index(index);

  this->logs.emplace_back(new_entry);
  this->logger->debug("{} [term {}, index {}] received a proposal with data: {}",
                     this->id, term, index, data);
  ProposalResult prop_result = {
    .index = index,
    .term = term,
    .is_leader = (this->state == RaftState::LEADER)
  };

  auto committed = std::make_shared<std::promise<bool>>();
  this->prop_promises.insert({index, committed});
  this->progress_index(index, this->id);

  this->mtx.unlock();

  if (committed->get_future().get()) {
    this->logger->info("Proposal succeeded");
    return prop_result;
  } else {
    // SHOULD NOT HAPPEN
    throw std::runtime_error("Proposal failed");
    return {.index = 0, .term = this->current_term, .is_leader = false};
  }
}

State Raft::get_state() const {
  std::lock_guard<std::mutex> lock(this->mtx);
  return {.term = this->current_term,
          .is_leader = (this->state == RaftState::LEADER)};
}

void Raft::become_leader() {
  if (this->state == RaftState::FOLLOWER) {
    this->logger->warn(
        "{} at term {} perform invalid transition: follower -> leader",
        this->id, this->current_term);
    return;
  }
  this->reset(this->current_term);
  this->tick_fn = std::bind(&Raft::tick_heartbeat, this);
  this->leader_id = this->id;
  this->state = RaftState::LEADER;

  // Followers enter replicate mode when they've been successfully probed
  // (perhaps after having received a snapshot as a result). The leader is
  // trivially in this state. Note that r.reset() has initialized this
  // progress with the last index already.
  auto &pr = this->tracker[this->id];
  pr.become_replicate();

  // force send empty heartbeat for probing
  this->bcast_heartbeat();

  this->logger->debug("{} become leader at term {}", this->id,
                     this->current_term);
}

void Raft::become_follower(uint64_t term, std::optional<uint64_t> leader_id) {
  this->reset(term);
  this->leader_id = leader_id;
  this->tick_fn = std::bind(&Raft::tick_election, this);
  if (this->state != RaftState::FOLLOWER) {
    auto old_state = this->state;
    this->state = RaftState::FOLLOWER;
    this->logger->debug("{} become follower at term {} from {}", this->id, term,
                       to_string(old_state));
  } else {
    this->logger->debug("{} remain follower at term {}", this->id, term);
  }
}

void Raft::become_candidate() {
  // sanity check
  // std::lock_guard<std::mutex> lock(this->mtx);
  if (this->state == RaftState::LEADER) {
    this->logger->critical(
        "{} at term {} perform invalid transition: leader -> candidate",
        this->id, this->current_term);
    return;
  }
  // this->step_fn =
  //     std::bind(&Raft::step_as_candidate, this, std::placeholders::_1);
  this->reset(this->current_term + 1);
  this->tick_fn = std::bind(&Raft::tick_election, this);
  this->voted_for = this->id;
  this->state = RaftState::CANDIDATE;
  this->logger->debug("{} become candidate at term {}", this->id,
                     this->current_term);
}

void Raft::reset(uint64_t term) {
  if (this->current_term != term) {
    this->current_term = term;
    this->voted_for = std::nullopt;
  }
  this->leader_id = std::nullopt;

  this->election_elapsed = 0;
  this->heartbeat_elapsed = 0;
  this->reset_rand_elec_timout();

  this->abort_leader_transfer();
  this->tracker.reset_votes();
  auto last_index = this->logs.size() - 1;
  this->tracker.apply([this, last_index](uint64_t id, tracker::Progress &pr) {
    pr = tracker::Progress(0, last_index + 1);
    if (id == this->id) {
      pr.set_match(last_index);
    }
  });
}

void Raft::abort_leader_transfer() { this->leader_transferee = std::nullopt; }

void Raft::reset_rand_elec_timout() {
  this->randomized_elec_timeout =
      this->election_timeout +
      utils::RandGen::get_instance().intn(this->election_timeout);
  logger->debug("{} reset election timeout to randomized_elec_timeout: {}",
                this->id, this->randomized_elec_timeout);
}

void Raft::tick() {
  std::lock_guard<std::mutex> lock(this->mtx);
  if (this->tick_fn) {
    this->tick_fn();
  } else {
    this->logger->critical(
        "{} tick function is not set, Raft is in invalid state", this->id);
  }
}

void Raft::tick_election() {
  this->election_elapsed++;
  if (this->have_past_elec_timeout()) {
    this->election_elapsed = 0;
    // try to start an election.
    this->campaign();
  }
}

bool Raft::have_past_elec_timeout() const {
  return election_elapsed >= randomized_elec_timeout;
}

void Raft::campaign() {
  if (this->state == RaftState::LEADER) {
    this->logger->debug("{} ignoring election because already leader",
                        this->id);
    return;
  }
  this->logger->debug("{} starts new election at term {}", this->id,
                     this->current_term);
  this->become_candidate();
  // auto msg_type = raftpb::MsgVote;
  auto term = this->current_term;

  // inferred ascending order.
  auto ids = this->tracker.get_ids();

  for (auto const &peer_id : ids) {
    if (peer_id == this->id) {
      // vote for self
      auto vr = this->poll(peer_id, true);
      this->resolve_vote_result(vr);
      continue;
    }
    auto last_id = this->get_last_entry_id();
    this->logger->debug(
        "{} [logterm: {}, index: {}] sent vote request to {} at term {}",
        this->id, last_id.term, last_id.index, peer_id, term);

    std::thread([this, peer_id, term, last_id] {
      this->send_request_vote_rpc(this->id, peer_id, term, last_id);
    }).detach();
  }
}

void Raft::send_request_vote_rpc(uint64_t candidate_id, uint64_t target_id,
                                 uint64_t sender_term, const EntryID &last_id) {
  raftpb::RequestVoteRequest req;
  req.set_term(sender_term);
  req.set_candidate_id(candidate_id);
  req.set_last_log_idx(last_id.index);
  req.set_last_log_term(last_id.term);

  auto context = this->create_context(target_id);

  raftpb::RequestVoteReply reply;

  this->logger->debug("{} [term {}] sending RequestVote to {}", this->id,
                     sender_term, target_id);
  auto status = this->peers_[target_id]->RequestVote(&*context, req, &reply);

  std::lock_guard<std::mutex> lock(this->mtx);

  if (this->is_dead())
    return;

  if (!status.ok()) {
    this->logger->error("candidate {} failed to send RequestVote to {}: {}",
                        this->id, target_id, status.error_message());
    return;
  }

  if (req.term() != this->current_term) {
    this->logger->debug("{} term has changed from {} to {} during election, "
                       "dropping vote response from {}",
                       this->id, req.term(), this->current_term, id);
    return;
  }

  if (this->state != RaftState::CANDIDATE) {
    this->logger->debug(
        "{} is no longer candidate (now is {}), dropping vote response from {}",
        this->id, to_string(this->state), id);
    return;
  }

  auto v_r = tracker::VoteResult();
  if (reply.vote_granted()) {
    this->logger->debug("{} [candidate] received vote from {}", this->id,
                       target_id);
    v_r = this->poll(target_id, true);
  } else {
    if (reply.term() > this->current_term) {
      this->logger->debug("{} [candidate] received higher term from {}, step "
                         "back to follower immediately",
                         this->id, id);
      // TODO: step back to follower
      this->become_follower(reply.term(), std::nullopt);
      return;
    }
    this->logger->debug("{} [candidate] received rejection from {}", this->id,
                       target_id);
    v_r = this->poll(target_id, false);
  }
  // this->election_elapsed = 0; // being a bit relaxed here
  this->resolve_vote_result(v_r);
}

void Raft::resolve_vote_result(const tracker::VoteResult &v_r) {
  switch (v_r.type) {
  case tracker::VoteType::VoteGranted:
    this->logger->debug(
        "{} [candidate] voting SUCCESS, granted: {}, rejected: {}", this->id,
        v_r.granted, v_r.rejected);
    this->become_leader();
    return;
  case tracker::VoteType::VoteDenied:
    this->logger->debug("{} [candidate] voting FAIL, granted: {}, rejected: {}",
                       this->id, v_r.granted, v_r.rejected);
    this->become_follower(this->current_term, std::nullopt);
    return;
  default:
    // pending vote, indecisive
    this->logger->debug(
        "{} [candidate] voting PENDING, granted: {}, rejected: {}", this->id,
        v_r.granted, v_r.rejected);
    break;
  }
}

void Raft::progress_index(uint64_t index, uint64_t id) {
  auto &pr = this->tracker[id];
  this->progress_index(index, pr);
}

void Raft::progress_index(uint64_t index, tracker::Progress &pr) {
  if (pr.maybe_update(index) ||
      (pr.get_match() == index &&
        pr.get_state() == tracker::StateType::Prob)) {
    if (pr.get_state() == tracker::StateType::Prob) {
      pr.become_replicate();
    }
    this->maybe_commit();
  }
}

void Raft::tick_heartbeat() {
  this->heartbeat_elapsed++;
  this->election_elapsed++;

  if (this->election_elapsed >= this->election_timeout) {
    this->election_elapsed = 0;
    // TODO: need to check quorum?
    // If current leader cannot transfer leadership in electionTimeout, it
    // becomes leader again.
    if (this->state == RaftState::LEADER &&
        this->leader_transferee.has_value()) {
      this->abort_leader_transfer();
    }
  }

  if (this->state != RaftState::LEADER)
    return;

  if (this->heartbeat_elapsed >= this->heartbeat_timeout) {
    this->heartbeat_elapsed = 0;
    this->bcast_heartbeat();
  }
}

void Raft::send_heartbeat(uint64_t to, bool empty_entry) {
  auto &pr = this->tracker[to];

  auto sender_term = this->current_term;
  auto leader_id = this->id;
  auto prev_log_idx = pr.get_next() - 1;
  assert(prev_log_idx <= this->last_index());
  auto prev_log_term = this->logs[prev_log_idx].term();

  auto args = SendAppendEntriesArgs{
      .sender_term = sender_term,
      .leader_id = leader_id,
      .prev_log_idx = prev_log_idx,
      .prev_log_term = prev_log_term,
      .entries = empty_entry ? std::vector<raftpb::Entry>()
                             : std::vector<raftpb::Entry>(this->logs.begin() +
                                                              prev_log_idx + 1,
                                                          this->logs.end()),
      .leader_commit = this->get_committed()};

  logger->debug("{} [term {}] sending heartbeat to {} [prev_log_idx: {}, "
               "prev_log_term: {}, leader_commit: {}]",
               this->id, sender_term, to, args.prev_log_idx, args.prev_log_term,
               args.leader_commit);

  // std::stringstream ss;
  // auto count = 0;
  // for (const auto& entry : this->logs) {
  //   ss << count << " " << entry.data() << ", ";
  //   count++;
  // }
  // logger->debug("log entries: {}", ss.str());
  // std::stringstream ss1;
  // ss1 << "[ ";
  // for (const auto& entry : args.entries) {
  //   ss1 << entry.data() << ", ";
  // }
  // ss1 << "]";
  // logger->debug("sending entries: {}", ss1.str());

  std::thread([this, to, &pr, args = std::move(args)] {
    this->send_append_entries_rpc(to, pr, args);
  }).detach();
}

void Raft::send_append_entries_rpc(uint64_t target_id, tracker::Progress &pr,
                                   const SendAppendEntriesArgs &args) {
  raftpb::AppendEntriesRequest req;
  req.set_term(args.sender_term);
  req.set_leader_id(args.leader_id);
  req.set_prev_log_idx(args.prev_log_idx);
  req.set_prev_log_term(args.prev_log_term);

  for (const auto &entry : args.entries) {
    req.add_entries()->CopyFrom(entry);
  }
  req.set_leader_commit(args.leader_commit);

  auto context = this->create_context(target_id);

  raftpb::AppendEntriesReply reply;
  grpc::Status status =
      this->peers_[target_id]->AppendEntries(&*context, req, &reply);

  std::lock_guard<std::mutex> lock(this->mtx);
  if (this->is_dead())
    return;

  if (!status.ok()) {
    this->logger->error("leader {} failed to send heartbeat to {}: {}",
                        this->id, target_id, status.error_message());
    return;
  }

  if (req.term() != this->current_term) {
    this->logger->debug("{} term has changed from {} to {} during heartbeat, "
                       "dropping response from {}",
                       this->id, req.term(), this->current_term, target_id);
    return;
  }

  if (this->state != RaftState::LEADER) {
    this->logger->debug(
        "{} is no longer leader (now is {}), dropping response from {}",
        this->id, to_string(this->state), target_id);
    return;
  }

  if (reply.success()) {
    auto new_match_index = req.prev_log_idx() + req.entries_size();
    this->progress_index(new_match_index, pr);
  } else {
    // rejected
    if (reply.term() > this->current_term) {
      // outdated leader, step back to follower directly.
      this->logger->debug(
          "outdated raft leader {} received higher term from {}: {}", this->id,
          target_id, reply.term());
      this->become_follower(reply.term(), std::nullopt);
      return;
    }

    this->logger->debug(
        "{} received AppendEntriesReply (rejected) from {} for index {}",
        this->id, target_id, args.prev_log_idx);
    auto next_probe_idx = args.prev_log_idx - 1;
    // fast recovery
    if (reply.conflict_term() > 0) {
      auto cf_term = reply.conflict_term();
      auto cf_idx = reply.conflict_idx();
      auto last_idx = [this, cf_term]() -> std::optional<uint64_t> {
        for (auto i = this->last_index(); i > 0; i--) {
          if (this->get_term(i) == cf_term) {
            return i;
          }
        }
        return std::nullopt;
      }();
      if (last_idx.has_value()) {
        next_probe_idx = last_idx.value() + 1;
      } else {
        next_probe_idx = cf_idx;
      }
    }
    if (pr.maybe_decrease_to(args.prev_log_idx, next_probe_idx)) {
      this->logger->debug("{} decreased progress of {} to {}", this->id,
                          target_id, pr.to_string());
      if (pr.get_state() == tracker::StateType::Replicate) {
        pr.become_probe();
      }
    }
  }
}

void Raft::bcast_heartbeat(bool empty_entry) {
  this->tracker.visit([this, empty_entry](uint64_t id,
                                          const tracker::Progress &pr
                                          [[maybe_unused]]) {
    if (id == this->id) {
      return;
    }
    this->send_heartbeat(id, empty_entry);
  });
}

uint64_t Raft::last_index() const { return this->logs.size() - 1; }

void Raft::commit_to(uint64_t idx) {
  if (idx > this->get_committed()) {
    if (idx > this->last_index()) {
      this->logger->critical(
          "commit index {} is out of bound last_index [{}]. Raft log is lost?",
          idx, this->last_index());
      // shouldn't happen
      throw std::runtime_error("commit index out of bound");
    }
    auto old_commit = this->get_committed();
    this->commit_index = idx;
    this->logger->debug("{} update committed index from {} to {}", this->id,
                       old_commit, this->get_committed());

    auto committed = this->get_committed();
    if (committed > this->last_applied) {
      // notify the caller/tester
      for (auto i = this->last_applied + 1; i <= committed; i++) {
        auto result = ApplyResult{
            .valid = true, .data = this->logs[i].data(), .index = i
        };
        this->apply(result);
        this->logger->info("{} applied log at index {} with data: {}", this->id,
                         i, this->logs[i].data());
        this->last_applied = i;

        // lab3
        if (this->prop_promises.contains(i)) {
          this->logger->info("set promise for index {}", i);
          this->prop_promises[i]->set_value(true);
        }
      }
    }
  }
}

bool Raft::maybe_commit() {
  auto at = EntryID{.term = this->current_term,
                    .index = this->tracker.get_committed()};
  logger->debug("{} maybe_commit at index {} with term {}", this->id, at.index,
               at.term);
  if (at.term != 0 && at.index > this->get_committed() &&
      this->match_term(at)) {
    this->commit_to(at.index);
    return true;
  }
  return false;
}

bool Raft::match_term(const EntryID &at) const {
  auto term = this->get_term(at.index);
  return term == at.term;
}

uint64_t Raft::get_term(uint64_t at) const {
  if (at > this->last_index())
    return 0;
  return this->logs[at].term();
}

EntryID Raft::get_last_entry_id() const {
  auto idx = this->last_index();
  auto term = this->get_term(idx);
  return {term, idx};
}

tracker::VoteResult Raft::poll(uint64_t id, bool granted) {
  if (granted) {
    this->logger->debug("{} received VoteGrant from {} at term {}, quorum: {}",
                       this->id, id, this->current_term,
                       this->tracker.get_quorum());
  } else {
    this->logger->debug("{} received VoteReject from {} at term {}, quorum: {}",
                       this->id, id, this->current_term,
                       this->tracker.get_quorum());
  }
  this->tracker.record_vote(id, granted);
  return this->tracker.tally_votes();
}

uint64_t Raft::find_conflict(std::span<const raftpb::Entry> entries) const {
  for (auto &entry : entries) {
    auto id = get_entry_id(entry);
    if (id.index > this->last_index()) {
      // no conflict
      return id.index;
    }
    if (!this->match_term(id)) {
      if (id.index <= this->last_index()) {
        // found conflicts, should print
        this->logger->debug("{} found conflict at index {} [existing term: {}, "
                           "conflicting term: {}]. entry = {}",
                           this->id, id.index, this->get_term(id.index),
                           id.term, entry.DebugString());
      }
      return id.index;
    }
  }
  return 0;
}

// helpers
EntryID Raft::get_entry_id(const raftpb::Entry &entry) const {
  return {entry.term(), entry.index()};
}

// getters
uint64_t Raft::get_committed() const { return this->commit_index; }

} // namespace rafty
