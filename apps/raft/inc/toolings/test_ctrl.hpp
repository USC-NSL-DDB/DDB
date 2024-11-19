#pragma once

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <csignal>
#include <cstdint>
#include <cstdlib>
#include <ctime>
#include <format>
#include <functional>
#include <future>
#include <google/protobuf/stubs/port.h>
#include <grpcpp/create_channel.h>
#include <grpcpp/grpcpp.h>
#include <grpcpp/health_check_service_interface.h>
#include <grpcpp/server.h>
#include <grpcpp/server_context.h>
#include <grpcpp/support/sync_stream.h>
#include <memory>
#include <mutex>
#include <optional>
#include <ostream>
#include <sys/types.h>
#include <thread>
#include <unistd.h>
#include <unordered_map>
#include <vector>

#include "common/config.hpp"

#include "absl/strings/str_split.h"

#include "spdlog/logger.h"
#include "tester.grpc.pb.h"
#include "tester.pb.h"

using grpc::Server;
using grpc::ServerBuilder;
using grpc::ServerContext;

namespace toolings {

class RaftTestCtrl final : public testerpb::TesterCommCtrlService::Service {
public:
  RaftTestCtrl() = delete;
  RaftTestCtrl(const std::vector<rafty::Config> &configs,
               const std::unordered_map<uint64_t, uint64_t> &node_tester_ports,
               const std::string &node_app_path, const std::string &ctrl_addr,
               uint8_t fail_type, uint8_t verbosity,
               std::shared_ptr<spdlog::logger> logger,
               const std::string &additional_flags = "")
      : logger(logger) {
    this->all_ready_ = false;
    // create ctrl server
    this->start_server(ctrl_addr);

    // spawn raft nodes
    this->spawn_raft_node(configs, node_tester_ports, node_app_path, ctrl_addr,
                          fail_type, verbosity, additional_flags);

    // connect to raft nodes
    this->connect_to_raft_nodes(node_tester_ports);
    this->server_apply_result_from_all_nodes();
  }

  ~RaftTestCtrl() {
    if (this->server_) {
      this->server_->Shutdown();
    }
    for (auto &[id, thread] : this->node_threads) {
      if (thread.joinable()) {
        thread.join();
      }
    }
    for (auto &thread : this->node_apply_threads) {
      if (thread.joinable()) {
        thread.join();
      }
    }
  }

  inline void start_server(const std::string &addr) {
    grpc::EnableDefaultHealthCheckService(false);

    ServerBuilder builder;
    builder.AddListeningPort(addr, grpc::InsecureServerCredentials());

    builder.RegisterService(this);

    std::unique_ptr<Server> server(builder.BuildAndStart());
    logger->info(std::format("Tester ctrl server listening on {}", addr));

    this->server_ = std::move(server);
    std::thread([this] { this->server_->Wait(); }).detach();
  }

  inline void
  register_applier_handler(std::function<void(testerpb::ApplyResult)> handler) {
    this->applier_func = handler;
  }

  inline void spawn_raft_node(
      const std::vector<rafty::Config> &configs,
      const std::unordered_map<uint64_t, uint64_t> &node_tester_ports,
      const std::string &node_app_path, const std::string &ctrl_addr,
      uint8_t fail_type, uint8_t verbosity, const std::string &additional_flags = ""
  ) {
    std::string command = std::format(
        "{} --enable_ctrl --ctrl_addr {} --fail_type {} --verbosity {}",
        node_app_path, ctrl_addr, fail_type, verbosity);
    // spawn node
    for (const auto &config : configs) {
      this->ready_nodes[config.id] = false;
      std::stringstream ss;
      for (const auto &peer : config.peer_addrs) {
        ss << peer.first << "+" << peer.second << ",";
      }
      std::string peer_addrs = ss.str().substr(0, ss.str().size() - 1);
      std::vector<std::string> splits = absl::StrSplit(config.addr, ":");
      if (splits.size() != 2) {
        std::cerr << "Invalid raft address: " << config.addr << std::endl;
        std::exit(1);
        return;
      }
      std::string cmd;
      if (peer_addrs.empty()) {
        cmd =
          std::format("{} --id {} --port {} --node_tester_port {} {} > "
                      "node_runner_{}.log 2>&1 &",
                      command, config.id, splits[1],
                      node_tester_ports.at(config.id), 
                      additional_flags, config.id);
      } else {
        cmd =
          std::format("{} --id {} --port {} --peers {} --node_tester_port {} {} > "
                      "node_runner_{}.log 2>&1 &",
                      command, config.id, splits[1], peer_addrs,
                      node_tester_ports.at(config.id), 
                      additional_flags, config.id);
      }
      logger->info("Spawning node: {}", cmd);
      this->node_threads[config.id] = std::thread([cmd] {
        int result = std::system(cmd.c_str());

        if (result == -1) {
          std::cerr << "Failed to spawn process" << std::endl;
          std::exit(1);
          return;
        }
      });
    }
  }

