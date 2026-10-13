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

ABSL_FLAG(uint16_t, port, 50051, "Server port.");

using grpc::ServerContext;
using grpc::Status;
using helloworld::Greeter;
using helloworld::HelloReply;
using helloworld::HelloRequest;

class GreeterServiceImpl final : public Greeter::Service {
  Status SayHello(ServerContext*, const HelloRequest* request,
                  HelloReply* reply) override {
    std::string prefix("Hello ");
    reply->set_message(prefix + request->name());
    return Status::OK;
  }
};

int main(int argc, char** argv) {
  absl::ParseCommandLine(argc, argv);
  auto connector = start_ddb();
  GreeterServiceImpl service;
  grpc::ServerBuilder builder;
  auto address = "127.0.0.1:" + std::to_string(absl::GetFlag(FLAGS_port));
  builder.AddListeningPort(address, grpc::InsecureServerCredentials());
  builder.RegisterService(&service);
  auto server = builder.BuildAndStart();
  if (!server) return 1;
  std::cout << "Server listening on " << address << std::endl;
  server->Wait();
}
