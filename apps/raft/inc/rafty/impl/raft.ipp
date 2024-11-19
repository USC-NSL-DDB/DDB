#pragma once

#include <grpcpp/health_check_service_interface.h>

namespace rafty {
using grpc::ServerBuilder;
using grpc::ServerContext;
using grpc::experimental::ClientInterceptorFactoryInterface;
using grpc::experimental::CreateCustomChannelWithInterceptors;

inline void Raft::start_server() {
  grpc::EnableDefaultHealthCheckService(false);

  ServerBuilder builder;
  builder.AddListeningPort(this->listening_addr,
                           grpc::InsecureServerCredentials());

  // TODO: register the service
  builder.RegisterService(&this->service_);

  std::unique_ptr<Server> server(builder.BuildAndStart());
  logger->info("Raft server {} listening on {}", id, listening_addr);

  this->server_ = std::move(server);

  std::thread([this] { this->server_->Wait(); }).detach();
}

inline void Raft::stop_server() {
  if (this->server_) {
    this->server_->Shutdown();
  }
}

inline void Raft::connect_peers() {
  grpc::ChannelArguments args;
  // Set the maximum backoff time for reconnection attempts (e.g., 1 second)
  args.SetInt(GRPC_ARG_MAX_RECONNECT_BACKOFF_MS, 200); // 1 second max backoff
  // Set the minimum backoff time for reconnection attempts (e.g., 100ms)
  args.SetInt(GRPC_ARG_MIN_RECONNECT_BACKOFF_MS, 50); // 100ms min backoff
  // Set the initial backoff time for reconnection attempts (e.g., 100ms)
  args.SetInt(GRPC_ARG_INITIAL_RECONNECT_BACKOFF_MS,
              50); // 100ms initial backoff

  for (const auto &peer_addr : peer_addrs) {
    logger->info("Connecting to peer {} at {}", peer_addr.first,
                 peer_addr.second);
    std::vector<std::unique_ptr<ClientInterceptorFactoryInterface>>
        interceptor_creators;
    interceptor_creators.push_back(
        std::make_unique<ByteCountingInterceptorFactory>());
    interceptor_creators.push_back(std::make_unique<NetInterceptorFactory>());
    auto channel = CreateCustomChannelWithInterceptors(
        peer_addr.second, grpc::InsecureChannelCredentials(), args,
        std::move(interceptor_creators));
    auto stub = raftpb::RaftService::NewStub(std::move(channel));
    peers_[peer_addr.first] = std::move(stub);
  }
}

inline bool Raft::is_dead() const { return this->dead.load(); }

inline void Raft::kill() {
  this->dead.store(true);
  // add you code here if needed.
}

inline std::unique_ptr<grpc::ClientContext>
Raft::create_context(uint64_t to) const {
  std::unique_ptr<grpc::ClientContext> context =
      std::make_unique<grpc::ClientContext>();
  context->AddMetadata("from", std::to_string(this->id));
  context->AddMetadata("to", std::to_string(to));
  return context;
}

inline void Raft::apply(const ApplyResult &result) {
  this->ready_queue.enqueue(result);
}

} // namespace rafty
