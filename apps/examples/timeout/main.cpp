#include <atomic>
#include <chrono>
#include <csignal>
#include <cstdlib>
#include <future>
#include <iomanip>
#include <iostream>
#include <thread>

#include <ddb/integration.hpp>

using namespace std;

static int DELAY = 0;

int long_running_function() {
  cout << "<<<<<<<<<<<<<<<<<<<<<<" << endl;
  cout << "Long running function started..." << endl;
  cout << "long_running_function() is running..." << endl;
  auto now = std::chrono::system_clock::now();
  auto now_time_t = std::chrono::system_clock::to_time_t(now);
  std::cout << "Current time: " << std::put_time(std::localtime(&now_time_t), "%Y-%m-%d %H:%M:%S") << std::endl;
  if (DELAY > 0) {
    this_thread::sleep_for(chrono::seconds(DELAY)); // Simulate a long-running task
  }
  now = std::chrono::system_clock::now();
  now_time_t = std::chrono::system_clock::to_time_t(now);
  std::cout << "Current time: " << std::put_time(std::localtime(&now_time_t), "%Y-%m-%d %H:%M:%S") << std::endl;
  cout << "Long running function finished." << endl;
  cout << "<<<<<<<<<<<<<<<<<<<<<<" << endl;
  return 42;
}

void timeout_function() { cout << "Timeout function executed!" << endl; }

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

void parse_arguments(int argc, char* argv[]) {
  for (int i = 1; i < argc; ++i) {
    std::string arg = argv[i];
    if (arg == "--enable_ddb") {
      enable_ddb = true;
    } else if (arg == "--ddb_addr" && i + 1 < argc) {
      ddb_addr = argv[++i];
    } else if (arg == "--delay" && i + 1 < argc) {
      DELAY = std::stoi(argv[++i]);
    } else {
      std::cerr << "Unknown argument: " << arg << std::endl;
      std::cerr << "Usage: " << argv[0] << " [--enable_ddb] [--ddb_addr <address>] [--delay <seconds>]" << std::endl;
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
  
  cout << "Simulated Delay: " << DELAY << " seconds" << endl;
  cout << "Enable DDB: " << (enable_ddb ? "true" : "false") << endl;
  if (enable_ddb) {
    cout << "DDB Address: " << ddb_addr << endl;
  }
  cout << "Timeout duration: 2 seconds" << endl;

  chrono::seconds timeout_duration(2);
  std::signal(SIGINT, signal_handler);

  while (!interrupted) {
    // Launch the long running function in a separate thread
    future<int> result = async(launch::async, long_running_function);

    // Wait for the result with a timeout
    try {
      if (result.wait_for(timeout_duration) == future_status::timeout) {
        cout << "Long running function timed out!" << endl;
        timeout_function();
      } else {
        cout << "Long running function returned: " << result.get() << endl;
      }
    } catch (const exception &e) {
      cerr << "Exception caught: " << e.what() << endl;
    }
    
    cout << "Waiting for next iteration..." << endl;
    this_thread::sleep_for(chrono::seconds(1));
    cout << "Continuing..." << endl;
  }

  return 0;
}
