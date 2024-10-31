#pragma once

#include <grpcpp/grpcpp.h>
#include <grpcpp/server_context.h>

#include "comm.grpc.pb.h"


namespace example {
using NodeBServiceStub = std::unique_ptr<commpb::NodeBService::Stub>;
using grpc::Server;
using grpc::ServerContext;

class NodeA final : public commpb::NodeAService::Service {
public:
  NodeA();
  ~NodeA() = default;

  void Wait();
  int Invoke();

  grpc::Status ExecuteA(grpc::ServerContext *context,
                        const commpb::NodeRequest*request,
                             commpb::NodeReply *response) override;

private:
  std::unique_ptr<Server> server_;
  NodeBServiceStub stub_;
  mutable std::mutex mtx;
};
}

