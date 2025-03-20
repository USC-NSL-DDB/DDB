#include <grpcpp/grpcpp.h>

#include "rafty/raft.hpp"
#include "rafty/raft_service.hpp"

namespace rafty {
using grpc::Status;

RaftService::RaftService(Raft *raft) : raft_(raft) {
  this->logger = utils::logger::get_logger(raft_->id);
}

Status RaftService::AppendEntries(grpc::ServerContext *context [[maybe_unused]],
                                  const raftpb::AppendEntriesRequest *request,
                                  raftpb::AppendEntriesReply *response) {
  auto r = this->raft_;
  this->logger->debug("{} received AppendEntries RPC from {} for term {}", r->id,
                     request->leader_id(), request->term());
  std::lock_guard<std::mutex> lock(r->mtx);
  response->set_term(r->current_term);

  // receiver 1. impl
  if (request->term() < r->current_term) {
    response->set_success(false);
    return Status::OK;
  }

  // actual valid leader term, reset election timer and proceed.
  r->election_elapsed = 0;
  // receiver 2. impl
  {
    auto prev_log_idx = request->prev_log_idx();
    if (prev_log_idx > r->last_index() ||
        !r->match_term({request->prev_log_term(), prev_log_idx})) {
      logger->debug(
          "{} [term {}] rejected AppendEntries from {} [term {}] due to log "
          "inconsistency at index {}. expected term: {}. last_index={}",
          r->id, r->current_term, request->leader_id(), request->term(),
          prev_log_idx, request->prev_log_term(), r->last_index());
      response->set_conflict_term(0);
      response->set_conflict_idx(0);
      if (prev_log_idx <= r->last_index()) {
        auto conflict_term = r->get_term(prev_log_idx);
        response->set_conflict_term(conflict_term);
        for (uint64_t i = 0; i < r->logs.size(); i++) {
          if (r->logs[i].term() == conflict_term) {
            response->set_conflict_idx(i);
            break;
          }
        }
      }

      response->set_success(false);
      return Status::OK;
    }
  }

  logger->debug("{} [term {}] received a heartbeat from {} [term {}], starting "
               "to process entries",
               r->id, r->current_term, request->leader_id(), request->term());
  r->become_follower(request->term(), request->leader_id());
  response->set_success(true);
  std::vector<raftpb::Entry> entries(request->entries().begin(),
                                     request->entries().end());
  // assumption: the index field in entry is properly set

  logger->debug("{} [term {}] received {} entries from {} [term {}]", r->id,
               r->current_term, entries.size(), request->leader_id(),
               request->term());

  // receiver 3. and 4. impl
  if (!entries.empty()) {
    auto ci = r->find_conflict(entries);
    if (ci == 0 || ci <= r->get_committed()) {
      this->logger->critical(
          "entry {} conflict with committed entry [committed({})]", ci,
          r->get_committed());
      throw std::runtime_error("entry conflict with committed entry");
    }
    if (ci <= r->last_index()) {
      this->logger->debug("{} found conflicted entry starting at index {}",
                         r->id, ci);
      r->logs.erase(r->logs.begin() + ci, r->logs.end());
    }
    auto offset = request->prev_log_idx() + 1;
    auto start_idx = offset - ci;
    r->logs.insert(r->logs.end(), entries.begin() + start_idx, entries.end());
    logger->debug("{} [term {}] appended {} entries from {} [term {}]", r->id,
                 r->current_term, entries.size(), request->leader_id(),
                 request->term());
    // std::stringstream ss;
    // ss << "[ ";
    // auto count = 0;
    // for (const auto &entry : r->logs) {
    //   ss << count << ": " << entry.data() << ", ";
    //   count++;
    // }
    // ss << "]";
    // logger->debug("log entries: {}", ss.str());
  }

  // receiver 5. impl
  if (request->leader_commit() > r->get_committed()) {
    auto last_new_idx =
        request->prev_log_idx() + static_cast<uint64_t>(entries.size());
    logger->debug("{} [term {}] received {} entries from {} [term {}], "
                 "last_new_idx = {}, commit_idx = {}",
                 r->id, r->current_term, entries.size(), request->leader_id(),
                 request->term(), last_new_idx, r->get_committed());
    r->commit_to(std::min(request->leader_commit(), last_new_idx));
  }
  return Status::OK;
}

Status RaftService::RequestVote(grpc::ServerContext *context [[maybe_unused]],
                                const raftpb::RequestVoteRequest *request,
                                raftpb::RequestVoteReply *response) {
  auto r = this->raft_;
  this->logger->debug("{} received RequestVote RPC from {} for term {}", r->id,
                     request->candidate_id(), request->term());
  std::lock_guard<std::mutex> lock(r->mtx);
  response->set_term(r->current_term);

  // receiver 1. impl
  if (request->term() < r->current_term) {
    response->set_vote_granted(false);
    return Status::OK;
  }

  // if a higher term received, meaning the current node is behind, revert back
  // to follower
  if (request->term() > r->current_term) {
    r->become_follower(request->term(), std::nullopt);
  }

  // receiver 2. impl
  // Leader Completeness Check
  auto last_entry = r->get_last_entry_id();
  if (last_entry.term > request->last_log_term() ||
      (last_entry.term == request->last_log_term() &&
       last_entry.index > request->last_log_idx())) {
    response->set_vote_granted(false);
    this->logger->debug("{} rejected vote request from {} in term {} due to "
                       "failed completeness check",
                       r->id, request->candidate_id(), request->term());
    return Status::OK;
  }

  // check if voting is happened in the same term
  if (!r->voted_for.has_value() ||
      r->voted_for.value() == request->candidate_id()) {
    response->set_vote_granted(true);
    this->logger->debug("{} voted for {} in term {}", r->id,
                       request->candidate_id(), request->term());
  } else {
    response->set_vote_granted(false);
    this->logger->debug("{} already voted for {} in term {}", r->id,
                       r->voted_for.value(), r->current_term);
  }
  r->election_elapsed = 0;

  return grpc::Status::OK;
}

Status RaftService::SayHello(grpc::ServerContext *context [[maybe_unused]],
                             const raftpb::HelloRequest *request,
                             raftpb::HelloReply *response) {
  this->logger->debug("{} received hello from {}", raft_->id, request->name());
  this->raft_->receive_hello(request->name());
  response->set_message("Hello " + request->name());
  return grpc::Status::OK;
}

} // namespace rafty
