#include <cstdint>
#include <unordered_map>
#include <vector>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"
#include "spdlog/sinks/basic_file_sink.h"
#include "spdlog/spdlog.h"

#include "common/config.hpp"
#include "toolings/config_gen.hpp"
#include "toolings/test_ctrl.hpp"

constexpr std::string_view node_path = "./raft_node";
constexpr std::string_view ctrl_addr = "0.0.0.0:55000";

ABSL_FLAG(uint64_t, num, 3, "number of nodes to spawn (>= 3)");
ABSL_FLAG(std::string, bin, std::string(node_path), "the binary of node app");
ABSL_FLAG(int, verbosity, 1,
          "Verbosity level: 0 (silent), 1 (raft message (file sink only))");
ABSL_FLAG(int, fail_type, 0, "Failure Type: 0 (disonnection), 1 (partition)");

// for ddb support
ABSL_FLAG(bool, enable_ddb, false, "Enable DDB.");
ABSL_FLAG(std::string, ddb_addr, "127.0.0.1", "IP Address reported to DDB service.");

void command_loop(toolings::RaftTestCtrl &ctrl) {
  std::string command;
  std::cout << "Enter commands: "
               "\n\tr \tstart multinode raft cluster, "
               "\n\tdis <id1{, id2}> \tdisconnect a node from the cluster, "
               "\n\tconn <id1{, id2}> \treconnect a node into the cluster, "
               "\n\tprop <data> \tpropose data to the cluster, "
               "\n\tk \tkill the multinode raft cluster"
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
        ctrl.run();
        continue;
      } else if (cmd == "k") {
        ctrl.kill();
        break;
      } else if (cmd.empty()) {
        continue;
      } else {
        std::cout << "Unknown command: " << cmd << std::endl;
      }
    } else if (splits.size() >= 2) {
      auto cmd = splits[0];
      if (cmd == "dis" || cmd == "conn") {
        std::vector<uint64_t> ids;
        for (auto begin = splits.begin() + 1; begin != splits.end(); begin++) {
          ids.push_back(std::stoul(*begin));
        }

        if (cmd == "dis") {
          ctrl.disconnect(ids);
        } else {
          ctrl.reconnect(ids);
        }
        continue;
      }

      if (cmd == "prop") {
        std::string data = splits[1];
        std::cout << "proposing data = " << data << std::endl;
        ctrl.propose_to_all(data);
        continue;
      }
      
      std::cout << "Unknown command: " << command_ << std::endl;
    } else {
      std::cout << "Unknown command: " << command_ << std::endl;
    }
  }

  std::cout << "exiting..." << std::endl;
}

static pid_t pgid = 0;

void signal_handler(int signal) {
  if (signal == SIGINT) {
    std::cout << "Caught SIGINT (Ctrl+C), cleaning up..." << std::endl;
    if (::kill(-pgid, SIGKILL) == 0) {
      // well... dead...
    } else {
      std::perror("Failed to kill process");
      std::exit(1);
    }
    exit(0);
  }
}

int main(int argc, char **argv) {
  absl::ParseCommandLine(argc, argv);

  auto num = absl::GetFlag(FLAGS_num);
  auto binary_path = absl::GetFlag(FLAGS_bin);
  auto fail_type = absl::GetFlag(FLAGS_fail_type);
  auto verbosity = absl::GetFlag(FLAGS_verbosity);

  pgid = getpid();
  // Register the signal handler for Ctrl+C (SIGINT)
  struct sigaction sigIntHandler;
  sigIntHandler.sa_handler = signal_handler;
  sigemptyset(&sigIntHandler.sa_mask);
  sigIntHandler.sa_flags = 0;
  sigaction(SIGINT, &sigIntHandler, nullptr);

  std::vector<rafty::Config> configs;
  std::unordered_map<uint64_t, uint64_t> node_tester_ports;
  uint64_t tester_port = 55001;

  auto insts = toolings::ConfigGen::gen_local_instances(num, 50050);
  for (const auto &inst : insts) {
    std::map<uint64_t, std::string> peer_addrs;
    for (const auto &peer : insts) {
      if (peer.id == inst.id)
        continue;
      peer_addrs[peer.id] = peer.external_addr;
    }
    rafty::Config config = {
        .id = inst.id, .addr = inst.listening_addr, .peer_addrs = peer_addrs};
    configs.push_back(config);
    node_tester_ports[inst.id] = tester_port;
    tester_port++;
  }

  // logger setup
  auto logger_name = "multinode";
  auto logger = spdlog::get(logger_name);
  if (!logger) {
    // Create the logger if it doesn't exist
    logger = spdlog::basic_logger_mt(
        logger_name, std::format("logs/{}.log", logger_name), true);
  }
  spdlog::flush_every(std::chrono::seconds(3));


  std::string additional_flags = "";
  if (absl::GetFlag(FLAGS_enable_ddb)) {
    auto ip_addr = absl::GetFlag(FLAGS_ddb_addr);
    if (ip_addr.empty()) {
      std::cerr << "Error: --ddb_addr flag is required when ddb is enabled" << std::endl;
      return 1;
    }
    additional_flags = std::format("--enable_ddb --ddb_addr {}", ip_addr);
  }

  toolings::RaftTestCtrl ctrl(configs, node_tester_ports,
                              binary_path, std::string(ctrl_addr), fail_type,
                              verbosity, logger, additional_flags);

  ctrl.register_applier_handler({
    [logger](testerpb::ApplyResult m) -> void {
      auto i = m.id();
      logger->info(
        "ApplyResult: id={}, index={}, data={}",
        i, m.index(), m.data()
      );
    }
  });

  command_loop(ctrl);

  return 0;
}