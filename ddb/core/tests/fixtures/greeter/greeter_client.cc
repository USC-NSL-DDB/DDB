/*
 * Copyright 2015 gRPC authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// Adapted from the pinned gRPC helloworld example for reproducible DDB tests.
#include <iostream>
#include <memory>
#include <string>

#include "absl/flags/parse.h"
#include "ddb_setup.hpp"
#include <grpcpp/grpcpp.h>
#include "helloworld.grpc.pb.h"

ABSL_FLAG(std::string, target, "127.0.0.1:50051", "Server address.");

using grpc::ClientContext;
using grpc::Status;
using helloworld::Greeter;
using helloworld::HelloReply;
using helloworld::HelloRequest;

class GreeterClient {
 public:
  explicit GreeterClient(std::shared_ptr<grpc::Channel> channel)
      : stub_(Greeter::NewStub(channel)) {}

  std::string SayHello(const std::string& user) {
    HelloRequest request;
    request.set_name(user);
    HelloReply reply;
    ClientContext context;
    Status status = stub_->SayHello(&context, request, &reply);
    return status.ok() ? reply.message() : "RPC failed";
  }

 private:
  std::unique_ptr<Greeter::Stub> stub_;
};

int main(int argc, char** argv) {
  absl::ParseCommandLine(argc, argv);
  auto connector = start_ddb();
  GreeterClient greeter(grpc::CreateChannel(absl::GetFlag(FLAGS_target),
                                          grpc::InsecureChannelCredentials()));
  std::cout << "Greeter received: " << greeter.SayHello("world") << std::endl;
}
