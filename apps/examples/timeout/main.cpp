#include <atomic>
#include <chrono>
#include <csignal>
#include <future>
#include <iostream>
#include <thread>

#include <ddb/integration.hpp>

using namespace std;

int long_running_function() {
  cout << "<<<<<<<<<<<<<<<<<<<<<<" << endl;
  cout << "Long running function started..." << endl;
  cout << "long_running_function() is running..." << endl;
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
    
    this_thread::sleep_for(chrono::seconds(1));
  }

  return 0;
}