  inline void connect_to_raft_nodes(
      const std::unordered_map<uint64_t, uint64_t> &node_tester_ports) {
    // connect to raft nodes
    std::unique_lock<std::mutex> lock(mtx);
    logger->info("Waiting for all nodes to be ready...");
    cv_ready_.wait(lock, [this] { return this->is_all_ready(); });
    logger->info("All nodes are ready");

    grpc::ChannelArguments args;
    args.SetInt(GRPC_ARG_MAX_RECONNECT_BACKOFF_MS, 1000);
    args.SetInt(GRPC_ARG_MIN_RECONNECT_BACKOFF_MS, 100);
    args.SetInt(GRPC_ARG_INITIAL_RECONNECT_BACKOFF_MS, 100);

    for (const auto &node_port : node_tester_ports) {
      // logger->info("Connecting to node tester {} at {}", node_port.first,
      //                 node_port.second);

      // assumption: all nodes are running on localhost
      std::string addr = std::format("localhost:{}", node_port.second);
      auto channel = grpc::CreateCustomChannel(
          addr, grpc::InsecureChannelCredentials(), args);
      auto stub = testerpb::TesterCommNodeService::NewStub(std::move(channel));
      this->nodes[node_port.first] = std::move(stub);
    }
  }

  inline void server_apply_result_from_all_nodes() {
    for (const auto &[id, _] : this->nodes) {
      this->node_apply_threads.push_back(
        std::thread([this, id] { this->serve_apply_result(id); })
      );
    }
  }

  void serve_apply_result(uint64_t id) {
    google::protobuf::Empty request;
    grpc::ClientContext context;

    std::unique_ptr<grpc::ClientReader<testerpb::ApplyResult>> reader(
      this->nodes[id]->Apply(&context, request)
    );

    testerpb::ApplyResult result;
    while (reader->Read(&result)) {
      result.set_id(id);
      this->applier_func(result);
    }

    grpc::Status status = reader->Finish();
  }

  grpc::Status ReportReady(ServerContext *context,
                           const testerpb::NodeMeta *req,
                           testerpb::CmdReply *reply) override {
    (void)context;
    {
      std::lock_guard<std::mutex> lock(mtx);
      auto id = req->id();
      auto pid = req->pid();
      this->ready_nodes[id] = true;
      this->node_pids[id] = pid;
    }
    this->cv_ready_.notify_all();
    reply->set_success(true);
    return grpc::Status::OK;
  }

  // control path to nodes
  inline void run(const std::unordered_map<uint64_t, bool> connected = {}) {
    std::vector<std::future<bool>> futs;
    for (const auto &[id, stub] : nodes) {
      if (!connected.empty() && !connected.at(id)) {
        continue;
      }
      futs.push_back(std::async(std::launch::async, [this, id]() {
        google::protobuf::Empty request;
        testerpb::CmdReply reply;
        grpc::ClientContext context;
        auto status = this->nodes[id]->Run(&context, request, &reply);
        if (status.ok()) {
          return true;
        }
        return false;
      }));
    }
    for (auto &fut : futs) {
      if (!fut.get()) {
        std::cerr << "Failed to run node" << std::endl;
        std::exit(1);
      }
    }
  }

