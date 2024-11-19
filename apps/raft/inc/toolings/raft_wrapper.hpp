#pragma once

#include <cstdint>
#include <grpcpp/client_context.h>
#include <grpcpp/completion_queue.h>
#include <grpcpp/server.h>
#include <grpcpp/server_builder.h>
#include <grpcpp/server_context.h>
#include <grpcpp/support/status.h>
#include <grpcpp/support/sync_stream.h>
#include <iostream>
#include <memory>
#include <string>
#include "common/common.hpp"
#include "common/utils/net_intercepter.hpp"
#include "rafty/raft.hpp"

#include "tester.grpc.pb.h"
#include "tester.pb.h"
#include "google/protobuf/empty.pb.h"
#include "toolings/msg_queue.hpp"

using google::protobuf::Empty;
using grpc::ServerBuilder;
using grpc::Server;

namespace toolings {
class RaftWrapper : public rafty::Raft, public testerpb::TesterCommNodeService::Service {
public:
  RaftWrapper() = delete;

  RaftWrapper(
    std::string ctrl_addr,
    uint64_t t_node_port,
    const rafty::Config &config,
    MessageQueue<rafty::ApplyResult> &ready
  ) : rafty::Raft(config, ready), ready(ready) { 
    this->with_ctrl = true;
    this->stop.store(false);
    this->connect_to_ctrl(ctrl_addr);
    this->run_tester_node_server(t_node_port);
    // notify the tester ctrl that this node is ready
    this->report_ready(config.id);
    this->id = config.id;
    // std::cout << "Raft Node " << config.id << " is ready" << std::endl;
  }

  RaftWrapper(
    const rafty::Config &config,
    MessageQueue<rafty::ApplyResult> &ready
  ) : rafty::Raft(config, ready), ready(ready) {
    with_ctrl = false;
  }

  ~RaftWrapper() {
    this->cleanup();
  }

  inline void report_ready(uint64_t id) {
    testerpb::NodeMeta meta;
    meta.set_id(id);
    meta.set_pid(getpid());
    testerpb::CmdReply reply;
    grpc::ClientContext context;
    // simply fire and forget...
    this->ctrl_svr->ReportReady(&context, meta, &reply);
  }

  grpc::Status Prepare(
    grpc::ServerContext *context,
    const testerpb::PrepareReq* request,
    testerpb::CmdReply *response
  ) override {
    (void) context;
    std::set<std::string> ids;
    for (const auto& id: request->world()) {
      ids.insert(std::to_string(id));
    }
    rafty::NetInterceptor::setup_rank(ids);

    auto failure = request->fail_type();
    if (failure == 0) {
      rafty::NetInterceptor::set_type(
          rafty::NetInterceptionType::NETWORK_FAILURE);
    } else if (failure == 1) {
      rafty::NetInterceptor::set_type(
          rafty::NetInterceptionType::NETWORK_PARTITION);
    } else {
      rafty::NetInterceptor::set_type(
          rafty::NetInterceptionType::NETWORK_FAILURE);
    }

    rafty::ByteCountingInterceptor::setup_rpc_monitor();
    response->set_success(true);
    return grpc::Status::OK;
  }

  grpc::Status Run(
    grpc::ServerContext *context,
    const Empty *request,
    testerpb::CmdReply *response
  ) override {
    (void) request;
    (void) context;
    this->connect_peers();
    this->run();
    response->set_success(true);
    return grpc::Status::OK;
  }

  grpc::Status Kill(
    grpc::ServerContext *context,
    const Empty *request,
    testerpb::CmdReply *response
  ) override {
    (void) context;
    (void) request;
    this->kill();
    this->stop_server();
    response->set_success(true);
    return grpc::Status::OK;
  }

  grpc::Status Disconnect(
    grpc::ServerContext *context,
    const testerpb::ConnOpt *request,
    testerpb::CmdReply *response
  ) override {
    (void) context;
    for (const auto &id : request->ids()) {
      std::cout << "Disconnect server " << id << std::endl;
      rafty::NetInterceptor::disconnect(std::to_string(id));
    }
    response->set_success(true);
    return grpc::Status::OK;
  }

  grpc::Status Reconnect(
    grpc::ServerContext *context,
    const testerpb::ConnOpt *request,
    testerpb::CmdReply *response
  ) override {
    (void) context;
    for (const auto &id : request->ids()) {
      std::cout << "Reconnect server " << id << std::endl;
      rafty::NetInterceptor::reconnect(std::to_string(id));
    }
    response->set_success(true);
    return grpc::Status::OK;
  }

  grpc::Status GetRPCStats(
    grpc::ServerContext *context,
    const Empty *request,
    testerpb::RPCStats *response
  ) override {
    (void) context;
    (void) request;
    auto bytes = rafty::ByteCountingInterceptor::get_total_bytes_sent();
    auto rpc_num= rafty::ByteCountingInterceptor::get_total_rpc_count();
    response->set_bytes(bytes);
    response->set_count(rpc_num);
    response->set_id(this->id);
    return grpc::Status::OK;
  }

