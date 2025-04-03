#include <atomic>
#include <chrono>
#include <csignal>
#include <cstdlib>
#include <iomanip>
#include <iostream>
#include <thread>

#include <ddb/integration.hpp>

using namespace std;

// Global variable for signal handling
std::atomic<bool> interrupted(false);

// Global signal handler function
void signal_handler(int signal) {
  interrupted.store(true);
  cout << "Interrupt signal received!" << endl;
  exit(0);
}

bool enable_ddb = false;
std::string ddb_addr;
bool enable_sleep = false;

void parse_arguments(int argc, char* argv[]) {
  for (int i = 1; i < argc; ++i) {
    std::string arg = argv[i];
    if (arg == "--enable_ddb") {
      enable_ddb = true;
    } else if (arg == "--ddb_addr" && i + 1 < argc) {
      ddb_addr = argv[++i];
    } else if (arg == "--enable_sleep") {
      enable_sleep = true;
    } else {
      std::cerr << "Unknown argument: " << arg << std::endl;
      std::cerr << "Usage: " << argv[0] << " [--enable_ddb] [--ddb_addr <address>] [--enable_sleep] [--delay <seconds>]" << std::endl;
      exit(1);
    }
  }
}

int main(int argc, char* argv[]) {
  parse_arguments(argc, argv);

  if (enable_ddb) {
    if (ddb_addr.empty()) {
      std::cerr << "Error: --ddb_addr flag is required when ddb is enabled" << std::endl;
      return 1;
    }
    auto ddb_config = DDB::Config::get_default(ddb_addr)
      .with_alias("raft_node");
    auto connector = DDB::DDBConnector(ddb_config);
    connector.init();
  }
  
  cout << "Enable DDB: " << (enable_ddb ? "true" : "false") << endl;
  if (enable_ddb) {
    cout << "DDB Address: " << ddb_addr << endl;
  }

  // chrono::seconds timeout_duration(2);
  std::signal(SIGINT, signal_handler);

  while (!interrupted) {
    // Get and print monotonic clock time with nanosecond precision
    auto now = std::chrono::steady_clock::now();
    auto now_ns = std::chrono::time_point_cast<std::chrono::nanoseconds>(now);
    auto duration = now_ns.time_since_epoch();
    auto ns = std::chrono::duration_cast<std::chrono::nanoseconds>(duration).count();
    
    // For human-readable format, we'll show seconds and nanoseconds
    auto sec = ns / 1000000000;
    auto ns_part = ns % 1000000000;
    
    std::cout << "Monotonic clock time: " << sec << "." << std::setfill('0') 
          << std::setw(9) << ns_part << " seconds since boot" << std::endl;
    
    // Also show system clock for reference
    auto system_now = std::chrono::system_clock::now();
    auto system_time_t = std::chrono::system_clock::to_time_t(system_now);
    std::tm system_tm = *std::localtime(&system_time_t);
    char timestamp[32];
    std::strftime(timestamp, sizeof(timestamp), "%Y-%m-%d %H:%M:%S", &system_tm);
    std::cout << "System clock time: " << timestamp << std::endl;

    // Sleep a bit before next iteration
    if (enable_sleep)
      std::this_thread::sleep_for(std::chrono::seconds(2));
  }

  return 0;
}
