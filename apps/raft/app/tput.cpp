#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <sys/types.h>
#include <unordered_map>
#include <vector>
#include <numeric> // For std::accumulate
#include <algorithm>  // For std::sort

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"
#include "spdlog/sinks/basic_file_sink.h"
#include "spdlog/spdlog.h"

#include "common/config.hpp"
#include "toolings/config_gen.hpp"
#include "toolings/test_ctrl.hpp"

constexpr std::string_view node_path = "./node";
constexpr std::string_view ctrl_addr = "0.0.0.0:55000";

ABSL_FLAG(uint64_t, num, 3, "number of nodes to spawn (>= 3)");
ABSL_FLAG(std::string, bin, "./node", "the binary of node app");
ABSL_FLAG(int, verbosity, 1,
          "Verbosity level: 0 (silent), 1 (raft message (file sink only))");
ABSL_FLAG(int, fail_type, 0, "Failure Type: 0 (disonnection), 1 (partition)");

constexpr size_t NUM_REQS = 1000;
constexpr size_t COL_WIDTH= 15;
std::vector<std::vector<std::string>> results;

void print_results() {
  // Print header
  for (const auto& header : results[0]) {
      std::cout << std::left << std::setw(COL_WIDTH) << header;
  }
  std::cout << std::endl;

  // Print horizontal line
  std::cout << std::string(COL_WIDTH * results[0].size(), '-') << std::endl;

  // // Print rows
  // for (size_t i = 1; i < results.size(); ++i) {
  //     for (const auto& cell : results[i]) {
  //         std::cout << std::left << std::setw(COL_WIDTH) << cell;
  //     }
  //     std::cout << std::endl;
  // }
}

void print_row(std::vector<std::string> row) {
  for (const auto& cell : row) {
    std::cout << std::left << std::setw(COL_WIDTH) << cell;
  }
  std::cout << std::endl;
}

template <size_t N>
double compute_avg(const std::array<uint64_t, N>& values) {
    double sum = std::accumulate(values.begin(), values.end(), 0.0);
    return sum / N;  // N is the compile-time size of the array
}

template <bool SORTED = false, size_t N>
double compute_p50(std::array<uint64_t, N>& values) {
    if constexpr (!SORTED) {
        std::sort(values.begin(), values.end());
    } 
    size_t mid = N / 2;
    if (N % 2 == 0) {
        return (values[mid - 1] + values[mid]) / 2.0;
    } else {
        return values[mid];
    }
}

template <bool SORTED = false, size_t N>
double compute_p90(std::array<uint64_t, N>& values) {
    if constexpr (!SORTED) {
        std::sort(values.begin(), values.end());
    } 
    size_t index = static_cast<size_t>(0.90 * N);
    return values[index];
}

template <bool SORTED = false, size_t N>
double compute_p99(std::array<uint64_t, N>& values) {
    if constexpr (!SORTED) {
        std::sort(values.begin(), values.end());
    } 
    size_t index = static_cast<size_t>(0.99 * N);
    return values[index];
}

void run_raft_servers(toolings::RaftTestCtrl &ctrl) {
  ctrl.register_applier_handler({[](testerpb::ApplyResult _ [[maybe_unused]]) -> void { /* Do nothing */ }});
  ctrl.run(); 
  std::this_thread::sleep_for(std::chrono::seconds(3));
}

void cleanup_raft_servers(toolings::RaftTestCtrl &ctrl) {
  ctrl.kill();
}

template <size_t N_CLIENTS>
void start_one_round(
  const std::vector<rafty::Config> &configs,
  const std::unordered_map<uint64_t, uint64_t> &node_tester_ports,
  uint8_t fail_type,
  uint8_t verbosity,
  std::shared_ptr<spdlog::logger> logger
) {
  toolings::RaftTestCtrl ctrl(
    configs, node_tester_ports,
    std::string(node_path), std::string(ctrl_addr), fail_type,
    verbosity, logger
  );
  run_raft_servers(ctrl);

  std::array<std::array<uint64_t, NUM_REQS>, N_CLIENTS> latencies;
  std::vector<std::thread> threads;
  auto tput_start = std::chrono::high_resolution_clock::now();
  for (size_t i = 0; i < N_CLIENTS; i++) {
    threads.push_back(std::thread([&latencies, &ctrl, i]() {
      std::string data = "hello, world!";
      for (uint64_t j = 0; j < NUM_REQS; j++) {
        auto start = std::chrono::high_resolution_clock::now();
        auto r = ctrl.propose_to_all_sync(data);
        auto end = std::chrono::high_resolution_clock::now();
        latencies[i][j] = std::chrono::duration_cast<std::chrono::microseconds>(end - start).count();
      }
    }));
  }

  for (auto &t : threads) {
    t.join();
  }

  auto tput_end = std::chrono::high_resolution_clock::now();
  auto duration = std::chrono::duration_cast<std::chrono::milliseconds>(tput_end - tput_start).count();
  const size_t total_ops = N_CLIENTS * NUM_REQS;
  double throughput = (double)total_ops / (duration / 1000.0); // ops per second

  std::array<uint64_t, total_ops> all_latencies;
  for (size_t i = 0; i < N_CLIENTS; i++) {
    for (size_t j = 0; j < NUM_REQS; j++) {
      all_latencies[i * NUM_REQS + j] = latencies[i][j];
    }
  }

  auto avg = compute_avg(all_latencies) / 1000;
  auto p50 = compute_p50(all_latencies) / 1000;
  auto p90 = compute_p90<false>(all_latencies) / 1000;
  auto p99 = compute_p99<false>(all_latencies) / 1000;

  // std::cout << N_CLIENTS << " \t" << avg << " \t" << p50 << " \t" << p99 << " \t" << throughput << std::endl;
  print_row({
    std::to_string(N_CLIENTS), 
    std::format("{:.3f}", avg), 
    std::format("{:.3f}", p50), 
    std::format("{:.3f}", p90), 
    std::format("{:.3f}", p99), 
    std::format("{:.3f}", throughput)
  });

  cleanup_raft_servers(ctrl);
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

  results.push_back({"numClient", "latAvg (ms)", "latP50 (ms)", "latP90 (ms)", "latP99 (ms)", "tput (ops/sec)"});

  print_results();

  start_one_round<1>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<2>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<4>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<8>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<16>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<32>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<64>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<128>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<256>(configs, node_tester_ports, fail_type, verbosity, logger);
  start_one_round<512>(configs, node_tester_ports, fail_type, verbosity, logger);
  // start_one_round<300>(configs, node_tester_ports, fail_type, verbosity, logger);
  // start_one_round<350>(configs, node_tester_ports, fail_type, verbosity, logger);
  // start_one_round<400>(configs, node_tester_ports, fail_type, verbosity, logger);
  // start_one_round<450>(configs, node_tester_ports, fail_type, verbosity, logger);
  return 0;
}