  grpc::Status GetState(
    grpc::ServerContext *context,
    const Empty *request,
    testerpb::State *response
  ) override {
    (void) context;
    (void) request;
    auto state = this->get_state();
    response->set_term(state.term);
    response->set_is_leader(state.is_leader);
    response->set_id(this->id);
    // std::cout << "GetState: term=" << response->term() << ", is_leader=" << response->is_leader() << ", id=" << response->id() << std::endl;
    return grpc::Status::OK;
  }

  grpc::Status Propose(
    grpc::ServerContext *context,
    const testerpb::ProposalReq *request,
    testerpb::ProposalResult *response
  ) override {
    (void) context;
    auto result = this->propose(request->data());
    response->set_index(result.index);
    response->set_term(result.term);
    response->set_is_leader(result.is_leader);
    response->set_id(this->id);
    return grpc::Status::OK;
  }

  grpc::Status ProposeSync(
    grpc::ServerContext *context,
    const testerpb::ProposalReq *request,
    testerpb::ProposalResult *response
  ) override {
    (void) context;
    auto result = this->propose_sync(request->data());
    response->set_index(result.index);
    response->set_term(result.term);
    response->set_is_leader(result.is_leader);
    response->set_id(this->id);
    return grpc::Status::OK;
  }

  grpc::Status Apply(
    grpc::ServerContext *context,
    const google::protobuf::Empty *request,
    grpc::ServerWriter<testerpb::ApplyResult>* writer
  ) override {
    (void) context;
    rafty::ApplyResult result;
    this->applier(context, request, writer);
    return grpc::Status::OK;
  }

  inline void start_svr_loop() {
    this->node_svr->Wait();
  }

private:
  inline void run_tester_node_server(uint64_t t_node_port) {
    // std::string server_address("0.0.0.0:50051");
    grpc::EnableDefaultHealthCheckService(false);

    ServerBuilder builder;

    std::string t_node_addr = "localhost:" + std::to_string(t_node_port);
    builder.AddListeningPort(
      t_node_addr, 
      grpc::InsecureServerCredentials()
    );

    builder.RegisterService(this);
    std::unique_ptr<Server> server(builder.BuildAndStart());
    std::cout << "Raft Tester Node (for testing only) listening on " << t_node_addr << std::endl;

    this->node_svr = std::move(server);
    // std::thread([this] { this->node_svr->Wait(); }).detach();
  }

  inline void connect_to_ctrl(const std::string& ctrl_addr) {
    grpc::ChannelArguments args;
    // Set the maximum backoff time for reconnection attempts (e.g., 1 second)
    args.SetInt(GRPC_ARG_MAX_RECONNECT_BACKOFF_MS, 200); // 1 second max backoff
    // Set the minimum backoff time for reconnection attempts (e.g., 100ms)
    args.SetInt(GRPC_ARG_MIN_RECONNECT_BACKOFF_MS, 50); // 100ms min backoff
    // Set the initial backoff time for reconnection attempts (e.g., 100ms)
    args.SetInt(GRPC_ARG_INITIAL_RECONNECT_BACKOFF_MS,
                50); // 100ms initial backoff

    std::cout << "Connecting to ctrl at " << ctrl_addr << std::endl;

    auto channel = grpc::CreateCustomChannel(
      ctrl_addr, 
      grpc::InsecureChannelCredentials(), 
      args
    );
    auto stub = testerpb::TesterCommCtrlService::NewStub(std::move(channel));
    this->ctrl_svr = std::move(stub);
  }

  inline void cleanup() {
    if (this->node_svr)
      this->node_svr->Shutdown();
    if (this->ctrl_svr)
      this->ctrl_svr.reset();
    this->ready.close();
  }

  void applier(
    grpc::ServerContext *context, 
    const Empty *request, 
    grpc::ServerWriter<testerpb::ApplyResult> *writer
  ) {
    (void) context;
    (void) request;

    while (true) {
      if (this->stop.load())
        break;
      auto result = this->ready.dequeue();
      if (this->stop.load())
        break;
      if (result.valid) {
        grpc::ClientContext context;

        testerpb::ApplyResult r;
        r.set_valid(result.valid);
        r.set_data(result.data);
        r.set_index(result.index);
        r.set_id(this->id);

        writer->Write(r);
      } else {
        // ignore for now... invalid
      }
    }
  }

private:
  uint64_t id;
  // friend class RaftTestNodeService;  
  bool with_ctrl = false;
  
  // RaftTestNodeService service_;
  std::unique_ptr<testerpb::TesterCommCtrlService::Stub> ctrl_svr;
  std::unique_ptr<Server> node_svr;
  MessageQueue<rafty::ApplyResult> &ready;
  std::atomic<bool> stop;

  // private members for applier
  std::unique_ptr<grpc::ClientAsyncResponseReader<testerpb::CmdReply>> apply_resp_reader;
};

} // namespace toolings
