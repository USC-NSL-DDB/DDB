#include <iostream>
#include <memory>
#include <string>

#include <ddb/integration.hpp>

#include "helloworld.grpc.pb.h"
#include <grpcpp/grpcpp.h>
#include <thread>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"

using grpc::Channel;
using grpc::ClientContext;
using grpc::Status;
using helloworld::Greeter;
using helloworld::HelloReply;
using helloworld::HelloRequest;

ABSL_FLAG(bool, enable_ddb, true, "Enable DDB.");
ABSL_FLAG(std::string, ddb_addr, "127.0.0.1",
          "IP Address reported to DDB service.");

class GreeterClient {
public:
  GreeterClient(std::shared_ptr<Channel> channel)
      : stub_(Greeter::NewStub(channel)) {}

  std::string SayHello(const std::string &user) {
    HelloRequest request;
    request.set_name(user);

    HelloReply reply;
    ClientContext context;

    Status status = stub_->SayHello(&context, request, &reply);

    if (status.ok()) {
      std::cout << "inside Greeter received: " << reply.message() << std::endl;
      while (true) {
        std::this_thread::sleep_for(std::chrono::seconds(60));
      }
      return reply.message();
    } else {
      std::cerr << "RPC failed: " << status.error_code() << ": "
                << status.error_message() << std::endl;
      return "RPC failed";
    }
  }

private:
  std::unique_ptr<Greeter::Stub> stub_;
};

int main(int argc, char **argv) {
  absl::ParseCommandLine(argc, argv);

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

  GreeterClient client(grpc::CreateChannel("localhost:60051",
                                           grpc::InsecureChannelCredentials()));

  std::string user("World");
  std::string reply = client.SayHello(user);
  std::cout << "Greeter received: " << reply << std::endl;

  return 0;
}
