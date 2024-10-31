#pragma once

#include <grpcpp/grpcpp.h>
#include <grpcpp/server_context.h>

#include "comm.grpc.pb.h"

namespace example {
using NodeAServiceStub = std::unique_ptr<commpb::NodeAService::Stub>;
using grpc::Server;
using grpc::ServerContext;

class NodeB final : public commpb::NodeBService::Service {
public:
  NodeB();
  void Wait();

  grpc::Status ExecuteB(grpc::ServerContext *context,
                        const commpb::NodeRequest *request,
                        commpb::NodeReply *response) override;

private:
  std::unique_ptr<Server> server_;
  NodeAServiceStub stub_;
};
} // namespace example