#include <iostream>
#include <memory>

#include "comm.grpc.pb.h"
#include "example/nodeb.hpp"

using grpc::ServerBuilder;

namespace example {
NodeB::NodeB() {
  std::string server_address("localhost:50052");
  ServerBuilder builder;
  builder.AddListeningPort(server_address, grpc::InsecureServerCredentials());
  builder.RegisterService(this);
  std::unique_ptr<grpc::Server> server(builder.BuildAndStart());
  std::cout << "Server listening on " << server_address << std::endl;
  this->server_ = std::move(server);
}

void NodeB::Wait() {
  if (server_) {
    server_->Wait();
  }
}

grpc::Status NodeB::ExecuteB(grpc::ServerContext *context,
                             const commpb::NodeRequest *request,
                             commpb::NodeReply *response) {
  std::cout << "NodeB ExecuteB method called." << std::endl;
  if (!stub_) {
    stub_ = commpb::NodeAService::NewStub(grpc::CreateChannel(
        "localhost:50051", grpc::InsecureChannelCredentials())
    );
  }

  commpb::NodeRequest node_a_request;
  commpb::NodeReply node_a_reply;
  grpc::ClientContext client_context;

  grpc::Status status =
      stub_->ExecuteA(&client_context, node_a_request, &node_a_reply);

  if (!status.ok()) {
    return grpc::Status(grpc::StatusCode::INTERNAL, "ExecuteA failed");
  }

  response->set_msg("NodeB ExecuteB method called.");
  return grpc::Status::OK;
}

} // namespace example
