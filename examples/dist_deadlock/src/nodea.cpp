#include <iostream>
#include <memory>
#include <mutex>

#include "comm.pb.h"
#include "example/nodea.hpp"

using grpc::ServerBuilder;

namespace example {
NodeA::NodeA() {
  std::string server_address("localhost:50051");
  ServerBuilder builder;
  builder.AddListeningPort(server_address, grpc::InsecureServerCredentials());
  builder.RegisterService(this);
  std::unique_ptr<grpc::Server> server(builder.BuildAndStart());
  std::cout << "Server listening on " << server_address << std::endl;
  this->server_ = std::move(server);

  // Initialize the stub for NodeB
  stub_ = commpb::NodeBService::NewStub(grpc::CreateChannel(
      "localhost:50052", grpc::InsecureChannelCredentials()));
}

void NodeA::Wait() {
  if (server_) {
    server_->Wait();
  }
}

int NodeA::Invoke() {
  std::cout << "NodeA invoke method called." << std::endl;

  std::lock_guard guard(mtx); // Lock A

  commpb::NodeRequest request;
  commpb::NodeReply response;
  grpc::ClientContext context;

  grpc::Status status = stub_->ExecuteB(&context, request, &response);

  if (status.ok()) {
    std::cout << response.msg() << std::endl;
  } else {
    std::cerr << "ExecuteB call failed: " << status.error_message()
              << std::endl;
  }
  return 0;
}

grpc::Status NodeA::ExecuteA(grpc::ServerContext *context,
                             const commpb::NodeRequest *request,
                             commpb::NodeReply *response) {
  std::cout << "NodeA ExecuteA method called." << std::endl;
  std::lock_guard guard(mtx); // Lock A again
  response->set_msg("Hello from NodeA!");
  return grpc::Status::OK;
}

} // namespace example