  inline void kill(const std::unordered_map<uint64_t, bool> connected = {}) {
    std::vector<std::future<bool>> futs;
    for (const auto &[id, stub] : nodes) {
      if (!connected.empty() && !connected.at(id)) {
        continue;
      }
      futs.push_back(std::async(std::launch::async, [this, id]() {
        google::protobuf::Empty request;
        testerpb::CmdReply reply;
        grpc::ClientContext context;
        auto status = this->nodes[id]->Kill(&context, request, &reply);
        if (status.ok()) {
          return true;
        }
        return false;
      }));
    }
    for (auto &fut : futs) {
      fut.wait_until(std::chrono::system_clock::now() +
                     std::chrono::seconds(2)); // wait for 2s at max.
    }
    std::this_thread::sleep_for(
        std::chrono::seconds(1)); // wait for 1s to ensure all nodes are dead.

    // Send SIGKILL to the process
    // assumption: all nodes are running locally
    for (const auto &[id, pid] : node_pids) {
      // if (::kill(pid, SIGTERM) == 0) {
      if (::kill(pid, SIGKILL) == 0) {
        logger->info("Process (node {}) {} has been killed.", id, pid);
      } else {
        std::perror("Failed to kill process");
        // std::exit(1);
      }
    }
  }

  inline void disconnect(const std::vector<uint64_t> &ids) {
    std::vector<std::future<bool>> futs;
    for (const auto &[id, stub] : nodes) {
      futs.push_back(std::async(std::launch::async, [this, id, &ids]() {
        testerpb::ConnOpt request;
        for (const auto &id : ids) {
          request.add_ids(id);
        }
        testerpb::CmdReply reply;
        grpc::ClientContext context;
        auto status = this->nodes[id]->Disconnect(&context, request, &reply);
        if (status.ok()) {
          return true;
        }
        return false;
      }));
    }
    for (auto &fut : futs) {
      if (!fut.get()) {
        std::cerr << "Failed to disconnect node" << std::endl;
        std::exit(1);
      }
    }
  }

  inline void reconnect(const std::vector<uint64_t> &ids) {
    std::vector<std::future<bool>> futs;
    for (const auto &[id, stub] : nodes) {
      futs.push_back(std::async(std::launch::async, [this, id, &ids]() {
        testerpb::ConnOpt request;
        for (const auto &id : ids) {
          request.add_ids(id);
        }
        testerpb::CmdReply reply;
        grpc::ClientContext context;
        auto status = this->nodes[id]->Reconnect(&context, request, &reply);
        if (status.ok()) {
          return true;
        }
        return false;
      }));
    }
    for (auto &fut : futs) {
      if (!fut.get()) {
        std::cerr << "Failed to reconnect node" << std::endl;
        std::exit(1);
      }
    }
  }

  inline std::vector<testerpb::State> get_all_states(const std::unordered_map<uint64_t, bool> connected = {}) {
    std::vector<std::future<testerpb::State>> futs;
    for (const auto &[id, stub] : nodes) {
      if (!connected.empty() && !connected.at(id)) {
        continue;
      }
      futs.push_back(std::async(std::launch::async, [this, id]() {
        google::protobuf::Empty request;
        testerpb::State response;
        grpc::ClientContext context;
        auto status = this->nodes[id]->GetState(&context, request, &response);
        if (status.ok()) {
          return response;
        }
        return response;
      }));
    }
    std::vector<testerpb::State> states;
    for (auto &fut : futs) {
      auto state = fut.get();
      states.push_back(state);
    }
    return states;
  }

  inline std::vector<testerpb::RPCStats> get_all_rpc_stats() {
    std::vector<std::future<testerpb::RPCStats>> futs;
    for (const auto &[id, stub] : nodes) {
      futs.push_back(std::async(std::launch::async, [this, id]() {
        google::protobuf::Empty request;
        testerpb::RPCStats response;
        grpc::ClientContext context;
        auto status =
            this->nodes[id]->GetRPCStats(&context, request, &response);
        if (status.ok()) {
          return response;
        }
        return response;
      }));
    }
    std::vector<testerpb::RPCStats> stats;
    for (auto &fut : futs) {
      auto stat = fut.get();
      stats.push_back(stat);
    }
    return stats;
  }

