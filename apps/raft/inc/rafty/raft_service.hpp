#pragma once

#include "common/logger.hpp"

#include "raft.grpc.pb.h"

namespace rafty {
class Raft;

class RaftService final : public raftpb::RaftService::Service {
public:
  RaftService() = delete;
  RaftService(Raft *raft);
  ~RaftService() = default;

  grpc::Status AppendEntries(grpc::ServerContext *context,
                             const raftpb::AppendEntriesRequest *request,
                             raftpb::AppendEntriesReply *response) override;
  grpc::Status RequestVote(grpc::ServerContext *context,
                           const raftpb::RequestVoteRequest *request,
                           raftpb::RequestVoteReply *response) override;
  grpc::Status SayHello(grpc::ServerContext *context,
                        const raftpb::HelloRequest *request,
                        raftpb::HelloReply *response) override;

private:
  Raft *raft_;
  std::unique_ptr<utils::logger> logger;
};
} // namespace rafty