#include <cstdint>
#include <string>
#include <thread>
#include <vector>

#include <ddb/integration.hpp>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"
#include "absl/strings/str_join.h"
#include "absl/strings/str_split.h"

#include "spdlog/sinks/basic_file_sink.h"
#include "spdlog/sinks/stdout_sinks.h"
#include "spdlog/spdlog.h"

#include "common/config.hpp"
#include "common/utils/net_intercepter.hpp"
#include "toolings/msg_queue.hpp"

#include "toolings/raft_wrapper.hpp"

// #include "rafty/raft.hpp"

static void DisableConsoleLogging(spdlog::logger &logger,
                                  bool disable_file_sink = false) {
  // disable console logging
  // file sink won't be affected unless specified
  for (auto &sink : logger.sinks()) {
    if (auto console_sink =
            std::dynamic_pointer_cast<spdlog::sinks::stdout_sink_mt>(sink)) {
      console_sink->set_level(spdlog::level::off);
    } else if (auto console_sink = std::dynamic_pointer_cast<
                   spdlog::sinks::stdout_color_sink_mt>(sink)) {
      console_sink->set_level(spdlog::level::off);
    } else if (auto file_sink =
                   std::dynamic_pointer_cast<spdlog::sinks::basic_file_sink_mt>(
                       sink)) {
      if (disable_file_sink)
        file_sink->set_level(spdlog::level::off);
    }
  }
}

struct PeerAddrs {
  std::map<uint64_t, std::string> values;
};

// Parsing function for the custom flag type
bool AbslParseFlag(absl::string_view text, PeerAddrs *out, std::string *error) {
  if (text.empty()) {
    return true;
  }
  // Split the input text on commas (or any other delimiter you prefer)
  std::vector<std::string> parts = absl::StrSplit(text, ',');
  for (const auto &part : parts) {
    // Split each part on the first colon to get the id and address
    std::vector<std::string> id_addr = absl::StrSplit(part, '+');
    if (id_addr.size() != 2) {
      *error = "Invalid peer address: " + part;
      return false;
    }
    // Parse the id and address
    uint64_t id;
    if (!absl::SimpleAtoi(id_addr[0], &id)) {
      *error = "Invalid peer id: " + id_addr[0];
      return false;
    }
    // Assign the id and address to the out value
    out->values[id] = id_addr[1];
  }
  return true;
}

// Unparsing function for the custom flag type
std::string AbslUnparseFlag(const PeerAddrs &flag) {
  // Join the vector of strings into a single comma-separated string
  std::vector<std::string> parts;
  for (const auto &[id, addr] : flag.values) {
    parts.emplace_back(std::to_string(id) + "+" + addr);
  }
  if (parts.empty()) {
    return "";
  }
  return absl::StrJoin(parts, ",");
}

ABSL_FLAG(uint16_t, id, 0,
          "id for the current node (must be unique across the raft cluster)");
ABSL_FLAG(uint16_t, port, 50051, "Server port for the service");
ABSL_FLAG(PeerAddrs, peers, PeerAddrs(), "a list of peer addresses");
ABSL_FLAG(int, verbosity, 1,
          "Verbosity level: 0 (silent), 1 (raft message (file sink only)), 2 "
          "(all message, file sink + console sink)");
ABSL_FLAG(int, fail_type, 0, "Failure Type: 0 (disonnection), 1 (partition)");

// used for multiprocess tester
ABSL_FLAG(bool, enable_ctrl, false, "Enable a test controller");
ABSL_FLAG(std::string, ctrl_addr, "localhost:55000", "Address for the test controller");
ABSL_FLAG(uint64_t, node_tester_port, 55001, "Port for the node tester that will listen to.");

// enable non-interactive mode
ABSL_FLAG(bool, ni, false, "Enable non-interactive mode");

// for ddb support
ABSL_FLAG(bool, enable_ddb, false, "Enable DDB.");
ABSL_FLAG(std::string, ddb_addr, "", "IP Address reported to DDB service.");

void run_in_ni_mode(rafty::Raft &raft) {
  raft.connect_peers();
  raft.run();
  std::cout << "Running in non-interactive mode. Press Ctrl+C to exit." << std::endl;
  std::promise<void> exit_signal;
  std::future<void> future = exit_signal.get_future();
  future.wait();
}