  inline testerpb::RPCStats get_total_rpc_stats() {
    testerpb::RPCStats total;
    uint64_t total_bytes = 0;
    uint64_t total_count = 0;
    auto stats = this->get_all_rpc_stats();
    for (const auto &stat : stats) {
        total_bytes += stat.bytes();
        total_count += stat.count();
    }
    total.set_bytes(total_bytes);
    total.set_count(total_count);
    return total;
  }

  inline std::vector<testerpb::ProposalResult>
  propose_to_all(const std::string &data, const std::unordered_map<uint64_t, bool> connected = {}) {
    std::vector<std::future<testerpb::ProposalResult>> futs;
    for (const auto &[id, stub] : nodes) {
      if (!connected.empty() && !connected.at(id)) {
        continue;
      }
      futs.push_back(std::async(std::launch::async, [this, id, &data]() {
        testerpb::ProposalReq request;
        request.set_data(data);
        testerpb::ProposalResult response;
        grpc::ClientContext context;
        auto status = this->nodes[id]->Propose(&context, request, &response);
        if (status.ok()) {
          return response;
        }
        return response;
      }));
    }
    std::vector<testerpb::ProposalResult> proposals;
    for (auto &fut : futs) {
      auto p = fut.get();
      proposals.push_back(p);
    }
    return proposals;
  }

  inline std::vector<testerpb::ProposalResult>
  propose_to_all_sync(const std::string &data, const std::unordered_map<uint64_t, bool> connected = {}) {
    std::vector<std::future<testerpb::ProposalResult>> futs;
    for (const auto &[id, stub] : nodes) {
      if (!connected.empty() && !connected.at(id)) {
        continue;
      }
      futs.push_back(std::async(std::launch::async, [this, id, &data]() {
        testerpb::ProposalReq request;
        request.set_data(data);
        testerpb::ProposalResult response;
        grpc::ClientContext context;
        auto status = this->nodes[id]->ProposeSync(&context, request, &response);
        if (status.ok()) {
          return response;
        }
        return response;
      }));
    }
    std::vector<testerpb::ProposalResult> proposals;
    for (auto &fut : futs) {
      auto p = fut.get();
      proposals.push_back(p);
    }
    return proposals;
  }

  inline std::optional<testerpb::ProposalResult>
  propose_to_one(uint64_t id, const std::string &data) {
    if (nodes.find(id) == nodes.end()) {
      return std::nullopt;
    }
    auto &stub = nodes.at(id);
    testerpb::ProposalReq request;
    request.set_data(data);
    testerpb::ProposalResult response;
    grpc::ClientContext context;
    auto status = stub->Propose(&context, request, &response);
    if (status.ok()) {
      return {response};
    }
    return std::nullopt;
  }

  inline std::optional<testerpb::ProposalResult>
  propose_to_one_sync(uint64_t id, const std::string &data) {
    if (nodes.find(id) == nodes.end()) {
      return std::nullopt;
    }
    auto &stub = nodes.at(id);
    testerpb::ProposalReq request;
    request.set_data(data);
    testerpb::ProposalResult response;
    grpc::ClientContext context;
    auto status = stub->ProposeSync(&context, request, &response);
    if (status.ok()) {
      return {response};
    }
    return std::nullopt;
  }

public:
  std::shared_ptr<spdlog::logger> logger;

private:
  inline bool is_all_ready() {
    for (const auto &[id, ready] : ready_nodes) {
      if (!ready) {
        return false;
      }
    }
    return true;
  }

private:
  mutable std::mutex mtx;
  std::condition_variable cv_ready_;
  std::atomic<bool> all_ready_ = false;
  std::unordered_map<uint64_t, bool> ready_nodes;

  std::unique_ptr<Server> server_;
  std::unordered_map<uint64_t, std::thread> node_threads;
  std::vector<std::thread> node_apply_threads;
  std::unordered_map<uint64_t, pid_t> node_pids;

  std::unordered_map<uint64_t,
                     std::unique_ptr<testerpb::TesterCommNodeService::Stub>>
      nodes;

  std::function<void(testerpb::ApplyResult)> applier_func;
};
} // namespace toolings