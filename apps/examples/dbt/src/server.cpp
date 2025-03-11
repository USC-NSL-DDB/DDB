#include <iostream>
#include <memory>
#include <string>

#include <ddb/integration.hpp>

#include "helloworld.grpc.pb.h"
#include <grpcpp/grpcpp.h>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"

using grpc::Server;
using grpc::ServerBuilder;
using grpc::ServerContext;
using grpc::Status;
using helloworld::Greeter;
using helloworld::HelloReply;
using helloworld::HelloRequest;

ABSL_FLAG(bool, enable_ddb, true, "Enable DDB.");
ABSL_FLAG(std::string, ddb_addr, "127.0.0.1",
          "IP Address reported to DDB service.");

class GreeterServiceImpl final : public Greeter::Service {
  Status SayHello(ServerContext *context, const HelloRequest *request,
                  HelloReply *reply) override {
    std::string prefix("Hello ");
    reply->set_message(prefix + request->name());
    std::cout << "Received request for " << request->name() << std::endl;
    return Status::OK;
  }
};

std::unique_ptr<Server>
RunServer(GreeterServiceImpl& service) {
  std::string server_address("0.0.0.0:60051");

  ServerBuilder builder;
  builder.AddListeningPort(server_address, grpc::InsecureServerCredentials());
  builder.RegisterService(&service);

  std::unique_ptr<Server> server(builder.BuildAndStart());
  std::cout << "Server listening on " << server_address << std::endl;
  return server;
}

int main(int argc, char **argv) {
  absl::ParseCommandLine(argc, argv);
  GreeterServiceImpl service;
  auto svr = RunServer(service);

  if (absl::GetFlag(FLAGS_enable_ddb)) {
    auto ip_addr = absl::GetFlag(FLAGS_ddb_addr);
    if (ip_addr.empty()) {
      std::cerr << "Error: --ddb_addr flag is required when ddb is enabled"
                << std::endl;
      return 1;
    }
    auto ddb_config = DDB::Config::get_default(ip_addr).with_alias("server");
    auto connector = DDB::DDBConnector(ddb_config);
    connector.init();
  }

  svr->Wait();
  return 0;
}