void command_loop(rafty::Raft &raft) {
  std::string command;
  std::cout << "Enter commands: "
               "\n\tr \tconnect peers + run the raft instance, "
               "\n\tdis <id1{, id2}> \tdisconnect the node (specified by id) "
               "from the cluster, "
               "\n\tconn <id1{, id2}> \tconnect the node (specified by id) to "
               "the cluster, "
               "\n\tk \tkill the raft instance"
            << std::endl;

  while (true) {
    std::cout << "> ";
    std::getline(std::cin, command);

    auto command_ = absl::StripAsciiWhitespace(command);
    std::vector<std::string> splits = absl::StrSplit(command_, " ");
    if (splits.empty())
      continue;
    if (splits.size() == 1) {
      auto cmd = splits.front();
      if (cmd == "r") {
        raft.connect_peers();
        raft.run();
      } else if (cmd == "k") {
        raft.kill();
        raft.stop_server();
        break;
      } else if (cmd.empty()) {
        continue;
      } else {
        std::cout << "Unknown command: " << cmd << std::endl;
      }
    } else if (splits.size() >= 2) {
      auto cmd = splits[0];
      std::vector<std::string> params = {splits.begin() + 1, splits.end()};
      if (cmd == "dis") {
        for (const auto &param : params) {
          rafty::NetInterceptor::disconnect(param);
        }
      } else if (cmd == "conn") {
        for (const auto &param : params) {
          rafty::NetInterceptor::reconnect(param);
        }
      } else {
        std::cout << "Unknown command: " << cmd << " ";
        for (const auto &param : params) {
          std::cout << param << " ";
        }
        std::cout << std::endl;
      }
    } else {
      std::cout << "Unknown command: " << command_ << std::endl;
    }
  }
  std::cout << "exiting..." << std::endl;
}

void prepare_logging(uint64_t id) {
  int verbosity = absl::GetFlag(FLAGS_verbosity);
  auto logger_name = std::format("rafty_node_{}", static_cast<uint64_t>(id));
  if (verbosity < 2) {
    DisableConsoleLogging(*spdlog::get(logger_name), false);
  }
  if (verbosity < 1) {
    DisableConsoleLogging(*spdlog::get(logger_name), true);
  }
  spdlog::flush_every(std::chrono::seconds(3));
}

void prepare_interceptor(const std::set<std::string> &world) {
  // setup network interceptor
  rafty::NetInterceptor::setup_rank(world);
  int failure = absl::GetFlag(FLAGS_fail_type);
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
}

int main(int argc, char **argv) {
  absl::ParseCommandLine(argc, argv);

  if (absl::GetFlag(FLAGS_enable_ddb)) {
    auto ip_addr = absl::GetFlag(FLAGS_ddb_addr);
    if (ip_addr.empty()) {
      std::cerr << "Error: --ddb_addr flag is required when ddb is enabled" << std::endl;
      return 1;
    }
    auto ddb_config = DDB::Config::get_default(ip_addr)
      .with_alias("raft_node");
    auto connector = DDB::DDBConnector(ddb_config);
    connector.init();
  }

  auto id = absl::GetFlag(FLAGS_id);
  auto port = absl::GetFlag(FLAGS_port);

  std::map<uint64_t, std::string> peers;
  if (!absl::GetFlag(FLAGS_peers).values.empty()) {
    // std::cerr << "Error: --peers flag is required" << std::endl;
    // return 1;
    peers = absl::GetFlag(FLAGS_peers).values;
  }

  auto addr = "0.0.0.0:" + std::to_string(port);

  rafty::Config config = {.id = id, .addr = addr, .peer_addrs = peers};

  std::set<std::string> world;
  world.insert(std::to_string(id));

  std::cout << "Raft node (id=" << id << ") starting at " << addr
            << " with peers: " << std::endl;
  for (const auto &[peer_id, addr] : peers) {
    std::cout << "\t" << peer_id << " " << addr << std::endl;
    world.insert(std::to_string(peer_id));
  }

  auto enable_tester = absl::GetFlag(FLAGS_enable_ctrl);
  toolings::MessageQueue<rafty::ApplyResult> ready;

  toolings::RaftWrapper raft = [&]() {
    if (enable_tester) {
      auto ctrl_addr = absl::GetFlag(FLAGS_ctrl_addr);
      auto node_tester_port = absl::GetFlag(FLAGS_node_tester_port);
      return toolings::RaftWrapper(ctrl_addr, node_tester_port, config, ready);
    }
    return toolings::RaftWrapper(config, ready);
  }();

  // rafty::Raft raft(config, ready);
  // toolings::RaftTestWrapper raft(config, ready);

  prepare_interceptor(world);
  prepare_logging(id);

  raft.start_server();

  if (absl::GetFlag(FLAGS_ni)) {
    // run in non-ineractive mode
    // it will run the application directly
    // until the process is killed
    run_in_ni_mode(raft);
  } else {
    if (enable_tester) {
      // control via grpc tester controller
      raft.start_svr_loop();
    } else {
      // cli command loop
      command_loop(raft);
    }
  }

  std::cout << "wait 2 seconds for cleanup." << std::endl;
  std::this_thread::sleep_for(std::chrono::seconds(2)); // wait for clean up

  return 0;
}
